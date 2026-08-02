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

fn purpose_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "description": "Optional purpose envelope. Strict mode requires a trusted opaque attestation.",
        "properties": {
            "request_id": { "type": "string" },
            "namespace": { "type": "string" },
            "project": { "type": "string" },
            "task_type": { "type": "string" },
            "intended_action": { "type": ["string", "null"] },
            "action_risk": { "type": "string", "enum": ["none", "low", "medium", "high", "critical"] },
            "require_source_backing": { "type": "boolean" },
            "purpose_attestation": { "type": ["string", "null"] },
            "confirmation_receipt": { "type": ["string", "null"] }
        },
        "required": ["request_id", "namespace", "project", "task_type", "action_risk"]
    })
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
    policy_principal: crate::influence::PolicyPrincipal,
}

impl IronMemServer {
    fn purpose_arg(args: &JsonObject) -> Result<Option<crate::purpose::RecallPurpose>, ErrorData> {
        args.get("purpose")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|error| ErrorData::invalid_params(format!("invalid purpose: {error}"), None))
    }

    async fn gate_memories(
        &self,
        memories: Vec<db::Memory>,
        namespace: &str,
        project: &str,
        args: &JsonObject,
        consumer: crate::egress::ConsumerCapabilities,
    ) -> Result<crate::egress::GateResult, ErrorData> {
        let purpose = Self::purpose_arg(args)?;
        let channel = if self.policy_principal.authority == "local_operator" {
            crate::egress::PurposeChannel::LocalOperator(self.policy_principal.actor.clone())
        } else {
            // Shared-token MCP is not per-agent identity. Strict calls must
            // carry a trusted purpose attestation.
            crate::egress::PurposeChannel::Remote {
                authenticated_agent: None,
            }
        };
        crate::egress::gate_memories_with_query(
            &self.db,
            memories,
            namespace,
            project,
            purpose.as_ref(),
            channel,
            consumer,
            &self.config.influence,
            args.get("query").and_then(|value| value.as_str()),
        )
        .await
        .map_err(|error| ErrorData::invalid_params(error.to_string(), None))
    }

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
                        "query": { "type": "string", "description": "Search query (optional)" },
                        "namespace": { "type": "string", "description": "Governance namespace/realm boundary (default local)" },
                        "purpose": purpose_schema()
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
                        "global": { "type": "boolean", "description": "When true, skim across all projects." },
                        "namespace": { "type": "string", "description": "Governance namespace/realm boundary (default local)" },
                        "purpose": purpose_schema()
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
                        "hash": { "type": "string", "description": "Blob content hash (alternative to chunk_id / observation_id / memory_id)" },
                        "namespace": { "type": "string", "description": "Governance namespace/realm boundary (default local)" },
                        "purpose": purpose_schema()
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
                        "limit": { "type": "integer", "description": "Max results (default 5)" },
                        "namespace": { "type": "string", "description": "Governance namespace/realm boundary (default local)" },
                        "purpose": purpose_schema()
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
                        "semantic": { "type": "boolean", "description": "Blend semantic vector search with keyword search (default true). Set false for keyword-only." },
                        "namespace": { "type": "string", "description": "Governance namespace/realm boundary (default local)" },
                        "purpose": purpose_schema()
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
                        "semantic": { "type": "boolean", "description": "Blend semantic vector search with keyword search (default true). Set false for keyword-only." },
                        "namespace": { "type": "string", "description": "Governance namespace/realm boundary (default local)" },
                        "purpose": purpose_schema()
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
                        "limit": { "type": "integer", "description": "Max memories to inject (default 5)" },
                        "namespace": { "type": "string", "description": "Governance namespace/realm boundary (default local)" },
                        "purpose": purpose_schema()
                    },
                    "required": ["project"]
                })),
            ),
            Tool::new(
                "remember",
                "Store an explicit, durable governed memory. Use scope='user' for facts/preferences that apply across projects inside the namespace; scope='project' (default) for this project only. PHI/PII requires consent_state='granted'.",
                schema(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "project": { "type": "string", "description": "Project root path" },
                        "text": { "type": "string", "description": "The memory content to store verbatim" },
                        "scope": { "type": "string", "description": "'project' (default) or 'user' (cross-project)" },
                        "kind": { "type": "string", "description": "session | error_solution | preference | architecture | learned_pattern | project_config | profile (default preference)" },
                        "tags": { "type": "string", "description": "Optional space-separated keywords" },
                        "event_at": { "type": "string", "description": "Optional ISO date YYYY-MM-DD (or range YYYY-MM-DD..YYYY-MM-DD) of when the described event actually occurred or will occur. This is the event/valid time, distinct from the storage time (created_at). Powers time-aware retrieval." },
                        "namespace": { "type": "string", "description": "Governance namespace/realm boundary (default local)" },
                        "source_type": { "type": "string", "description": "user_input | tool_output | agent_generated | derived | external | sync_peer" },
                        "trust_tier": { "type": "string", "description": "high | medium | low | untrusted" },
                        "writer_identity": { "type": "string", "description": "Writer identity for the tamper-evident ledger" },
                        "classification": { "type": "string", "description": "public | internal | confidential | restricted | phi | pii" },
                        "consent_state": { "type": "string", "description": "required | granted | denied | withdrawn; PHI/PII requires granted" },
                        "residency": { "type": "string", "description": "Optional residency tag" },
                        "retention_policy_id": { "type": "string", "description": "Optional retention policy id" },
                        "expires_at": { "type": "integer", "description": "Optional Unix timestamp expiry" },
                        "legal_hold": { "type": "boolean", "description": "If true, governed forget refuses deletion" },
                        "source_ref": { "type": "string", "description": "Optional source event, receipt, URL, or tool id" }
                    },
                    "required": ["project", "text"]
                })),
            ),
            Tool::new(
                "get_memory_influence",
                "Read the versioned influence policy for a memory. Requires influence_policy:read on shared HTTP MCP; local stdio is a local-operator channel.",
                schema(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "memory_id": { "type": "integer", "description": "Memory id" },
                        "namespace": { "type": "string", "description": "Governance namespace (default local)" }
                    },
                    "required": ["memory_id"]
                })),
            ),
            Tool::new(
                "set_memory_influence",
                "Apply a version-checked influence-policy patch and append an atomic ledger receipt. Requires influence_policy:write on shared HTTP MCP; local stdio is a local-operator channel.",
                schema(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "memory_id": { "type": "integer", "description": "Memory id" },
                        "namespace": { "type": "string", "description": "Governance namespace (default local)" },
                        "expected_version": { "type": "integer", "minimum": 1, "description": "Current policy version for optimistic concurrency" },
                        "reason": { "type": "string", "description": "Required audit reason" },
                        "request_id": { "type": "string", "description": "Optional caller request id; generated when omitted" },
                        "state": { "type": "string", "enum": ["eligible", "quarantined", "reasoning_only", "action_restricted", "blocked", "superseded"] },
                        "allowed_task_types": { "type": "array", "items": { "type": "string" }, "maxItems": 64 },
                        "denied_task_types": { "type": "array", "items": { "type": "string" }, "maxItems": 64 },
                        "maximum_action_risk": { "type": "string", "enum": ["none", "low", "medium", "high", "critical"] },
                        "requires_original_source": { "type": "boolean" },
                        "requires_human_confirmation": { "type": "boolean" },
                        "maximum_derivation_depth": { "type": "integer", "minimum": 0 },
                        "clear_maximum_derivation_depth": { "type": "boolean", "description": "Clear an existing depth limit; mutually exclusive with maximum_derivation_depth" }
                    },
                    "required": ["memory_id", "expected_version", "reason"]
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
                "dream_memory",
                "Run a safe sleep-cycle consolidation pass. Defaults to dry_run=true and apply=false; use apply=true only when proposed memories should be promoted.",
                schema(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "project": { "type": "string", "description": "Optional project root path; omit for all projects" },
                        "dry_run": { "type": "boolean", "description": "Preview proposals without writing them (default true)" },
                        "apply": { "type": "boolean", "description": "Promote proposed consolidations into memories (default false)" },
                        "limit": { "type": "integer", "description": "Max memories per kind to scan (default 200)" }
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
                    "ok": false,
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
        let observation_id = args.get("observation_id").and_then(|v| v.as_i64());
        let memory_id = args.get("memory_id").and_then(|v| v.as_i64());
        let hash = args.get("hash").and_then(|v| v.as_str());
        let chunk_id = args.get("chunk_id").and_then(|v| v.as_str());
        let namespace = namespace_arg(args);
        let ids = db::memory_ids_for_original_reference(
            &self.db,
            observation_id,
            memory_id,
            hash,
            chunk_id,
        )
        .await
        .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
        let memories = db::memories_by_ids_in_namespace(&self.db, &ids, &namespace)
            .await
            .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
        if self.config.influence.enabled && memories.is_empty() {
            return Ok(error_result(
                "original_source_not_bound_to_authorized_memory".to_string(),
            ));
        }
        let project = memories
            .first()
            .map(|memory| memory.project.clone())
            .unwrap_or_else(|| "*".to_string());
        let gate = self
            .gate_memories(
                memories,
                &namespace,
                &project,
                args,
                crate::egress::ConsumerCapabilities {
                    reasoning_only_channel: true,
                    exact_source_expansion: true,
                    denial_diagnostics: false,
                },
            )
            .await?;
        if self.config.influence.enabled
            && gate.authorized.is_empty()
            && gate.advisory.is_empty()
            && gate.source_required.is_empty()
        {
            return Ok(error_result("memory_influence_denied".to_string()));
        }
        match crate::expansion::retrieve_original(
            &self.db,
            observation_id,
            memory_id,
            hash,
            chunk_id,
        )
        .await
        {
            Ok(expanded) => {
                let mut json = serde_json::to_value(expanded).unwrap();
                if let Some(obj) = json.as_object_mut() {
                    obj.insert("ok".to_string(), serde_json::json!(true));
                    obj.insert(
                        "influence_decisions".to_string(),
                        serde_json::to_value(gate.decisions).unwrap_or_default(),
                    );
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

    /// Dual-naming surfacing: build the `event_times` side map (memory id ->
    /// event/valid date), mirroring the HTTP /context response so the read
    /// tools expose when an event happened alongside created_at (when it was
    /// stored). Empty object when no returned memory carries an event date.
    async fn event_times_map(&self, memories: &[db::Memory]) -> serde_json::Value {
        let ids: Vec<i64> = memories.iter().map(|m| m.id).collect();
        let map = db::event_times_for(&self.db, &ids)
            .await
            .unwrap_or_default();
        serde_json::to_value(map).unwrap_or_else(|_| serde_json::json!({}))
    }

    async fn evidence_chains_json(
        &self,
        memory_ids: &[i64],
        chunks: &std::collections::HashMap<i64, Vec<db::MemoryChunk>>,
    ) -> serde_json::Value {
        let graph_edges = db::memory_edges_for_memories(&self.db, memory_ids)
            .await
            .unwrap_or_default();
        let mut chains = Vec::with_capacity(memory_ids.len());
        for &memory_id in memory_ids {
            let meta = db::get_memory_meta_full(&self.db, memory_id)
                .await
                .unwrap_or_default();
            chains.push(serde_json::json!({
                "memory_id": memory_id,
                "kind": meta.kind,
                "event_time": meta.event_time,
                "source_ref": meta.source_ref,
                "chunks": chunks.get(&memory_id).cloned().unwrap_or_default(),
                "graph_edges": graph_edges.get(&memory_id).cloned().unwrap_or_default(),
            }));
        }
        serde_json::Value::Array(chains)
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
        let namespace = namespace_arg(args);

        let memories = match query {
            Some(q) if !q.is_empty() => self
                .hybrid_in_namespace(&namespace, Some(project), q, limit, semantic)
                .await
                .unwrap_or_default(),
            _ => db::get_recent_memories_in_namespace(&self.db, &namespace, project, limit)
                .await
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?,
        };

        let gate = self
            .gate_memories(
                memories,
                &namespace,
                project,
                args,
                crate::egress::ConsumerCapabilities {
                    reasoning_only_channel: true,
                    exact_source_expansion: false,
                    denial_diagnostics: self.policy_principal.authority == "local_operator",
                },
            )
            .await?;
        let memories = gate.authorized;
        let memory_ids: Vec<i64> = memories.iter().map(|m| m.id).collect();
        let chunks = db::chunks_for_memories(&self.db, &memory_ids)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let expansions: Vec<_> = memory_ids
            .iter()
            .copied()
            .map(|memory_id| {
                serde_json::json!({
                    "memory_id": memory_id,
                    "chunks": chunks.get(&memory_id).cloned().unwrap_or_default(),
                })
            })
            .collect();

        let event_times = self.event_times_map(&memories).await;
        let evidence_chains = self.evidence_chains_json(&memory_ids, &chunks).await;
        let json = serde_json::json!({
            "memories": memories,
            "advisory_memories": gate.advisory,
            "influence_decisions": gate.decisions,
            "expansions": expansions,
            "event_times": event_times,
            "evidence_chains": evidence_chains
        });
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
        let namespace = namespace_arg(args);
        let chunks = db::recent_memory_chunks_in_namespace(&self.db, &namespace, project, limit)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let mem_ids: Vec<i64> = chunks.iter().map(|c| c.memory_id).collect();
        let memories = db::memories_by_ids_in_namespace(&self.db, &mem_ids, &namespace)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let gate = self
            .gate_memories(
                memories,
                &namespace,
                project.unwrap_or("*"),
                args,
                crate::egress::ConsumerCapabilities {
                    reasoning_only_channel: true,
                    exact_source_expansion: false,
                    denial_diagnostics: false,
                },
            )
            .await?;
        let allowed_ids = gate
            .authorized
            .iter()
            .map(|memory| memory.id)
            .collect::<std::collections::HashSet<_>>();
        let advisory_ids = gate
            .advisory
            .iter()
            .map(|memory| memory.id)
            .collect::<std::collections::HashSet<_>>();
        let authorized_chunks = chunks
            .iter()
            .filter(|chunk| allowed_ids.contains(&chunk.memory_id))
            .cloned()
            .collect::<Vec<_>>();
        let advisory_chunks = chunks
            .iter()
            .filter(|chunk| advisory_ids.contains(&chunk.memory_id))
            .cloned()
            .collect::<Vec<_>>();
        let event_times = serde_json::to_value(
            db::event_times_for(&self.db, &mem_ids)
                .await
                .unwrap_or_default(),
        )
        .unwrap_or_else(|_| serde_json::json!({}));
        let json = serde_json::json!({ "chunks": authorized_chunks, "advisory_chunks": advisory_chunks, "influence_decisions": gate.decisions, "event_times": event_times });
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
            "influence": {
                "enabled": self.config.influence.enabled,
                "mode": self.config.influence.mode,
                "require_purpose": self.config.influence.require_purpose,
                "require_trusted_attestation": self.config.influence.require_trusted_attestation,
                "events": db::influence_event_status(&self.db).await.unwrap_or_else(|_| serde_json::json!({})),
            },
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
        let namespace = namespace_arg(args);

        let memories = db::get_recent_memories_in_namespace(&self.db, &namespace, project, limit)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let gate = self
            .gate_memories(
                memories,
                &namespace,
                project,
                args,
                crate::egress::ConsumerCapabilities {
                    reasoning_only_channel: true,
                    exact_source_expansion: false,
                    denial_diagnostics: false,
                },
            )
            .await?;
        let event_times = self.event_times_map(&gate.authorized).await;
        let json = serde_json::json!({ "memories": gate.authorized, "advisory_memories": gate.advisory, "influence_decisions": gate.decisions, "event_times": event_times });
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
        let namespace = namespace_arg(args);

        let memories = self
            .hybrid_in_namespace(&namespace, Some(project), query, limit, semantic)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let gate = self
            .gate_memories(
                memories,
                &namespace,
                project,
                args,
                crate::egress::ConsumerCapabilities {
                    reasoning_only_channel: true,
                    exact_source_expansion: false,
                    denial_diagnostics: false,
                },
            )
            .await?;
        let event_times = self.event_times_map(&gate.authorized).await;
        let json = serde_json::json!({ "memories": gate.authorized, "advisory_memories": gate.advisory, "influence_decisions": gate.decisions, "event_times": event_times });
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
        let namespace = namespace_arg(args);

        let memories = self
            .hybrid_in_namespace(&namespace, None, query, limit, semantic)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let gate = self
            .gate_memories(
                memories,
                &namespace,
                "*",
                args,
                crate::egress::ConsumerCapabilities {
                    reasoning_only_channel: true,
                    exact_source_expansion: false,
                    denial_diagnostics: false,
                },
            )
            .await?;
        let event_times = self.event_times_map(&gate.authorized).await;
        let json = serde_json::json!({ "memories": gate.authorized, "advisory_memories": gate.advisory, "influence_decisions": gate.decisions, "event_times": event_times });
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
        let namespace = namespace_arg(args);
        let gate = self
            .gate_memories(
                memories,
                &namespace,
                project,
                args,
                crate::egress::ConsumerCapabilities {
                    reasoning_only_channel: false,
                    exact_source_expansion: false,
                    denial_diagnostics: false,
                },
            )
            .await?;
        let memories = gate.authorized;

        hooks::write_ironmem_file(project, &memories)
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        hooks::ensure_claude_md_import(project)
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        let json = serde_json::json!({
            "injected": memories.len(),
            "project": project,
            "denied_memory_ids": gate.denied_memory_ids,
            "influence_decisions": gate.decisions,
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
        // Dual-naming: the caller-supplied event/valid time (when the thing
        // described happened), distinct from created_at (when we stored it).
        // Validate up front against the same ISO date/range format the
        // compressor's WHEN: extraction uses, so an invalid value never creates
        // an orphan memory.
        let event_at = args
            .get("event_at")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());
        if let Some(when) = event_at {
            if !crate::provider::is_valid_memory_date_or_range(when) {
                return Ok(error_result(
                    "'event_at' must be an ISO date YYYY-MM-DD or range YYYY-MM-DD..YYYY-MM-DD",
                ));
            }
        }
        let governance = crate::governance::MemoryGovernance {
            namespace: crate::governance::normalize_namespace(
                args.get("namespace")
                    .and_then(|v| v.as_str())
                    .unwrap_or(crate::governance::DEFAULT_NAMESPACE),
            ),
            source_type: crate::governance::parse_source_type(
                args.get("source_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("user_input"),
            ),
            trust_tier: crate::governance::parse_trust_tier(
                args.get("trust_tier")
                    .and_then(|v| v.as_str())
                    .unwrap_or("high"),
            ),
            writer_identity: args
                .get("writer_identity")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .or_else(|| Some("ironmem:mcp".to_string())),
            source_ref: args
                .get("source_ref")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            parent_memory_id: None,
            classification: crate::governance::parse_classification(
                args.get("classification")
                    .and_then(|v| v.as_str())
                    .unwrap_or("internal"),
            ),
            consent_state: args
                .get("consent_state")
                .and_then(|v| v.as_str())
                .and_then(crate::governance::parse_consent_state),
            residency: args
                .get("residency")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            retention_policy_id: args
                .get("retention_policy_id")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            expires_at: args.get("expires_at").and_then(|v| v.as_i64()),
            legal_hold: args
                .get("legal_hold")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        };
        let namespace = crate::governance::normalize_namespace(&governance.namespace);

        let memory_id = compress::remember_with_governance(
            &self.db,
            self.embedder.as_deref(),
            self.store.as_ref(),
            project,
            scope,
            kind,
            text,
            tags,
            governance,
        )
        .await
        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        // Stamp the event/valid time after creation (validated above). Reuses
        // the same metadata path the compressor uses for WHEN: extraction.
        if let Some(when) = event_at {
            db::set_memory_event_time(&self.db, memory_id, when)
                .await
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        }

        let json = serde_json::json!({
            "ok": true,
            "memory_id": memory_id,
            "namespace": namespace,
            "scope": db::clamp_scope(scope),
            "kind": db::clamp_kind(kind),
            "event_at": event_at,
        });
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&json).unwrap(),
        )]))
    }

    async fn handle_get_memory_influence(
        &self,
        args: &JsonObject,
    ) -> Result<CallToolResult, ErrorData> {
        let memory_id = args
            .get("memory_id")
            .and_then(|value| value.as_i64())
            .ok_or_else(|| ErrorData::invalid_params("missing 'memory_id'", None))?;
        let namespace = namespace_arg(args);
        match crate::influence::get_memory_policy(
            &self.db,
            &self.policy_principal,
            memory_id,
            &namespace,
        )
        .await
        {
            Ok(policy) => {
                let value = serde_json::json!({ "ok": true, "record": policy });
                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string_pretty(&value).unwrap(),
                )]))
            }
            Err(error) => Ok(policy_tool_error(error)),
        }
    }

    async fn handle_set_memory_influence(
        &self,
        args: &JsonObject,
    ) -> Result<CallToolResult, ErrorData> {
        let memory_id = args
            .get("memory_id")
            .and_then(|value| value.as_i64())
            .ok_or_else(|| ErrorData::invalid_params("missing 'memory_id'", None))?;
        let expected_version = args
            .get("expected_version")
            .and_then(|value| value.as_u64())
            .ok_or_else(|| ErrorData::invalid_params("missing 'expected_version'", None))?;
        let reason = args
            .get("reason")
            .and_then(|value| value.as_str())
            .ok_or_else(|| ErrorData::invalid_params("missing 'reason'", None))?;
        let state = args
            .get("state")
            .and_then(|value| value.as_str())
            .map(str::parse)
            .transpose()
            .map_err(|error: crate::influence::PolicyError| {
                ErrorData::invalid_params(error.to_string(), None)
            })?;
        let maximum_action_risk = args
            .get("maximum_action_risk")
            .and_then(|value| value.as_str())
            .map(str::parse)
            .transpose()
            .map_err(|error: crate::influence::PolicyError| {
                ErrorData::invalid_params(error.to_string(), None)
            })?;
        let maximum_derivation_depth = args
            .get("maximum_derivation_depth")
            .and_then(|value| value.as_u64())
            .map(|value| {
                u32::try_from(value).map_err(|_| {
                    ErrorData::invalid_params("'maximum_derivation_depth' exceeds u32 range", None)
                })
            })
            .transpose()?;
        let clear_depth = args
            .get("clear_maximum_derivation_depth")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        if clear_depth && maximum_derivation_depth.is_some() {
            return Err(ErrorData::invalid_params(
                "'maximum_derivation_depth' and 'clear_maximum_derivation_depth' are mutually exclusive",
                None,
            ));
        }
        let namespace = namespace_arg(args);
        let request = crate::influence::PolicyMutationRequest {
            expected_version,
            patch: crate::influence::MemoryInfluencePolicyPatch {
                state,
                allowed_task_types: string_array_arg(args, "allowed_task_types")?,
                denied_task_types: string_array_arg(args, "denied_task_types")?,
                maximum_action_risk,
                requires_original_source: args
                    .get("requires_original_source")
                    .and_then(|value| value.as_bool()),
                requires_human_confirmation: args
                    .get("requires_human_confirmation")
                    .and_then(|value| value.as_bool()),
                maximum_derivation_depth: if clear_depth {
                    Some(None)
                } else {
                    maximum_derivation_depth.map(Some)
                },
            },
            reason: reason.to_string(),
            request_id: args
                .get("request_id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| format!("mcp-policy-{}", uuid::Uuid::new_v4())),
        };
        match crate::influence::update_memory_policy(
            &self.db,
            &self.policy_principal,
            memory_id,
            &namespace,
            &request,
        )
        .await
        {
            Ok(policy) => {
                let value = serde_json::json!({ "ok": true, "record": policy });
                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string_pretty(&value).unwrap(),
                )]))
            }
            Err(error) => Ok(policy_tool_error(error)),
        }
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

    async fn handle_dream_memory(&self, args: &JsonObject) -> Result<CallToolResult, ErrorData> {
        let project = args.get("project").and_then(|v| v.as_str());
        let dry_run = args
            .get("dry_run")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let apply = args.get("apply").and_then(|v| v.as_bool()).unwrap_or(false);
        let limit = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(200);
        let report = crate::reflection::run(
            &self.db,
            self.embedder.as_deref(),
            self.store.as_ref(),
            project,
            dry_run,
            apply,
            limit.max(1),
        )
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
        let mut count = 0_u64;
        for id in ids {
            match db::governed_delete_memory(
                &self.db,
                id,
                Some("ironmem:mcp"),
                Some("project wipe"),
            )
            .await
            {
                Ok(true) => {
                    count += 1;
                    if let Err(e) =
                        vectorstore::purge_memory(&self.db, self.store.as_ref(), id).await
                    {
                        tracing::warn!("vector/meta cleanup failed for memory {id}: {e}");
                    }
                }
                Ok(false) => {}
                Err(e) => tracing::warn!("governed wipe failed for memory {id}: {e}"),
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
    #[allow(dead_code)]
    async fn hybrid(
        &self,
        project: Option<&str>,
        query: &str,
        limit: i64,
        semantic: bool,
    ) -> anyhow::Result<Vec<Memory>> {
        self.hybrid_in_namespace(
            crate::governance::DEFAULT_NAMESPACE,
            project,
            query,
            limit,
            semantic,
        )
        .await
    }

    async fn hybrid_in_namespace(
        &self,
        namespace: &str,
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
        retrieval::hybrid_search_in_namespace(
            &self.db,
            embedder,
            self.store.as_ref(),
            namespace,
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

fn namespace_arg(args: &JsonObject) -> String {
    crate::governance::normalize_namespace(
        args.get("namespace")
            .and_then(|v| v.as_str())
            .unwrap_or(crate::governance::DEFAULT_NAMESPACE),
    )
}

fn policy_tool_error(error: anyhow::Error) -> CallToolResult {
    let value = if let Some(policy_error) = crate::influence::policy_error(&error) {
        serde_json::json!({
            "ok": false,
            "error": {
                "code": policy_error.code(),
                "message": policy_error.to_string(),
                "current_version": policy_error.current_version(),
            }
        })
    } else {
        serde_json::json!({
            "ok": false,
            "error": {
                "code": "influence_policy_storage_error",
                "message": error.to_string(),
            }
        })
    };
    CallToolResult::error(vec![Content::text(
        serde_json::to_string_pretty(&value).unwrap(),
    )])
}

fn string_array_arg(
    args: &JsonObject,
    field: &str,
) -> std::result::Result<Option<Vec<String>>, ErrorData> {
    let Some(value) = args.get(field) else {
        return Ok(None);
    };
    let values = value
        .as_array()
        .ok_or_else(|| ErrorData::invalid_params(format!("'{field}' must be an array"), None))?;
    values
        .iter()
        .map(|value| {
            value.as_str().map(str::to_string).ok_or_else(|| {
                ErrorData::invalid_params(format!("every '{field}' entry must be a string"), None)
            })
        })
        .collect::<std::result::Result<Vec<_>, _>>()
        .map(Some)
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
            "get_memory_influence" => self.handle_get_memory_influence(&args).await,
            "set_memory_influence" => self.handle_set_memory_influence(&args).await,
            "get_profile" => self.handle_get_profile().await,
            "refresh_profile" => self.handle_refresh_profile().await,
            "list_corrections" => self.handle_list_corrections(&args).await,
            "memory_graph" => self.handle_memory_graph(&args).await,
            "reconcile_memory_graph" => self.handle_reconcile_memory_graph(&args).await,
            "dream_memory" => self.handle_dream_memory(&args).await,
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
        policy_principal: crate::influence::PolicyPrincipal::local_operator("ironmem:mcp:stdio"),
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
    let policy_principal = crate::influence::PolicyPrincipal::configured(
        "ironmem:mcp:http",
        "shared_mcp",
        config.mcp_namespaces.clone(),
        config.mcp_capabilities.clone(),
    );
    let server = IronMemServer {
        db,
        config: Arc::new(config),
        embedder,
        store,
        policy_principal,
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
                policy_principal: crate::influence::PolicyPrincipal::local_operator(
                    "ironmem:mcp:test",
                ),
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

    #[test]
    fn tool_list_includes_dream_memory() {
        let tools = IronMemServer::build_tool_list();
        let t = tools
            .iter()
            .find(|t| t.name.as_ref() == "dream_memory")
            .expect("dream_memory tool registered");
        let v = serde_json::to_value(t).unwrap();
        let props = &v["inputSchema"]["properties"];
        assert!(props.get("dry_run").is_some(), "schema has dry_run");
        assert!(props.get("apply").is_some(), "schema has apply");
    }

    #[test]
    fn tool_list_includes_influence_policy_crud() {
        let tools = IronMemServer::build_tool_list();
        let get_policy = tools
            .iter()
            .find(|tool| tool.name.as_ref() == "get_memory_influence")
            .expect("get_memory_influence tool registered");
        let set_policy = tools
            .iter()
            .find(|tool| tool.name.as_ref() == "set_memory_influence")
            .expect("set_memory_influence tool registered");
        let get_schema = serde_json::to_value(get_policy).unwrap();
        let set_schema = serde_json::to_value(set_policy).unwrap();
        assert!(get_schema["inputSchema"]["properties"]["memory_id"].is_object());
        assert!(set_schema["inputSchema"]["properties"]["expected_version"].is_object());
        assert!(set_schema["inputSchema"]["properties"]["reason"].is_object());
    }

    #[tokio::test]
    async fn influence_policy_tools_share_versioned_capability_checked_handlers() {
        let (server, path) = test_server().await;
        let session = db::create_session(&server.db, "/tmp/mcp-policy")
            .await
            .unwrap();
        let memory_id = db::insert_memory(
            &server.db,
            "/tmp/mcp-policy",
            &session,
            "MCP policy memory",
            None,
        )
        .await
        .unwrap();

        let mut get_args = JsonObject::new();
        get_args.insert("memory_id".into(), serde_json::json!(memory_id));
        get_args.insert("namespace".into(), serde_json::json!("local"));
        let initial: serde_json::Value = serde_json::from_str(&result_text(
            &server.handle_get_memory_influence(&get_args).await.unwrap(),
        ))
        .unwrap();
        assert_eq!(initial["ok"], true);
        assert_eq!(initial["record"]["policy"]["version"], 1);
        assert_eq!(initial["record"]["explicit"], false);

        let mut set_args = JsonObject::new();
        set_args.insert("memory_id".into(), serde_json::json!(memory_id));
        set_args.insert("namespace".into(), serde_json::json!("local"));
        set_args.insert("expected_version".into(), serde_json::json!(1));
        set_args.insert("reason".into(), serde_json::json!("MCP policy test"));
        set_args.insert("request_id".into(), serde_json::json!("mcp-policy-test"));
        set_args.insert("state".into(), serde_json::json!("reasoning_only"));
        set_args.insert(
            "allowed_task_types".into(),
            serde_json::json!(["Code Review"]),
        );
        let updated: serde_json::Value = serde_json::from_str(&result_text(
            &server.handle_set_memory_influence(&set_args).await.unwrap(),
        ))
        .unwrap();
        assert_eq!(updated["ok"], true);
        assert_eq!(updated["record"]["policy"]["version"], 2);
        assert_eq!(
            updated["record"]["policy"]["allowed_task_types"][0],
            "code_review"
        );

        let stale_result = server.handle_set_memory_influence(&set_args).await.unwrap();
        assert_eq!(stale_result.is_error, Some(true));
        let stale: serde_json::Value = serde_json::from_str(&result_text(&stale_result)).unwrap();
        assert_eq!(stale["ok"], false);
        assert_eq!(stale["error"]["code"], "policy_version_conflict");
        assert_eq!(stale["error"]["current_version"], 2);

        let mut shared_http = server.clone();
        shared_http.policy_principal = crate::influence::PolicyPrincipal::configured(
            "ironmem:mcp:http",
            "shared_mcp",
            vec!["local".to_string()],
            Vec::new(),
        );
        let denied_result = shared_http
            .handle_get_memory_influence(&get_args)
            .await
            .unwrap();
        assert_eq!(denied_result.is_error, Some(true));
        let denied: serde_json::Value = serde_json::from_str(&result_text(&denied_result)).unwrap();
        assert_eq!(denied["ok"], false);
        assert_eq!(
            denied["error"]["code"],
            "influence_policy_capability_required"
        );

        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn blocked_memory_never_crosses_mcp_context_search_source_or_file_egress() {
        let (mut server, path) = test_server().await;
        Arc::make_mut(&mut server.config).influence.enabled = true;
        let project_dir =
            std::env::temp_dir().join(format!("ironmem-mcp-egress-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&project_dir).unwrap();
        let project = project_dir.to_string_lossy().to_string();
        let session = db::create_session(&server.db, &project).await.unwrap();
        let memory_id = db::insert_memory(
            &server.db,
            &project,
            &session,
            "mcp-blocked-content-marker",
            None,
        )
        .await
        .unwrap();
        crate::influence::update_memory_policy(
            &server.db,
            &crate::influence::PolicyPrincipal::local_operator("test"),
            memory_id,
            "local",
            &crate::influence::PolicyMutationRequest {
                expected_version: 1,
                patch: crate::influence::MemoryInfluencePolicyPatch {
                    state: Some(crate::influence::InfluenceState::Blocked),
                    ..Default::default()
                },
                reason: "egress test".into(),
                request_id: "mcp-egress-policy".into(),
            },
        )
        .await
        .unwrap();

        let mut context_args = JsonObject::new();
        context_args.insert("project".into(), serde_json::json!(project));
        let context = server.handle_get_context(&context_args).await.unwrap();
        assert!(!result_text(&context).contains("mcp-blocked-content-marker"));

        let mut search_args = context_args.clone();
        search_args.insert(
            "query".into(),
            serde_json::json!("mcp-blocked-content-marker"),
        );
        let search = server.handle_search_memories(&search_args).await.unwrap();
        assert!(!result_text(&search).contains("mcp-blocked-content-marker"));

        let mut source_args = JsonObject::new();
        source_args.insert("memory_id".into(), serde_json::json!(memory_id));
        let source = server.handle_retrieve_original(&source_args).await.unwrap();
        assert!(!result_text(&source).contains("mcp-blocked-content-marker"));

        let injected = server.handle_inject_context(&context_args).await.unwrap();
        assert!(!result_text(&injected).contains("mcp-blocked-content-marker"));
        if let Ok(file) = std::fs::read_to_string(project_dir.join("IRONMEM.md")) {
            assert!(!file.contains("mcp-blocked-content-marker"));
        }

        let _ = std::fs::remove_dir_all(project_dir);
        let _ = std::fs::remove_file(path);
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

    #[test]
    fn tool_list_remember_has_event_at() {
        let tools = IronMemServer::build_tool_list();
        let t = tools
            .iter()
            .find(|t| t.name.as_ref() == "remember")
            .expect("remember tool registered");
        let v = serde_json::to_value(t).unwrap();
        assert!(
            v["inputSchema"]["properties"].get("event_at").is_some(),
            "remember schema exposes event_at"
        );
    }

    #[tokio::test]
    async fn remember_accepts_and_surfaces_event_at() {
        let (server, path) = test_server().await;

        // Dual-naming write path: store with an explicit event date (valid
        // time), distinct from created_at (when we stored it).
        let mut args = JsonObject::new();
        args.insert("project".into(), serde_json::json!("/tmp/projE"));
        args.insert("text".into(), serde_json::json!("the acquisition closed"));
        args.insert("event_at".into(), serde_json::json!("2023-05-07"));
        let v: serde_json::Value =
            serde_json::from_str(&result_text(&server.handle_remember(&args).await.unwrap()))
                .unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["event_at"], "2023-05-07");
        let mid = v["memory_id"].as_i64().unwrap();

        // Surfaced back through a read tool as the event_times side map.
        let mut q = JsonObject::new();
        q.insert("project".into(), serde_json::json!("/tmp/projE"));
        let read: serde_json::Value = serde_json::from_str(&result_text(
            &server.handle_list_memories(&q).await.unwrap(),
        ))
        .unwrap();
        assert_eq!(
            read["event_times"][mid.to_string().as_str()],
            "2023-05-07",
            "event date surfaces on the read tool"
        );

        // An invalid event_at is rejected gracefully (ok:false, not a protocol error).
        let mut bad = JsonObject::new();
        bad.insert("project".into(), serde_json::json!("/tmp/projE"));
        bad.insert("text".into(), serde_json::json!("undated note"));
        bad.insert("event_at".into(), serde_json::json!("last Friday"));
        let vbad: serde_json::Value =
            serde_json::from_str(&result_text(&server.handle_remember(&bad).await.unwrap()))
                .unwrap();
        assert_eq!(vbad["ok"], false);

        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn derived_memories_are_quarantined_from_retrieval() {
        let (server, path) = test_server().await;
        let proj = "/tmp/projQ";

        // A normal memory and a derived (kind="inference") memory share a term.
        let mut a = JsonObject::new();
        a.insert("project".into(), serde_json::json!(proj));
        a.insert(
            "text".into(),
            serde_json::json!("zorptamine lowers blood pressure"),
        );
        let va: serde_json::Value =
            serde_json::from_str(&result_text(&server.handle_remember(&a).await.unwrap())).unwrap();
        let normal_id = va["memory_id"].as_i64().unwrap();

        let mut b = JsonObject::new();
        b.insert("project".into(), serde_json::json!(proj));
        b.insert(
            "text".into(),
            serde_json::json!("zorptamine therefore cures hypertension"),
        );
        b.insert("kind".into(), serde_json::json!("inference"));
        let vb: serde_json::Value =
            serde_json::from_str(&result_text(&server.handle_remember(&b).await.unwrap())).unwrap();
        // The new kind survives clamp_kind (else it would collapse to "session").
        assert_eq!(vb["kind"], "inference");
        let derived_id = vb["memory_id"].as_i64().unwrap();

        // Default retrieval surfaces the normal memory but quarantines the derived one.
        let mut q = JsonObject::new();
        q.insert("project".into(), serde_json::json!(proj));
        q.insert("query".into(), serde_json::json!("zorptamine"));
        let res: serde_json::Value = serde_json::from_str(&result_text(
            &server.handle_search_memories(&q).await.unwrap(),
        ))
        .unwrap();
        let ids: Vec<i64> = res["memories"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|m| m["id"].as_i64())
            .collect();
        assert!(ids.contains(&normal_id), "normal memory is retrievable");
        assert!(
            !ids.contains(&derived_id),
            "derived (inference) memory is quarantined from default retrieval"
        );

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
            policy_principal: crate::influence::PolicyPrincipal::local_operator("ironmem:mcp:test"),
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
        db::set_memory_scope_kind(&server.db, mem_id, "project", "fact")
            .await
            .unwrap();
        db::set_memory_event_time(&server.db, mem_id, "2024-03-10")
            .await
            .unwrap();
        db::insert_memory_edge(
            &server.db,
            &db::NewMemoryEdge {
                project: "/tmp/p".to_string(),
                memory_id: mem_id,
                source: "Alice".to_string(),
                relation: "visited".to_string(),
                target: "Austin".to_string(),
                valid_from: Some("2024-03-10".to_string()),
                valid_until: None,
                confidence: 0.92,
            },
        )
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
        assert_eq!(
            context["evidence_chains"][0]["memory_id"].as_i64(),
            Some(mem_id)
        );
        assert_eq!(context["evidence_chains"][0]["kind"].as_str(), Some("fact"));
        assert_eq!(
            context["evidence_chains"][0]["event_time"].as_str(),
            Some("2024-03-10")
        );
        assert_eq!(
            context["evidence_chains"][0]["chunks"][0]["chunk_id"]
                .as_str()
                .unwrap(),
            format!("mem:{mem_id}:overview")
        );
        assert_eq!(
            context["evidence_chains"][0]["graph_edges"][0]["relation"].as_str(),
            Some("visited")
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
