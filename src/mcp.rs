use crate::config::Config;
use crate::db::{self, Database, Memory};
use crate::embedder::Embedder;
use crate::vectorstore::{self, VectorStore};
use crate::{compress, hooks, retrieval};
use anyhow::Result;
use axum::extract::Request;
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
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

/// A successful tool result whose payload reports a graceful, non-fatal error
/// (e.g. unknown id / missing blob) — distinct from an MCP protocol error.
fn error_result(message: impl Into<String>) -> CallToolResult {
    let json = serde_json::json!({ "ok": false, "error": message.into() });
    CallToolResult::success(vec![Content::text(
        serde_json::to_string_pretty(&json).unwrap(),
    )])
}

fn extract_bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
}

async fn require_bearer_auth(request: Request, next: Next, auth_token: String) -> Response {
    match extract_bearer_token(request.headers()) {
        Some(token) if token == auth_token => next.run(request).await,
        _ => (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            "Missing or invalid bearer token",
        )
            .into_response(),
    }
}

fn with_optional_bearer_auth(router: axum::Router, auth_token: Option<String>) -> axum::Router {
    match auth_token {
        Some(auth_token) if !auth_token.trim().is_empty() => {
            router.route_layer(middleware::from_fn(move |request, next| {
                let auth_token = auth_token.clone();
                async move { require_bearer_auth(request, next, auth_token).await }
            }))
        }
        _ => router,
    }
}

#[derive(Clone)]
pub struct IronMemServer {
    db: Arc<Database>,
    config: Arc<Config>,
    embedder: Option<Arc<dyn Embedder>>,
    store: Arc<dyn VectorStore>,
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
                "Retrieve memories for a project. Optionally search with a query. Results include expansion chunks with chunk_id handles for retrieve_original.",
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
                "memory_skim",
                "Return the compressed working-memory skim chunks for one project or globally. Use chunk_id with retrieve_original to expand exact evidence on demand.",
                schema(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "project": { "type": "string", "description": "Project root path. Omit when global=true." },
                        "limit": { "type": "integer", "description": "Max chunks (default 15)" },
                        "global": { "type": "boolean", "description": "When true, skim across all projects." }
                    }
                })),
            ),
            Tool::new(
                "get_status",
                "Get database stats: total sessions, memories, observations, graph edges, and CCR storage.",
                schema(serde_json::json!({
                    "type": "object",
                    "properties": {}
                })),
            ),
            Tool::new(
                "retrieve_original",
                "Retrieve the verbatim original behind a compressed/truncated memory. Provide chunk_id (preferred expansion handle), observation_id, memory_id, or a blob hash.",
                schema(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "chunk_id": { "type": "string", "description": "Chunk id returned by get_context or memory_skim; expands exact source span when available" },
                        "observation_id": { "type": "integer", "description": "Observation id whose full original output to retrieve" },
                        "memory_id": { "type": "integer", "description": "Memory id whose verbatim pre-LLM session transcript to retrieve" },
                        "hash": { "type": "string", "description": "Blob content hash (alternative to chunk_id / observation_id / memory_id)" }
                    }
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
                "Hybrid (keyword + semantic) search across session memories.",
                schema(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Search query" },
                        "project": { "type": "string", "description": "Project root path" },
                        "limit": { "type": "integer", "description": "Max results (default 10)" },
                        "semantic": { "type": "boolean", "description": "Blend semantic vector search with keyword search (default true). Set false for keyword-only." }
                    },
                    "required": ["query", "project"]
                })),
            ),
            Tool::new(
                "search_global",
                "Hybrid (keyword + semantic) search across all projects.",
                schema(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Search query" },
                        "limit": { "type": "integer", "description": "Max results (default 10)" },
                        "semantic": { "type": "boolean", "description": "Blend semantic vector search with keyword search (default true). Set false for keyword-only." }
                    },
                    "required": ["query"]
                })),
            ),
            Tool::new(
                "list_projects",
                "List all projects that have stored memories.",
                schema(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "limit": { "type": "integer", "description": "Max results (default 50)" }
                    }
                })),
            ),
            Tool::new(
                "list_sessions",
                "List session history for a project, including observation counts and memory tags.",
                schema(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "project": { "type": "string", "description": "Project root path" },
                        "limit": { "type": "integer", "description": "Max results (default 20)" }
                    },
                    "required": ["project"]
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
                "remember",
                "Store an explicit, durable memory. Use scope='user' for facts/preferences about the user that apply across every project; scope='project' (default) for this project only. kind classifies it: session | error_solution | preference | architecture | learned_pattern | project_config | profile.",
                schema(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "project": { "type": "string", "description": "Project root path" },
                        "text": { "type": "string", "description": "The memory content to store verbatim" },
                        "scope": { "type": "string", "description": "'project' (default) or 'user' (cross-project)" },
                        "kind": { "type": "string", "description": "session | error_solution | preference | architecture | learned_pattern | project_config | profile (default preference)" },
                        "tags": { "type": "string", "description": "Optional space-separated keywords" }
                    },
                    "required": ["project", "text"]
                })),
            ),
            Tool::new(
                "get_profile",
                "Get the current user profile (durable cross-project facts + recent activity), if one has been generated.",
                schema(serde_json::json!({
                    "type": "object",
                    "properties": {}
                })),
            ),
            Tool::new(
                "refresh_profile",
                "Regenerate the user profile from scope=user memories (uses the LLM when available, else a deterministic local rollup) and return it.",
                schema(serde_json::json!({
                    "type": "object",
                    "properties": {}
                })),
            ),
            Tool::new(
                "list_corrections",
                "List mined error→fix corrections (kind=error_solution) — past failures and how they were resolved. Optionally scope to a project.",
                schema(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "project": { "type": "string", "description": "Project root path (omit for all projects)" },
                        "limit": { "type": "integer", "description": "Max results (default 10)" }
                    }
                })),
            ),
            Tool::new(
                "memory_graph",
                "Query temporal graph edges for an entity. Returns active edges by default; include_superseded=true returns provenance history.",
                schema(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "entity": { "type": "string", "description": "Entity to query (person, project, organization, concept)" },
                        "project": { "type": "string", "description": "Optional project root path; omit for all projects" },
                        "include_superseded": { "type": "boolean", "description": "Include duplicate/superseded historical edges (default false)" },
                        "at_time": { "type": "string", "description": "Optional YYYY-MM-DD valid-time filter" },
                        "limit": { "type": "integer", "description": "Max graph edges (default 20)" }
                    },
                    "required": ["entity"]
                })),
            ),
            Tool::new(
                "reconcile_memory_graph",
                "Scan temporal graph edges and mark duplicates/current-state supersessions. Use dry_run=true to preview.",
                schema(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "project": { "type": "string", "description": "Optional project root path; omit for all projects" },
                        "dry_run": { "type": "boolean", "description": "Report what would change without writing (default false)" }
                    }
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

    async fn handle_retrieve_original(
        &self,
        args: &JsonObject,
    ) -> Result<CallToolResult, ErrorData> {
        match crate::expansion::retrieve_original(
            &self.db,
            args.get("observation_id").and_then(|v| v.as_i64()),
            args.get("memory_id").and_then(|v| v.as_i64()),
            args.get("hash").and_then(|v| v.as_str()),
            args.get("chunk_id").and_then(|v| v.as_str()),
        )
        .await
        {
            Ok(expanded) => {
                let mut json = serde_json::to_value(expanded).unwrap();
                if let Some(obj) = json.as_object_mut() {
                    obj.insert("ok".to_string(), serde_json::json!(true));
                }
                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string_pretty(&json).unwrap(),
                )]))
            }
            Err(e) => Ok(error_result(e.to_string())),
        }
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
        let semantic = semantic_arg(args);

        let memories = match query {
            Some(q) if !q.is_empty() => self
                .hybrid(Some(project), q, limit, semantic)
                .await
                .unwrap_or_default(),
            _ => db::get_recent_memories(&self.db, project, limit)
                .await
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?,
        };

        let memory_ids: Vec<i64> = memories.iter().map(|m| m.id).collect();
        let chunks = db::chunks_for_memories(&self.db, &memory_ids)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let expansions: Vec<_> = memory_ids
            .into_iter()
            .map(|memory_id| {
                serde_json::json!({
                    "memory_id": memory_id,
                    "chunks": chunks.get(&memory_id).cloned().unwrap_or_default(),
                })
            })
            .collect();

        let json = serde_json::json!({ "memories": memories, "expansions": expansions });
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&json).unwrap(),
        )]))
    }

    async fn handle_memory_skim(&self, args: &JsonObject) -> Result<CallToolResult, ErrorData> {
        let limit = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(15);
        let global = args
            .get("global")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let project = if global {
            None
        } else {
            args.get("project").and_then(|v| v.as_str())
        };
        let chunks = db::recent_memory_chunks(&self.db, project, limit)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let json = serde_json::json!({ "chunks": chunks });
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
            "memory_edges": stats.total_memory_edges,
            "memory_chunks": stats.total_memory_chunks,
            "db_path": self.config.db_path,
            "ccr": stats.ccr_json(),
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
        let semantic = semantic_arg(args);

        let memories = self
            .hybrid(Some(project), query, limit, semantic)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let json = serde_json::json!({ "memories": memories });
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&json).unwrap(),
        )]))
    }

    async fn handle_search_global(&self, args: &JsonObject) -> Result<CallToolResult, ErrorData> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ErrorData::invalid_params("missing 'query'", None))?;
        let limit = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(10);
        let semantic = semantic_arg(args);

        let memories = self
            .hybrid(None, query, limit, semantic)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let json = serde_json::json!({ "memories": memories });
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&json).unwrap(),
        )]))
    }

    async fn handle_list_projects(&self, args: &JsonObject) -> Result<CallToolResult, ErrorData> {
        let limit = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(50);

        let projects = db::list_projects(&self.db, limit)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let json = serde_json::json!({ "projects": projects });
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&json).unwrap(),
        )]))
    }

    async fn handle_list_sessions(&self, args: &JsonObject) -> Result<CallToolResult, ErrorData> {
        let project = args
            .get("project")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ErrorData::invalid_params("missing 'project'", None))?;
        let limit = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(20);

        let sessions = db::list_session_history(&self.db, project, limit)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let json = serde_json::json!({ "sessions": sessions });
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

        let memories = retrieval::rank_for_injection(
            &self.db,
            self.embedder.as_deref(),
            self.store.as_ref(),
            project,
            &self.config.embedding.weights,
            self.config.embedding.recency_half_life_days,
            limit as usize,
        )
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

    async fn handle_remember(&self, args: &JsonObject) -> Result<CallToolResult, ErrorData> {
        let project = args
            .get("project")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ErrorData::invalid_params("missing 'project'", None))?;
        let text = args
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ErrorData::invalid_params("missing 'text'", None))?;
        if text.trim().is_empty() {
            return Ok(error_result("'text' must not be empty"));
        }
        let scope = args
            .get("scope")
            .and_then(|v| v.as_str())
            .unwrap_or("project");
        let kind = args
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or("preference");
        let tags = args.get("tags").and_then(|v| v.as_str());

        let memory_id = compress::remember(
            &self.db,
            self.embedder.as_deref(),
            self.store.as_ref(),
            project,
            scope,
            kind,
            text,
            tags,
        )
        .await
        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let json = serde_json::json!({
            "ok": true,
            "memory_id": memory_id,
            "scope": db::clamp_scope(scope),
            "kind": db::clamp_kind(kind),
        });
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&json).unwrap(),
        )]))
    }

    async fn handle_get_profile(&self) -> Result<CallToolResult, ErrorData> {
        let profile = db::get_profile_memory(&self.db)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let json = serde_json::json!({ "ok": true, "profile": profile });
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&json).unwrap(),
        )]))
    }

    async fn handle_refresh_profile(&self) -> Result<CallToolResult, ErrorData> {
        let id = crate::profile::regenerate(
            &self.db,
            self.embedder.as_deref(),
            self.store.as_ref(),
            Some(&self.config),
        )
        .await
        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let profile = db::get_profile_memory(&self.db)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let json = serde_json::json!({
            "ok": true,
            "regenerated": id.is_some(),
            "profile": profile,
        });
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&json).unwrap(),
        )]))
    }

    async fn handle_list_corrections(
        &self,
        args: &JsonObject,
    ) -> Result<CallToolResult, ErrorData> {
        let project = args.get("project").and_then(|v| v.as_str());
        let limit = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(10);

        let corrections = db::get_memories_by_kind(&self.db, project, "error_solution", limit)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let json = serde_json::json!({ "corrections": corrections });
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&json).unwrap(),
        )]))
    }

    async fn handle_memory_graph(&self, args: &JsonObject) -> Result<CallToolResult, ErrorData> {
        let entity = args
            .get("entity")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ErrorData::invalid_params("missing 'entity'", None))?;
        let project = args.get("project").and_then(|v| v.as_str());
        let include_superseded = args
            .get("include_superseded")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let at_time = args.get("at_time").and_then(|v| v.as_str());
        if let Some(at) = at_time {
            if !crate::provider::is_valid_memory_date(at) {
                return Err(ErrorData::invalid_params(
                    "at_time must be a valid YYYY-MM-DD date",
                    None,
                ));
            }
        }
        let limit = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(20);

        let edges = db::memory_edges_for_entity_at(
            &self.db,
            project,
            entity,
            include_superseded,
            at_time,
            limit.max(1) as usize,
        )
        .await
        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let json = serde_json::json!({
            "ok": true,
            "entity": entity,
            "project": project,
            "include_superseded": include_superseded,
            "at_time": at_time,
            "edges": edges,
        });
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&json).unwrap(),
        )]))
    }

    async fn handle_reconcile_memory_graph(
        &self,
        args: &JsonObject,
    ) -> Result<CallToolResult, ErrorData> {
        let project = args.get("project").and_then(|v| v.as_str());
        let dry_run = args
            .get("dry_run")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let report = db::reconcile_memory_graph(&self.db, project, dry_run)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let json = serde_json::json!({ "ok": true, "report": report });
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&json).unwrap(),
        )]))
    }

    async fn handle_wipe_project(&self, args: &JsonObject) -> Result<CallToolResult, ErrorData> {
        let project = args
            .get("project")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ErrorData::invalid_params("missing 'project'", None))?;

        // Capture ids before deletion so we can purge their vectors + metadata.
        let ids = db::memory_ids_for_project(&self.db, project)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let count = db::delete_memories_for_project(&self.db, project)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        for id in ids {
            // Release the CCR session-transcript reference before purge_memory
            // deletes the memory_meta row that records it.
            if let Err(e) = db::decref_memory_session_blob(&self.db, id).await {
                tracing::warn!("CCR decref failed for memory {id}: {e}");
            }
            if let Err(e) = vectorstore::purge_memory(&self.db, self.store.as_ref(), id).await {
                tracing::warn!("vector/meta cleanup failed for memory {id}: {e}");
            }
        }

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
        compress::run(
            &self.db,
            self.embedder.as_deref(),
            self.store.as_ref(),
            &self.config,
            session_id,
        )
        .await
    }

    /// Hybrid (keyword + semantic) search. `semantic=false` forces FTS-only.
    /// With no embedder configured the result is identical to legacy FTS.
    async fn hybrid(
        &self,
        project: Option<&str>,
        query: &str,
        limit: i64,
        semantic: bool,
    ) -> anyhow::Result<Vec<Memory>> {
        let embedder = if semantic {
            self.embedder.as_deref()
        } else {
            None
        };
        retrieval::hybrid_search(
            &self.db,
            embedder,
            self.store.as_ref(),
            project,
            query,
            limit as usize,
        )
        .await
    }
}

/// Read the optional `semantic` tool arg (default true).
fn semantic_arg(args: &JsonObject) -> bool {
    args.get("semantic")
        .and_then(|v| v.as_bool())
        .unwrap_or(true)
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
            "memory_skim" => self.handle_memory_skim(&args).await,
            "get_status" => self.handle_get_status().await,
            "retrieve_original" => self.handle_retrieve_original(&args).await,
            "list_memories" => self.handle_list_memories(&args).await,
            "search_memories" => self.handle_search_memories(&args).await,
            "search_global" => self.handle_search_global(&args).await,
            "list_projects" => self.handle_list_projects(&args).await,
            "list_sessions" => self.handle_list_sessions(&args).await,
            "inject_context" => self.handle_inject_context(&args).await,
            "remember" => self.handle_remember(&args).await,
            "get_profile" => self.handle_get_profile().await,
            "refresh_profile" => self.handle_refresh_profile().await,
            "list_corrections" => self.handle_list_corrections(&args).await,
            "memory_graph" => self.handle_memory_graph(&args).await,
            "reconcile_memory_graph" => self.handle_reconcile_memory_graph(&args).await,
            "wipe_project" => self.handle_wipe_project(&args).await,
            _ => Err(ErrorData::invalid_params(
                format!("unknown tool: {}", request.name),
                None,
            )),
        }
    }
}

pub async fn run_stdio(db: Arc<Database>, config: Config) -> Result<()> {
    let (embedder, store) = vectorstore::build_semantic(&db, &config).await;
    let server = IronMemServer {
        db,
        config: Arc::new(config),
        embedder,
        store,
    };

    let service = server.serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
}

pub async fn run_streamable_http(
    db: Arc<Database>,
    config: Config,
    bind: SocketAddr,
) -> Result<()> {
    let (embedder, store) = vectorstore::build_semantic(&db, &config).await;
    let server = IronMemServer {
        db,
        config: Arc::new(config),
        embedder,
        store,
    };
    let auth_token = server.config.auth_token.clone();

    // rmcp >=1.4 marks StreamableHttpServerConfig #[non_exhaustive], so it can no
    // longer be built with a struct literal — start from Default and set fields.
    let mut http_config = StreamableHttpServerConfig::default();
    http_config.json_response = true;
    http_config.stateful_mode = false;

    let session_manager = Arc::new(LocalSessionManager::default());
    let service =
        StreamableHttpService::new(move || Ok(server.clone()), session_manager, http_config);

    let app = with_optional_bearer_auth(
        axum::Router::new().route("/mcp", axum::routing::any_service(service)),
        auth_token,
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use axum::routing::get;
    use axum::Router;
    use tower::util::ServiceExt;

    #[tokio::test]
    async fn auth_middleware_rejects_requests_without_token() {
        let app = with_optional_bearer_auth(
            Router::new().route("/mcp", get(|| async { "ok" })),
            Some("secret-token".to_string()),
        );

        let response = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/mcp")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            response.headers().get(header::WWW_AUTHENTICATE).unwrap(),
            "Bearer"
        );
    }

    #[tokio::test]
    async fn auth_middleware_accepts_matching_bearer_token() {
        let app = with_optional_bearer_auth(
            Router::new().route("/mcp", get(|| async { "ok" })),
            Some("secret-token".to_string()),
        );

        let response = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/mcp")
                    .header(header::AUTHORIZATION, "Bearer secret-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn auth_middleware_is_disabled_without_token() {
        let app =
            with_optional_bearer_auth(Router::new().route("/mcp", get(|| async { "ok" })), None);

        let response = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/mcp")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    // ── retrieve_original (CCR) ──────────────────────────────────────────────

    async fn test_server() -> (IronMemServer, String) {
        let db_path = std::env::temp_dir().join(format!("ironmem-mcp-{}.db", uuid::Uuid::new_v4()));
        let db_path_string = db_path.to_string_lossy().to_string();
        let db = Database::new(&db_path_string).await.unwrap();
        db.migrate().await.unwrap();
        let db = Arc::new(db);
        let mut config = Config::default();
        config.embedding.provider = "none".to_string(); // no embedder probe in tests
        let config = Arc::new(config);
        let (embedder, store) = vectorstore::build_semantic(&db, &config).await;
        (
            IronMemServer {
                db,
                config,
                embedder,
                store,
            },
            db_path_string,
        )
    }

    fn result_text(r: &CallToolResult) -> String {
        let v = serde_json::to_value(r).unwrap();
        v["content"][0]["text"]
            .as_str()
            .expect("text content")
            .to_string()
    }

    #[test]
    fn tool_list_includes_retrieve_original() {
        let tools = IronMemServer::build_tool_list();
        let t = tools
            .iter()
            .find(|t| t.name.as_ref() == "retrieve_original")
            .expect("retrieve_original tool registered");
        let v = serde_json::to_value(t).unwrap();
        let props = &v["inputSchema"]["properties"];
        assert!(
            props.get("observation_id").is_some(),
            "schema has observation_id"
        );
        assert!(props.get("chunk_id").is_some(), "schema has chunk_id");
        assert!(props.get("hash").is_some(), "schema has hash");
    }

    #[test]
    fn tool_list_includes_memory_skim() {
        let tools = IronMemServer::build_tool_list();
        let t = tools
            .iter()
            .find(|t| t.name.as_ref() == "memory_skim")
            .expect("memory_skim tool registered");
        let v = serde_json::to_value(t).unwrap();
        let props = &v["inputSchema"]["properties"];
        assert!(props.get("project").is_some(), "schema has project");
        assert!(props.get("global").is_some(), "schema has global");
    }

    #[tokio::test]
    async fn remember_stores_user_scoped_memory_retrievable_cross_project() {
        let (server, path) = test_server().await;
        let mut args = JsonObject::new();
        args.insert("project".into(), serde_json::json!("/tmp/projX"));
        args.insert(
            "text".into(),
            serde_json::json!("user prefers vim keybindings"),
        );
        args.insert("scope".into(), serde_json::json!("user"));
        args.insert("kind".into(), serde_json::json!("preference"));
        args.insert("tags".into(), serde_json::json!("editor pref"));

        let v: serde_json::Value =
            serde_json::from_str(&result_text(&server.handle_remember(&args).await.unwrap()))
                .unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["scope"], "user");
        assert_eq!(v["kind"], "preference");
        let mid = v["memory_id"].as_i64().unwrap();

        // Visible via the global user scope (i.e. from any other project).
        let users = db::get_recent_memories_scoped(&server.db, "user", None, 10)
            .await
            .unwrap();
        assert!(users.iter().any(|m| m.id == mid));

        // Empty text is rejected gracefully (ok:false, not a protocol error).
        let mut bad = JsonObject::new();
        bad.insert("project".into(), serde_json::json!("/tmp/projX"));
        bad.insert("text".into(), serde_json::json!("   "));
        let v2: serde_json::Value =
            serde_json::from_str(&result_text(&server.handle_remember(&bad).await.unwrap()))
                .unwrap();
        assert_eq!(v2["ok"], false);

        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn get_and_refresh_profile_tools() {
        // Use a provider whose key resolves from the env only (no ~/.ironmem
        // file fallback) and leave it unset, so refresh uses the deterministic
        // local rollup — no network call in tests, on any machine.
        let db_path =
            std::env::temp_dir().join(format!("ironmem-pmcp-{}.db", uuid::Uuid::new_v4()));
        let dbs = db_path.to_string_lossy().to_string();
        let db = Database::new(&dbs).await.unwrap();
        db.migrate().await.unwrap();
        let db = Arc::new(db);
        let mut config = Config {
            provider: crate::provider::Provider::Openai,
            ..Config::default()
        };
        config.embedding.provider = "none".to_string();
        let config = Arc::new(config);
        let (embedder, store) = vectorstore::build_semantic(&db, &config).await;
        let server = IronMemServer {
            db,
            config,
            embedder,
            store,
        };

        // No profile yet.
        let v: serde_json::Value =
            serde_json::from_str(&result_text(&server.handle_get_profile().await.unwrap()))
                .unwrap();
        assert!(v["profile"].is_null());

        // Seed a user memory, then refresh.
        let s = db::create_session(&server.db, "/tmp/p").await.unwrap();
        let uid = db::insert_memory(
            &server.db,
            "/tmp/p",
            &s,
            "user prefers dark mode",
            Some("pref"),
        )
        .await
        .unwrap();
        db::set_memory_scope_kind(&server.db, uid, "user", "preference")
            .await
            .unwrap();

        let rv: serde_json::Value = serde_json::from_str(&result_text(
            &server.handle_refresh_profile().await.unwrap(),
        ))
        .unwrap();
        assert_eq!(rv["ok"], true);
        assert_eq!(rv["regenerated"], true);
        assert!(rv["profile"]["summary"]
            .as_str()
            .unwrap()
            .contains("dark mode"));

        // get_profile now returns it.
        let v2: serde_json::Value =
            serde_json::from_str(&result_text(&server.handle_get_profile().await.unwrap()))
                .unwrap();
        assert!(!v2["profile"].is_null());

        let _ = std::fs::remove_file(db_path);
    }

    #[tokio::test]
    async fn list_corrections_returns_mined_error_solutions() {
        let (server, path) = test_server().await;
        let s = db::create_session(&server.db, "/tmp/p").await.unwrap();
        let transcript = vec![
            crate::db::Observation {
                id: 0,
                session_id: s.clone(),
                project: "/tmp/p".into(),
                tool: "Bash".into(),
                input: Some("cargo build".into()),
                output: Some("error[E0425]: cannot find value `foo`".into()),
                created_at: 0,
            },
            crate::db::Observation {
                id: 0,
                session_id: s.clone(),
                project: "/tmp/p".into(),
                tool: "Edit".into(),
                input: Some("src/lib.rs".into()),
                output: Some("ok".into()),
                created_at: 0,
            },
            crate::db::Observation {
                id: 0,
                session_id: s.clone(),
                project: "/tmp/p".into(),
                tool: "Bash".into(),
                input: Some("cargo build".into()),
                output: Some("Finished `dev` profile".into()),
                created_at: 0,
            },
        ];
        let n = crate::corrections::mine_and_store(
            &server.db,
            server.embedder.as_deref(),
            server.store.as_ref(),
            "/tmp/p",
            &s,
            &transcript,
        )
        .await
        .unwrap();
        assert_eq!(n, 1);

        let mut args = JsonObject::new();
        args.insert("project".into(), serde_json::json!("/tmp/p"));
        let v: serde_json::Value = serde_json::from_str(&result_text(
            &server.handle_list_corrections(&args).await.unwrap(),
        ))
        .unwrap();
        let arr = v["corrections"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert!(arr[0]["summary"].as_str().unwrap().contains("E0425"));

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn tool_list_includes_remember() {
        let tools = IronMemServer::build_tool_list();
        let t = tools
            .iter()
            .find(|t| t.name.as_ref() == "remember")
            .expect("remember tool registered");
        let v = serde_json::to_value(t).unwrap();
        let props = &v["inputSchema"]["properties"];
        assert!(props.get("scope").is_some() && props.get("kind").is_some());
    }

    #[tokio::test]
    async fn retrieve_original_by_observation_id_returns_full_output() {
        let (server, path) = test_server().await;
        let s = db::create_session(&server.db, "/tmp/p").await.unwrap();
        let big = "x✓".repeat(40_000); // ~160 KB, multibyte, well over the cap
        let id = db::insert_observation(&server.db, &s, "/tmp/p", "Read", None, Some(&big), 2048)
            .await
            .unwrap();

        let mut args = JsonObject::new();
        args.insert("observation_id".into(), serde_json::json!(id));
        let result = server.handle_retrieve_original(&args).await.unwrap();

        let v: serde_json::Value = serde_json::from_str(&result_text(&result)).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["original"].as_str().unwrap(), big);

        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn retrieve_original_by_hash_returns_blob() {
        let (server, path) = test_server().await;
        let r = crate::ccr::store_blob(&server.db, b"verbatim bytes addressed by hash", None)
            .await
            .unwrap();

        let mut args = JsonObject::new();
        args.insert("hash".into(), serde_json::json!(r.hash));
        let result = server.handle_retrieve_original(&args).await.unwrap();

        let v: serde_json::Value = serde_json::from_str(&result_text(&result)).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(
            v["original"].as_str().unwrap(),
            "verbatim bytes addressed by hash"
        );

        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn retrieve_original_by_chunk_id_returns_exact_span() {
        let (server, path) = test_server().await;
        let s = db::create_session(&server.db, "/tmp/p").await.unwrap();
        let mem_id = db::insert_memory(&server.db, "/tmp/p", &s, "summary", Some("t"))
            .await
            .unwrap();
        let transcript = "alpha\nbravo\ncharlie\n";
        let r = crate::ccr::store_blob(&server.db, transcript.as_bytes(), None)
            .await
            .unwrap();
        db::replace_memory_chunks(
            &server.db,
            mem_id,
            &[db::NewMemoryChunk {
                chunk_id: format!("mem:{mem_id}:obs:1"),
                project: "/tmp/p".to_string(),
                memory_id: mem_id,
                session_id: s,
                ordinal: 0,
                density: "high".to_string(),
                kind: "session".to_string(),
                title: "Observation".to_string(),
                summary: "bravo chunk".to_string(),
                source_hash: Some(r.hash),
                source_start: Some(6),
                source_end: Some(12),
                token_estimate: 2,
            }],
        )
        .await
        .unwrap();

        let mut args = JsonObject::new();
        args.insert(
            "chunk_id".into(),
            serde_json::json!(format!("mem:{mem_id}:obs:1")),
        );
        let result = server.handle_retrieve_original(&args).await.unwrap();

        let v: serde_json::Value = serde_json::from_str(&result_text(&result)).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["original"].as_str().unwrap(), "bravo\n");
        assert_eq!(v["memory_id"].as_i64(), Some(mem_id));

        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn context_and_memory_skim_return_expansion_chunks() {
        let (server, path) = test_server().await;
        let s = db::create_session(&server.db, "/tmp/p").await.unwrap();
        let mem_id = db::insert_memory(&server.db, "/tmp/p", &s, "alpha summary", Some("t"))
            .await
            .unwrap();
        db::replace_memory_chunks(
            &server.db,
            mem_id,
            &[db::NewMemoryChunk {
                chunk_id: format!("mem:{mem_id}:overview"),
                project: "/tmp/p".to_string(),
                memory_id: mem_id,
                session_id: s,
                ordinal: 0,
                density: "medium".to_string(),
                kind: "session".to_string(),
                title: "Memory overview".to_string(),
                summary: "alpha summary".to_string(),
                source_hash: None,
                source_start: None,
                source_end: None,
                token_estimate: 3,
            }],
        )
        .await
        .unwrap();

        let mut context_args = JsonObject::new();
        context_args.insert("project".into(), serde_json::json!("/tmp/p"));
        let context: serde_json::Value = serde_json::from_str(&result_text(
            &server.handle_get_context(&context_args).await.unwrap(),
        ))
        .unwrap();
        assert_eq!(
            context["expansions"][0]["chunks"][0]["chunk_id"]
                .as_str()
                .unwrap(),
            format!("mem:{mem_id}:overview")
        );

        let mut skim_args = JsonObject::new();
        skim_args.insert("project".into(), serde_json::json!("/tmp/p"));
        let skim: serde_json::Value = serde_json::from_str(&result_text(
            &server.handle_memory_skim(&skim_args).await.unwrap(),
        ))
        .unwrap();
        assert_eq!(
            skim["chunks"][0]["chunk_id"].as_str().unwrap(),
            format!("mem:{mem_id}:overview")
        );

        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn retrieve_original_unknown_id_is_graceful() {
        let (server, path) = test_server().await;
        let mut args = JsonObject::new();
        args.insert("observation_id".into(), serde_json::json!(999_999));
        let result = server.handle_retrieve_original(&args).await.unwrap();

        // Graceful (not an MCP protocol error): a success result with ok=false.
        let v: serde_json::Value = serde_json::from_str(&result_text(&result)).unwrap();
        assert_eq!(v["ok"], false);

        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn retrieve_original_by_memory_id_returns_transcript() {
        let (server, path) = test_server().await;
        let s = db::create_session(&server.db, "/tmp/p").await.unwrap();
        let mem_id = db::insert_memory(&server.db, "/tmp/p", &s, "summary", Some("t"))
            .await
            .unwrap();
        db::upsert_memory_meta(&server.db, mem_id, 0.5)
            .await
            .unwrap();

        let transcript = "## Read\ninput: x\noutput: y\n\n";
        let r = crate::ccr::store_blob(&server.db, transcript.as_bytes(), None)
            .await
            .unwrap();
        db::set_memory_session_blob(&server.db, mem_id, &r.hash)
            .await
            .unwrap();

        let mut args = JsonObject::new();
        args.insert("memory_id".into(), serde_json::json!(mem_id));
        let result = server.handle_retrieve_original(&args).await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&result_text(&result)).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["original"].as_str().unwrap(), transcript);

        // Unknown memory id → graceful ok:false.
        let mut args = JsonObject::new();
        args.insert("memory_id".into(), serde_json::json!(987_654));
        let v: serde_json::Value = serde_json::from_str(&result_text(
            &server.handle_retrieve_original(&args).await.unwrap(),
        ))
        .unwrap();
        assert_eq!(v["ok"], false);

        let _ = std::fs::remove_file(path);
    }
}
