use crate::config::Config;
use crate::db::{self, Database};
use crate::{compress, hooks};
use anyhow::Result;
use axum::extract::State as AxumState;
use axum::http::{HeaderMap, StatusCode as AxumStatusCode};
use axum::response::IntoResponse;
use axum::routing::{any, get};
use rmcp::model::*;
use rmcp::{ServerHandler, ServiceExt};
use std::net::SocketAddr;
use std::sync::Arc;

type JsonObject = serde_json::Map<String, serde_json::Value>;

fn schema(val: serde_json::Value) -> Arc<JsonObject> {
    Arc::new(val.as_object().expect("schema must be an object").clone())
}

#[derive(Clone)]
pub struct IronMemServer {
    db: Arc<Database>,
    config: Arc<Config>,
}

impl IronMemServer {
    fn build_tool_list() -> Vec<Tool> {
        vec![
            Tool::new(
                "session_start",
                "Start a new session for a project. Returns a session_id.",
                schema(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "project": { "type": "string", "description": "Project root path" }
                    },
                    "required": ["project"]
                })),
            ),
            Tool::new(
                "session_end",
                "End a session and trigger compression. Returns memory_id if compression succeeds.",
                schema(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string", "description": "Session ID to end" }
                    },
                    "required": ["session_id"]
                })),
            ),
            Tool::new(
                "record_event",
                "Record a tool call observation in the current session.",
                schema(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string", "description": "Session ID" },
                        "project": { "type": "string", "description": "Project root path" },
                        "tool": { "type": "string", "description": "Tool name" },
                        "input": { "type": "string", "description": "Tool input (optional)" },
                        "output": { "type": "string", "description": "Tool output (optional)" }
                    },
                    "required": ["session_id", "project", "tool"]
                })),
            ),
            Tool::new(
                "compress_session",
                "Manually compress a session into a memory.",
                schema(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string", "description": "Session ID to compress" }
                    },
                    "required": ["session_id"]
                })),
            ),
            Tool::new(
                "get_context",
                "Retrieve memories for a project. Optionally search with a query.",
                schema(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "project": { "type": "string", "description": "Project root path" },
                        "limit": { "type": "integer", "description": "Max results (default 5)" },
                        "query": { "type": "string", "description": "Search query (optional)" }
                    },
                    "required": ["project"]
                })),
            ),
            Tool::new(
                "get_status",
                "Get database stats: total sessions, memories, and observations.",
                schema(serde_json::json!({
                    "type": "object",
                    "properties": {}
                })),
            ),
            Tool::new(
                "list_memories",
                "List recent memories for a project.",
                schema(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "project": { "type": "string", "description": "Project root path" },
                        "limit": { "type": "integer", "description": "Max results (default 5)" }
                    },
                    "required": ["project"]
                })),
            ),
            Tool::new(
                "search_memories",
                "Full-text search across session memories.",
                schema(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Search query" },
                        "project": { "type": "string", "description": "Project root path" },
                        "limit": { "type": "integer", "description": "Max results (default 10)" }
                    },
                    "required": ["query", "project"]
                })),
            ),
            Tool::new(
                "inject_context",
                "Write IRONMEM.md to a project root with recent session memories.",
                schema(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "project": { "type": "string", "description": "Project root path" },
                        "limit": { "type": "integer", "description": "Max memories to inject (default 5)" }
                    },
                    "required": ["project"]
                })),
            ),
            Tool::new(
                "wipe_project",
                "Delete all memories for a project.",
                schema(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "project": { "type": "string", "description": "Project root path" }
                    },
                    "required": ["project"]
                })),
            ),
        ]
    }

    async fn handle_session_start(&self, args: &JsonObject) -> Result<CallToolResult, ErrorData> {
        let project = args
            .get("project")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ErrorData::invalid_params("missing 'project'", None))?;

        let session_id = db::create_session(&self.db, project)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let json = serde_json::json!({ "session_id": session_id });
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&json).unwrap(),
        )]))
    }

    async fn handle_session_end(&self, args: &JsonObject) -> Result<CallToolResult, ErrorData> {
        let session_id = args
            .get("session_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ErrorData::invalid_params("missing 'session_id'", None))?;

        db::end_session(&self.db, session_id)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let count = db::observation_count_for_session(&self.db, session_id)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        if count == 0 {
            let json = serde_json::json!({
                "ok": true,
                "memory_id": null,
                "skipped": true,
                "reason": "No tool calls recorded"
            });
            return Ok(CallToolResult::success(vec![Content::text(
                serde_json::to_string_pretty(&json).unwrap(),
            )]));
        }

        match self.run_compression(session_id).await {
            Ok(memory_id) => {
                let json = serde_json::json!({
                    "ok": true,
                    "memory_id": memory_id,
                    "skipped": false
                });
                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string_pretty(&json).unwrap(),
                )]))
            }
            Err(e) => {
                let json = serde_json::json!({
                    "ok": true,
                    "memory_id": null,
                    "skipped": true,
                    "reason": format!("Compression failed: {}", e)
                });
                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string_pretty(&json).unwrap(),
                )]))
            }
        }
    }

    async fn handle_record_event(&self, args: &JsonObject) -> Result<CallToolResult, ErrorData> {
        let session_id = args
            .get("session_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ErrorData::invalid_params("missing 'session_id'", None))?;
        let project = args
            .get("project")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ErrorData::invalid_params("missing 'project'", None))?;
        let tool = args
            .get("tool")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ErrorData::invalid_params("missing 'tool'", None))?;
        let input = args.get("input").and_then(|v| v.as_str());
        let output = args.get("output").and_then(|v| v.as_str());

        let id = db::insert_observation(
            &self.db,
            session_id,
            project,
            tool,
            input,
            output,
            self.config.max_observation_bytes,
        )
        .await
        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let json = serde_json::json!({ "id": id });
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&json).unwrap(),
        )]))
    }

    async fn handle_compress_session(
        &self,
        args: &JsonObject,
    ) -> Result<CallToolResult, ErrorData> {
        let session_id = args
            .get("session_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ErrorData::invalid_params("missing 'session_id'", None))?;

        let memory_id = self
            .run_compression(session_id)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let json = serde_json::json!({ "memory_id": memory_id });
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&json).unwrap(),
        )]))
    }

    async fn handle_get_context(&self, args: &JsonObject) -> Result<CallToolResult, ErrorData> {
        let project = args
            .get("project")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ErrorData::invalid_params("missing 'project'", None))?;
        let limit = args
            .get("limit")
            .and_then(|v| v.as_i64())
            .unwrap_or(self.config.inject_limit as i64);
        let query = args.get("query").and_then(|v| v.as_str());

        let memories = if let Some(q) = query {
            if !q.is_empty() {
                db::search_memories(&self.db, project, q, limit)
                    .await
                    .unwrap_or_default()
            } else {
                db::get_recent_memories(&self.db, project, limit)
                    .await
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?
            }
        } else {
            db::get_recent_memories(&self.db, project, limit)
                .await
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?
        };

        let json = serde_json::json!({ "memories": memories });
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&json).unwrap(),
        )]))
    }

    async fn handle_get_status(&self) -> Result<CallToolResult, ErrorData> {
        let stats = db::get_stats(&self.db)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let json = serde_json::json!({
            "ok": true,
            "sessions": stats.total_sessions,
            "memories": stats.total_memories,
            "observations": stats.total_observations,
            "db_path": self.config.db_path,
        });
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&json).unwrap(),
        )]))
    }

    async fn handle_list_memories(&self, args: &JsonObject) -> Result<CallToolResult, ErrorData> {
        let project = args
            .get("project")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ErrorData::invalid_params("missing 'project'", None))?;
        let limit = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(5);

        let memories = db::get_recent_memories(&self.db, project, limit)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let json = serde_json::json!({ "memories": memories });
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&json).unwrap(),
        )]))
    }

    async fn handle_search_memories(&self, args: &JsonObject) -> Result<CallToolResult, ErrorData> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ErrorData::invalid_params("missing 'query'", None))?;
        let project = args
            .get("project")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ErrorData::invalid_params("missing 'project'", None))?;
        let limit = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(10);

        let memories = db::search_memories(&self.db, project, query, limit)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let json = serde_json::json!({ "memories": memories });
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&json).unwrap(),
        )]))
    }

    async fn handle_inject_context(&self, args: &JsonObject) -> Result<CallToolResult, ErrorData> {
        let project = args
            .get("project")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ErrorData::invalid_params("missing 'project'", None))?;
        let limit = args
            .get("limit")
            .and_then(|v| v.as_i64())
            .unwrap_or(self.config.inject_limit as i64);

        let memories = db::get_recent_memories(&self.db, project, limit)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        hooks::write_ironmem_file(project, &memories)
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        hooks::ensure_claude_md_import(project)
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let json = serde_json::json!({
            "injected": memories.len(),
            "project": project,
        });
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&json).unwrap(),
        )]))
    }

    async fn handle_wipe_project(&self, args: &JsonObject) -> Result<CallToolResult, ErrorData> {
        let project = args
            .get("project")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ErrorData::invalid_params("missing 'project'", None))?;

        let count = db::delete_memories_for_project(&self.db, project)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let _ = std::fs::remove_file(std::path::Path::new(project).join("IRONMEM.md"));
        let _ = hooks::remove_claude_md_import(project);

        let json = serde_json::json!({
            "wiped": count,
            "project": project,
        });
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&json).unwrap(),
        )]))
    }

    async fn run_compression(&self, session_id: &str) -> anyhow::Result<i64> {
        let api_key = std::env::var("ANTHROPIC_API_KEY")
            .or_else(|_| {
                let key_path = crate::config::ironmem_dir().join("api_key");
                std::fs::read_to_string(&key_path)
                    .map(|k| k.trim().to_string())
                    .map_err(|_| std::env::VarError::NotPresent)
            })
            .map_err(|_| {
                anyhow::anyhow!("ANTHROPIC_API_KEY not set and ~/.ironmem/api_key not found")
            })?;

        let session = db::get_session(&self.db, session_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Session not found: {}", session_id))?;

        let observations = db::get_observations_for_session(&self.db, session_id).await?;

        let result =
            compress::compress_session(&observations, &self.config.model, &api_key).await?;

        let memory_id = db::insert_memory(
            &self.db,
            &session.project,
            session_id,
            &result.summary,
            Some(&result.tags),
        )
        .await?;

        db::mark_compressed(&self.db, session_id).await?;

        tracing::info!(
            "Session {} compressed → memory_id={}",
            session_id,
            memory_id
        );

        Ok(memory_id)
    }
}

impl ServerHandler for IronMemServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::V_2024_11_05,
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: Implementation {
                name: "IronMem".to_string(),
                version: "0.1.0".to_string(),
            },
            ..Default::default()
        }
    }

    async fn list_tools(
        &self,
        _request: PaginatedRequestParam,
        _context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult {
            tools: Self::build_tool_list(),
            next_cursor: None,
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParam,
        _context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let args = request.arguments.unwrap_or_default();
        match request.name.as_ref() {
            "session_start" => self.handle_session_start(&args).await,
            "session_end" => self.handle_session_end(&args).await,
            "record_event" => self.handle_record_event(&args).await,
            "compress_session" => self.handle_compress_session(&args).await,
            "get_context" => self.handle_get_context(&args).await,
            "get_status" => self.handle_get_status().await,
            "list_memories" => self.handle_list_memories(&args).await,
            "search_memories" => self.handle_search_memories(&args).await,
            "inject_context" => self.handle_inject_context(&args).await,
            "wipe_project" => self.handle_wipe_project(&args).await,
            _ => Err(ErrorData::invalid_params(
                format!("unknown tool: {}", request.name),
                None,
            )),
        }
    }
}

pub async fn run_stdio(db: Arc<Database>, config: Config) -> Result<()> {
    let server = IronMemServer {
        db,
        config: Arc::new(config),
    };

    let service = server.serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
}

pub async fn run_sse_with_auth(
    db: Arc<Database>,
    config: Config,
    bind: SocketAddr,
    auth_token: Option<String>,
) -> Result<()> {
    let server = IronMemServer {
        db,
        config: Arc::new(config),
    };

    match auth_token {
        Some(token) => {
            // Auth mode: rmcp on internal loopback, auth proxy on public port
            let internal_port = bind.port() + 100;
            let internal_addr: SocketAddr =
                format!("127.0.0.1:{}", internal_port).parse().unwrap();

            let ct = rmcp::transport::sse_server::SseServer::serve(internal_addr)
                .await?
                .with_service(move || server.clone());

            let client = reqwest::Client::new();
            let proxy_state = AuthProxyState {
                token,
                upstream: format!("http://127.0.0.1:{}", internal_port),
                client,
            };

            let app = axum::Router::new()
                .route("/sse", get(proxy_sse))
                .route("/message", any(proxy_message))
                .with_state(proxy_state);

            let listener = tokio::net::TcpListener::bind(bind).await?;
            tracing::info!(
                "IronMem MCP SSE server (auth-protected) listening on {}",
                bind
            );
            tracing::info!("SSE endpoint: http://{}/sse", bind);

            tokio::select! {
                result = axum::serve(listener, app) => {
                    if let Err(e) = result {
                        tracing::error!("Auth proxy error: {}", e);
                    }
                }
                _ = tokio::signal::ctrl_c() => {
                    tracing::info!("Shutting down...");
                }
            }

            ct.cancel();
        }
        None => {
            // No auth: rmcp directly on the requested address
            let ct = rmcp::transport::sse_server::SseServer::serve(bind)
                .await?
                .with_service(move || server.clone());

            tracing::info!("IronMem MCP SSE server listening on {}", bind);
            tracing::info!("SSE endpoint: http://{}/sse", bind);

            tokio::signal::ctrl_c().await?;
            ct.cancel();
        }
    }

    tracing::info!("SSE transport shutdown complete.");
    Ok(())
}

// --- Auth proxy for SSE endpoint ---

#[derive(Clone)]
struct AuthProxyState {
    token: String,
    upstream: String,
    client: reqwest::Client,
}

fn check_bearer_token(headers: &HeaderMap, expected: &str) -> Result<(), AxumStatusCode> {
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .ok_or(AxumStatusCode::UNAUTHORIZED)?;

    if let Some(token) = auth.strip_prefix("Bearer ") {
        if token == expected {
            return Ok(());
        }
    }
    Err(AxumStatusCode::UNAUTHORIZED)
}

async fn proxy_sse(
    AxumState(state): AxumState<AuthProxyState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AxumStatusCode> {
    check_bearer_token(&headers, &state.token)?;

    let upstream_url = format!("{}/sse", state.upstream);
    let resp = state
        .client
        .get(&upstream_url)
        .send()
        .await
        .map_err(|_| AxumStatusCode::BAD_GATEWAY)?;

    let status = axum::http::StatusCode::from_u16(resp.status().as_u16())
        .unwrap_or(AxumStatusCode::BAD_GATEWAY);

    let mut response_headers = axum::http::HeaderMap::new();
    for (key, value) in resp.headers() {
        if let Ok(name) = axum::http::HeaderName::from_bytes(key.as_ref()) {
            if let Ok(val) = axum::http::HeaderValue::from_bytes(value.as_ref()) {
                response_headers.insert(name, val);
            }
        }
    }

    let stream = resp.bytes_stream();
    let body = axum::body::Body::from_stream(stream);

    Ok((status, response_headers, body))
}

async fn proxy_message(
    AxumState(state): AxumState<AuthProxyState>,
    headers: HeaderMap,
    query: axum::extract::RawQuery,
    body: axum::body::Bytes,
) -> Result<impl IntoResponse, AxumStatusCode> {
    check_bearer_token(&headers, &state.token)?;

    let mut upstream_url = format!("{}/message", state.upstream);
    if let Some(q) = query.0 {
        upstream_url = format!("{}?{}", upstream_url, q);
    }

    let content_type = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json")
        .to_string();

    let resp = state
        .client
        .post(&upstream_url)
        .header("content-type", content_type)
        .body(body)
        .send()
        .await
        .map_err(|_| AxumStatusCode::BAD_GATEWAY)?;

    let status = axum::http::StatusCode::from_u16(resp.status().as_u16())
        .unwrap_or(AxumStatusCode::BAD_GATEWAY);
    let resp_body = resp
        .bytes()
        .await
        .map_err(|_| AxumStatusCode::BAD_GATEWAY)?;

    Ok((status, resp_body))
}
