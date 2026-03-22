use crate::config::Config;
use crate::db::{self, Database};
use crate::{hooks, provider};
use anyhow::Result;
use rmcp::model::*;
use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};
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
        let session = db::get_session(&self.db, session_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Session not found: {}", session_id))?;

        let observations = db::get_observations_for_session(&self.db, session_id).await?;

        let result = provider::compress(&observations, &self.config).await?;

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
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("IronMem", "0.2.0"))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(Self::build_tool_list()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
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

pub async fn run_streamable_http(db: Arc<Database>, config: Config, bind: SocketAddr) -> Result<()> {
    let server = IronMemServer {
        db,
        config: Arc::new(config),
    };

    let http_config = StreamableHttpServerConfig {
        json_response: true,
        stateful_mode: false,
        ..Default::default()
    };

    let session_manager = Arc::new(LocalSessionManager::default());
    let service = StreamableHttpService::new(
        move || Ok(server.clone()),
        session_manager,
        http_config,
    );

    let app = axum::Router::new().route(
        "/mcp",
        axum::routing::any_service(service),
    );

    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!("IronMem MCP Streamable HTTP server listening on {}", bind);
    tracing::info!("Endpoint: http://{}/mcp", bind);

    tokio::select! {
        result = axum::serve(listener, app) => {
            if let Err(e) = result {
                tracing::error!("Streamable HTTP server error: {}", e);
            }
        }
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("Shutting down...");
        }
    }

    Ok(())
}
