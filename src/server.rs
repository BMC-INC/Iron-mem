use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{Html, Json},
    routing::{delete, get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::embedder::Embedder;
use crate::vectorstore::{self, VectorStore};
use crate::{compress, config::Config, db, retrieval};

#[derive(Clone)]
pub struct AppState {
    pub db: db::Database,
    pub config: Config,
    pub embedder: Option<Arc<dyn Embedder>>,
    pub store: Arc<dyn VectorStore>,
}

impl AppState {
    /// Hybrid (keyword + semantic) search using this server's embedder + store.
    #[allow(dead_code)]
    async fn hybrid(&self, project: Option<&str>, query: &str, limit: i64) -> Vec<db::Memory> {
        self.hybrid_in_namespace(crate::governance::DEFAULT_NAMESPACE, project, query, limit)
            .await
    }

    async fn hybrid_in_namespace(
        &self,
        namespace: &str,
        project: Option<&str>,
        query: &str,
        limit: i64,
    ) -> Vec<db::Memory> {
        retrieval::hybrid_search_in_namespace(
            &self.db,
            self.embedder.as_deref(),
            self.store.as_ref(),
            namespace,
            project,
            query,
            limit as usize,
        )
        .await
        .unwrap_or_default()
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/session/start", post(session_start))
        .route("/session/end", post(session_end))
        .route("/event", post(record_event))
        .route("/compress", post(compress_session))
        .route("/context", get(get_context))
        .route("/skim", get(get_skim))
        .route("/status", get(get_status))
        .route("/retrieve_original", post(retrieve_original))
        .route("/remember", post(remember))
        .route("/profile", get(get_profile))
        .route("/refresh_profile", post(refresh_profile))
        .route("/corrections", get(list_corrections))
        .route("/graph", get(memory_graph))
        .route(
            "/graph/{id}",
            post(update_graph_edge).delete(delete_graph_edge),
        )
        .route("/feedback", post(record_feedback))
        .route("/reflect", post(run_reflection))
        .route("/dream", post(run_dream))
        .route("/code/relink", post(code_relink))
        .route("/snapshots", get(list_snapshots).post(create_snapshot))
        .route("/snapshots/{id}/restore", post(restore_snapshot))
        .route(
            "/sync/events",
            get(export_sync_events).post(publish_sync_event),
        )
        // Web UI routes
        .route("/ui", get(web_ui))
        .route("/api/projects", get(api_list_projects))
        .route("/api/memories", get(api_list_memories))
        .route("/api/memories/{id}", delete(api_delete_memory))
        .route("/api/sessions", get(api_list_sessions))
        .with_state(Arc::new(state))
}

// POST /session/start
#[derive(Deserialize)]
pub struct SessionStartRequest {
    pub project: String,
}

#[derive(Serialize)]
pub struct SessionStartResponse {
    pub session_id: String,
}

async fn session_start(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SessionStartRequest>,
) -> Result<Json<SessionStartResponse>, (StatusCode, String)> {
    let session_id = db::create_session(&state.db, &body.project)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    tracing::info!("Session started: {} project={}", session_id, body.project);
    Ok(Json(SessionStartResponse { session_id }))
}

// POST /session/end
#[derive(Deserialize)]
pub struct SessionEndRequest {
    pub session_id: String,
}

#[derive(Serialize)]
pub struct SessionEndResponse {
    pub ok: bool,
    pub memory_id: Option<i64>,
    pub skipped: bool,
    pub reason: Option<String>,
}

async fn session_end(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SessionEndRequest>,
) -> Result<Json<SessionEndResponse>, (StatusCode, String)> {
    db::end_session(&state.db, &body.session_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Check if there are any observations worth compressing
    let count = db::observation_count_for_session(&state.db, &body.session_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if count == 0 {
        tracing::info!(
            "Session {} ended with no observations, skipping compression",
            body.session_id
        );
        return Ok(Json(SessionEndResponse {
            ok: true,
            memory_id: None,
            skipped: true,
            reason: Some("No tool calls recorded".to_string()),
        }));
    }

    // Trigger compression
    let memory_id = run_compression(&state, &body.session_id).await;

    match memory_id {
        Ok(id) => Ok(Json(SessionEndResponse {
            ok: true,
            memory_id: Some(id),
            skipped: false,
            reason: None,
        })),
        Err(e) => {
            tracing::warn!("Compression failed for session {}: {}", body.session_id, e);
            Ok(Json(SessionEndResponse {
                ok: false,
                memory_id: None,
                skipped: true,
                reason: Some(format!("Compression failed: {}", e)),
            }))
        }
    }
}

// POST /event
#[derive(Deserialize)]
pub struct EventRequest {
    pub session_id: String,
    pub project: String,
    pub tool: String,
    pub input: Option<String>,
    pub output: Option<String>,
}

#[derive(Serialize)]
pub struct EventResponse {
    pub id: i64,
}

async fn record_event(
    State(state): State<Arc<AppState>>,
    Json(body): Json<EventRequest>,
) -> Result<Json<EventResponse>, (StatusCode, String)> {
    let id = db::insert_observation(
        &state.db,
        &body.session_id,
        &body.project,
        &body.tool,
        body.input.as_deref(),
        body.output.as_deref(),
        state.config.max_observation_bytes,
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(EventResponse { id }))
}

// POST /retrieve_original  (CCR: pull back the verbatim original)
#[derive(Deserialize)]
pub struct RetrieveOriginalRequest {
    pub observation_id: Option<i64>,
    pub memory_id: Option<i64>,
    pub hash: Option<String>,
    pub chunk_id: Option<String>,
}

#[derive(Serialize)]
pub struct RetrieveOriginalResponse {
    pub hash: Option<String>,
    pub bytes: usize,
    pub original: String,
    pub chunk_id: Option<String>,
    pub memory_id: Option<i64>,
    pub source_start: Option<i64>,
    pub source_end: Option<i64>,
}

async fn retrieve_original(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RetrieveOriginalRequest>,
) -> Result<Json<RetrieveOriginalResponse>, (StatusCode, String)> {
    let expanded = crate::expansion::retrieve_original(
        &state.db,
        body.observation_id,
        body.memory_id,
        body.hash.as_deref(),
        body.chunk_id.as_deref(),
    )
    .await
    .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?;

    Ok(Json(RetrieveOriginalResponse {
        hash: expanded.hash,
        bytes: expanded.bytes,
        original: expanded.original,
        chunk_id: expanded.chunk_id,
        memory_id: expanded.memory_id,
        source_start: expanded.source_start,
        source_end: expanded.source_end,
    }))
}

// POST /remember  (Supermemory: store an explicit typed/scoped memory)
#[derive(Deserialize)]
pub struct RememberRequest {
    pub project: String,
    pub text: String,
    pub scope: Option<String>,
    pub kind: Option<String>,
    pub tags: Option<String>,
    pub namespace: Option<String>,
    pub source_type: Option<String>,
    pub trust_tier: Option<String>,
    pub writer_identity: Option<String>,
    pub classification: Option<String>,
    pub consent_state: Option<String>,
    pub residency: Option<String>,
    pub retention_policy_id: Option<String>,
    pub expires_at: Option<i64>,
    pub legal_hold: Option<bool>,
    pub source_ref: Option<String>,
}

#[derive(Serialize)]
pub struct RememberResponse {
    pub memory_id: i64,
    pub namespace: String,
    pub scope: String,
    pub kind: String,
}

async fn remember(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RememberRequest>,
) -> Result<Json<RememberResponse>, (StatusCode, String)> {
    if body.text.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "'text' must not be empty".to_string(),
        ));
    }
    let scope = body.scope.as_deref().unwrap_or("project");
    let kind = body.kind.as_deref().unwrap_or("preference");
    let governance = crate::governance::MemoryGovernance {
        namespace: crate::governance::normalize_namespace(
            body.namespace
                .as_deref()
                .unwrap_or(crate::governance::DEFAULT_NAMESPACE),
        ),
        source_type: crate::governance::parse_source_type(
            body.source_type.as_deref().unwrap_or("user_input"),
        ),
        trust_tier: crate::governance::parse_trust_tier(
            body.trust_tier.as_deref().unwrap_or("high"),
        ),
        writer_identity: body
            .writer_identity
            .as_deref()
            .map(str::to_string)
            .or_else(|| Some("ironmem:rest".to_string())),
        source_ref: body.source_ref.clone(),
        parent_memory_id: None,
        classification: crate::governance::parse_classification(
            body.classification.as_deref().unwrap_or("internal"),
        ),
        consent_state: body
            .consent_state
            .as_deref()
            .and_then(crate::governance::parse_consent_state),
        residency: body.residency.clone(),
        retention_policy_id: body.retention_policy_id.clone(),
        expires_at: body.expires_at,
        legal_hold: body.legal_hold.unwrap_or(false),
    };
    let namespace = crate::governance::normalize_namespace(&governance.namespace);

    let memory_id = compress::remember_with_governance(
        &state.db,
        state.embedder.as_deref(),
        state.store.as_ref(),
        &body.project,
        scope,
        kind,
        &body.text,
        body.tags.as_deref(),
        governance,
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(RememberResponse {
        memory_id,
        namespace,
        scope: db::clamp_scope(scope).to_string(),
        kind: db::clamp_kind(kind).to_string(),
    }))
}

// GET /profile  +  POST /refresh_profile  (Supermemory user profile)
#[derive(Serialize)]
pub struct ProfileResponse {
    pub profile: Option<db::Memory>,
}

async fn get_profile(
    State(state): State<Arc<AppState>>,
) -> Result<Json<ProfileResponse>, (StatusCode, String)> {
    let profile = db::get_profile_memory(&state.db)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(ProfileResponse { profile }))
}

#[derive(Serialize)]
pub struct RefreshProfileResponse {
    pub regenerated: bool,
    pub profile: Option<db::Memory>,
}

async fn refresh_profile(
    State(state): State<Arc<AppState>>,
) -> Result<Json<RefreshProfileResponse>, (StatusCode, String)> {
    let id = crate::profile::regenerate(
        &state.db,
        state.embedder.as_deref(),
        state.store.as_ref(),
        Some(&state.config),
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let profile = db::get_profile_memory(&state.db)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(RefreshProfileResponse {
        regenerated: id.is_some(),
        profile,
    }))
}

// GET /corrections?project=&limit=  (mined error_solution memories)
#[derive(Deserialize)]
pub struct CorrectionsQuery {
    pub project: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Serialize)]
pub struct CorrectionsResponse {
    pub corrections: Vec<db::Memory>,
}

async fn list_corrections(
    State(state): State<Arc<AppState>>,
    Query(params): Query<CorrectionsQuery>,
) -> Result<Json<CorrectionsResponse>, (StatusCode, String)> {
    let limit = params.limit.unwrap_or(10);
    let corrections = db::get_memories_by_kind(
        &state.db,
        params.project.as_deref(),
        "error_solution",
        limit,
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(CorrectionsResponse { corrections }))
}

// GET /graph?entity=&project=&history=&at=&limit=  (temporal memory graph)
#[derive(Deserialize)]
pub struct GraphQuery {
    pub entity: String,
    pub project: Option<String>,
    pub history: Option<bool>,
    pub at: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Serialize)]
pub struct GraphResponse {
    pub entity: String,
    pub project: Option<String>,
    pub include_superseded: bool,
    pub at: Option<String>,
    pub edges: Vec<db::MemoryEdge>,
}

async fn memory_graph(
    State(state): State<Arc<AppState>>,
    Query(params): Query<GraphQuery>,
) -> Result<Json<GraphResponse>, (StatusCode, String)> {
    if params.entity.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "'entity' must not be empty".to_string(),
        ));
    }
    let include_superseded = params.history.unwrap_or(false);
    if let Some(at) = &params.at {
        if !crate::provider::is_valid_memory_date(at) {
            return Err((
                StatusCode::BAD_REQUEST,
                "'at' must be a valid YYYY-MM-DD date".to_string(),
            ));
        }
    }
    let edges = db::memory_edges_for_entity_at(
        &state.db,
        params.project.as_deref(),
        &params.entity,
        include_superseded,
        params.at.as_deref(),
        params.limit.unwrap_or(20).max(1),
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(GraphResponse {
        entity: params.entity,
        project: params.project,
        include_superseded,
        at: params.at,
        edges,
    }))
}

#[derive(Deserialize)]
pub struct GraphEdgeUpdateRequest {
    pub source: String,
    pub relation: String,
    pub target: String,
    pub valid_from: Option<String>,
    pub valid_until: Option<String>,
    pub confidence: Option<f64>,
}

async fn update_graph_edge(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Json(body): Json<GraphEdgeUpdateRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    for value in [body.valid_from.as_deref(), body.valid_until.as_deref()]
        .into_iter()
        .flatten()
    {
        if !crate::provider::is_valid_memory_date(value) {
            return Err((
                StatusCode::BAD_REQUEST,
                "valid_from/valid_until must be YYYY-MM-DD".to_string(),
            ));
        }
    }
    let updated = db::curate_memory_edge_update(
        &state.db,
        id,
        &body.source,
        &body.relation,
        &body.target,
        body.valid_from.as_deref(),
        body.valid_until.as_deref(),
        body.confidence.unwrap_or(1.0),
    )
    .await
    .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    Ok(Json(serde_json::json!({ "updated": updated })))
}

async fn delete_graph_edge(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let deleted = db::curate_memory_edge_delete(&state.db, id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(serde_json::json!({ "deleted": deleted })))
}

#[derive(Deserialize)]
pub struct FeedbackRequest {
    pub memory_id: i64,
    pub project: String,
    pub signal: String,
    pub weight: Option<f64>,
    pub detail: Option<String>,
}

async fn record_feedback(
    State(state): State<Arc<AppState>>,
    Json(body): Json<FeedbackRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let id = db::record_memory_feedback(
        &state.db,
        body.memory_id,
        &body.project,
        &body.signal,
        body.weight.unwrap_or(1.0),
        body.detail.as_deref(),
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(serde_json::json!({ "id": id })))
}

#[derive(Deserialize)]
pub struct ReflectionRequest {
    pub project: Option<String>,
    pub dry_run: Option<bool>,
    pub apply: Option<bool>,
    pub limit: Option<i64>,
}

async fn run_reflection(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ReflectionRequest>,
) -> Result<Json<crate::reflection::ReflectionReport>, (StatusCode, String)> {
    let report = crate::reflection::run(
        &state.db,
        state.embedder.as_deref(),
        state.store.as_ref(),
        body.project.as_deref(),
        body.dry_run.unwrap_or(true),
        body.apply.unwrap_or(false),
        body.limit.unwrap_or(200),
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(report))
}

async fn run_dream(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ReflectionRequest>,
) -> Result<Json<crate::reflection::ReflectionReport>, (StatusCode, String)> {
    let report = crate::reflection::run(
        &state.db,
        state.embedder.as_deref(),
        state.store.as_ref(),
        body.project.as_deref(),
        body.dry_run.unwrap_or(true),
        body.apply.unwrap_or(false),
        body.limit.unwrap_or(200),
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(report))
}

#[derive(Deserialize)]
pub struct CodeRelinkRequest {
    pub project: String,
    pub dry_run: Option<bool>,
    pub anchor_only: Option<bool>,
    pub relink_only: Option<bool>,
}

async fn code_relink(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CodeRelinkRequest>,
) -> Result<Json<crate::code_anchor::CodeRelinkReport>, (StatusCode, String)> {
    let dry_run = body.dry_run.unwrap_or(true);
    let mut combined = crate::code_anchor::CodeRelinkReport {
        dry_run,
        ..Default::default()
    };
    if !body.relink_only.unwrap_or(false) {
        let r = crate::code_anchor::anchor_project(&state.db, &body.project, dry_run)
            .await
            .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
        combined.scanned_symbols = combined.scanned_symbols.max(r.scanned_symbols);
        combined.anchors_created += r.anchors_created;
    }
    if !body.anchor_only.unwrap_or(false) {
        let r = crate::code_anchor::relink_project(&state.db, &body.project, dry_run)
            .await
            .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
        combined.scanned_symbols = combined.scanned_symbols.max(r.scanned_symbols);
        combined.anchors_relinked += r.anchors_relinked;
    }
    Ok(Json(combined))
}

#[derive(Deserialize)]
pub struct SnapshotCreateRequest {
    pub label: Option<String>,
    pub project: Option<String>,
}

async fn create_snapshot(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SnapshotCreateRequest>,
) -> Result<Json<db::BrainSnapshot>, (StatusCode, String)> {
    let snap = crate::snapshot::create(&state.db, body.label.as_deref(), body.project.as_deref())
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(snap))
}

#[derive(Deserialize)]
pub struct SnapshotListQuery {
    pub limit: Option<i64>,
}

async fn list_snapshots(
    State(state): State<Arc<AppState>>,
    Query(params): Query<SnapshotListQuery>,
) -> Result<Json<Vec<db::BrainSnapshot>>, (StatusCode, String)> {
    let snaps = db::list_brain_snapshots(&state.db, params.limit.unwrap_or(20))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(snaps))
}

#[derive(Deserialize)]
pub struct RestoreSnapshotRequest {
    pub dry_run: Option<bool>,
}

async fn restore_snapshot(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<RestoreSnapshotRequest>,
) -> Result<Json<crate::snapshot::RestoreReport>, (StatusCode, String)> {
    let report = crate::snapshot::restore(&state.db, &id, body.dry_run.unwrap_or(true))
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    Ok(Json(report))
}

#[derive(Deserialize)]
pub struct SyncQuery {
    pub project: Option<String>,
    pub after_lamport: Option<i64>,
    pub limit: Option<i64>,
}

async fn export_sync_events(
    State(state): State<Arc<AppState>>,
    Query(params): Query<SyncQuery>,
) -> Result<Json<Vec<db::SyncEvent>>, (StatusCode, String)> {
    let events = crate::sync::export_events(
        &state.db,
        params.project.as_deref(),
        params.after_lamport.unwrap_or(0),
        params.limit.unwrap_or(100),
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(events))
}

#[derive(Deserialize)]
pub struct SyncPublishRequest {
    pub node_id: String,
    pub project: Option<String>,
    pub op_type: String,
    pub payload: serde_json::Value,
}

async fn publish_sync_event(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SyncPublishRequest>,
) -> Result<Json<crate::sync::PublishResult>, (StatusCode, String)> {
    let payload = crate::sync::SyncPayload {
        kind: body.op_type.clone(),
        memory_id: body.payload.get("memory_id").and_then(|v| v.as_i64()),
        edge_id: body.payload.get("edge_id").and_then(|v| v.as_i64()),
        body: body.payload,
    };
    let result = crate::sync::publish(
        &state.db,
        &body.node_id,
        body.project.as_deref(),
        &body.op_type,
        &payload,
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(result))
}

// POST /compress  (manual trigger)
#[derive(Deserialize)]
pub struct CompressRequest {
    pub session_id: String,
}

#[derive(Serialize)]
pub struct CompressResponse {
    pub memory_id: i64,
}

async fn compress_session(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CompressRequest>,
) -> Result<Json<CompressResponse>, (StatusCode, String)> {
    let memory_id = run_compression(&state, &body.session_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(CompressResponse { memory_id }))
}

// GET /context?project=&limit=&query=
#[derive(Deserialize)]
pub struct ContextQuery {
    pub project: String,
    pub limit: Option<i64>,
    pub query: Option<String>,
    pub namespace: Option<String>,
    /// Per-request override for LLM reranking ("1"/"true"/"yes"/"on"). Absent ⇒
    /// fall back to the server's `rerank.enabled` config default.
    pub rerank: Option<String>,
    /// Optional wide candidate pool for reranking experiments. Final output is
    /// still capped by `limit`; this only controls how many candidates the LLM
    /// may promote from.
    pub pool: Option<usize>,
}

/// Interpret a `?rerank=` value as a boolean opt-in. Accepts the common truthy
/// spellings; `None` (param absent) leaves the decision to the config default.
fn truthy(v: &Option<String>) -> Option<bool> {
    v.as_deref().map(|s| {
        matches!(
            s.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

#[derive(Serialize)]
pub struct ContextResponse {
    pub memories: Vec<db::Memory>,
    pub expansions: Vec<ContextExpansion>,
}

#[derive(Serialize)]
pub struct ContextExpansion {
    pub memory_id: i64,
    pub chunks: Vec<db::MemoryChunk>,
}

async fn get_context(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ContextQuery>,
) -> Result<Json<ContextResponse>, (StatusCode, String)> {
    let limit = params.limit.unwrap_or(state.config.inject_limit as i64);
    let rerank_on = truthy(&params.rerank).unwrap_or(state.config.rerank.enabled);
    let namespace = crate::governance::normalize_namespace(
        params
            .namespace
            .as_deref()
            .unwrap_or(crate::governance::DEFAULT_NAMESPACE),
    );

    let memories = match &params.query {
        Some(q) if !q.is_empty() => {
            if rerank_on {
                // Re-anchored reranked retrieval: protect the base@limit ordering
                // (FTS-strong temporal answers) while letting the LLM promote
                // buried wide-pool answers. Failure falls back to base order.
                retrieval::rerank_search_in_namespace_with_pool(
                    &state.db,
                    state.embedder.as_deref(),
                    state.store.as_ref(),
                    &state.config,
                    &namespace,
                    Some(&params.project),
                    q,
                    limit as usize,
                    params.pool,
                )
                .await
                .unwrap_or_default()
            } else {
                state
                    .hybrid_in_namespace(&namespace, Some(&params.project), q, limit)
                    .await
            }
        }
        _ => db::get_recent_memories_in_namespace(&state.db, &namespace, &params.project, limit)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
    };

    let memory_ids: Vec<i64> = memories.iter().map(|m| m.id).collect();
    let chunks = db::chunks_for_memories(&state.db, &memory_ids)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let expansions = memory_ids
        .into_iter()
        .map(|memory_id| ContextExpansion {
            memory_id,
            chunks: chunks.get(&memory_id).cloned().unwrap_or_default(),
        })
        .collect();

    Ok(Json(ContextResponse {
        memories,
        expansions,
    }))
}

// GET /skim?project=&limit=&global=
#[derive(Deserialize)]
pub struct SkimQuery {
    pub project: Option<String>,
    pub limit: Option<i64>,
    pub global: Option<String>,
    pub namespace: Option<String>,
}

#[derive(Serialize)]
pub struct SkimResponse {
    pub chunks: Vec<db::MemoryChunk>,
}

async fn get_skim(
    State(state): State<Arc<AppState>>,
    Query(params): Query<SkimQuery>,
) -> Result<Json<SkimResponse>, (StatusCode, String)> {
    let limit = params.limit.unwrap_or(state.config.inject_limit as i64 * 3);
    let project = if truthy(&params.global).unwrap_or(false) {
        None
    } else {
        params.project.as_deref()
    };
    let namespace = crate::governance::normalize_namespace(
        params
            .namespace
            .as_deref()
            .unwrap_or(crate::governance::DEFAULT_NAMESPACE),
    );
    let chunks = db::recent_memory_chunks_in_namespace(&state.db, &namespace, project, limit)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(SkimResponse { chunks }))
}

// GET /status
#[derive(Serialize)]
pub struct StatusResponse {
    pub ok: bool,
    pub sessions: i64,
    pub memories: i64,
    pub observations: i64,
    pub memory_edges: i64,
    pub memory_chunks: i64,
    pub db_path: String,
    pub ccr: serde_json::Value,
    /// Per-governance-operation cost (paper RQ5): count / avg_us / max_us.
    pub governance_cost: serde_json::Value,
}

async fn get_status(
    State(state): State<Arc<AppState>>,
) -> Result<Json<StatusResponse>, (StatusCode, String)> {
    let stats = db::get_stats(&state.db)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(StatusResponse {
        ok: true,
        sessions: stats.total_sessions,
        memories: stats.total_memories,
        observations: stats.total_observations,
        memory_edges: stats.total_memory_edges,
        memory_chunks: stats.total_memory_chunks,
        db_path: state.config.db_path.clone(),
        ccr: stats.ccr_json(),
        governance_cost: crate::metrics::snapshot(),
    }))
}

// Shared compression logic
async fn run_compression(state: &AppState, session_id: &str) -> anyhow::Result<i64> {
    compress::run(
        &state.db,
        state.embedder.as_deref(),
        state.store.as_ref(),
        &state.config,
        session_id,
    )
    .await
}

// ── Web UI ──────────────────────────────────────────────────────────

async fn web_ui() -> Html<&'static str> {
    Html(include_str!("web_ui.html"))
}

#[derive(Deserialize)]
pub struct MemoriesQuery {
    pub project: Option<String>,
    pub query: Option<String>,
    pub limit: Option<i64>,
    pub namespace: Option<String>,
}

#[derive(Deserialize)]
pub struct ProjectsQuery {
    pub limit: Option<i64>,
}

async fn api_list_projects(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ProjectsQuery>,
) -> Result<Json<Vec<db::ProjectSummary>>, (StatusCode, String)> {
    let limit = params.limit.unwrap_or(100);
    let projects = db::list_projects(&state.db, limit)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(projects))
}

async fn api_list_memories(
    State(state): State<Arc<AppState>>,
    Query(params): Query<MemoriesQuery>,
) -> Result<Json<Vec<db::Memory>>, (StatusCode, String)> {
    let limit = params.limit.unwrap_or(50);
    let namespace = crate::governance::normalize_namespace(
        params
            .namespace
            .as_deref()
            .unwrap_or(crate::governance::DEFAULT_NAMESPACE),
    );

    let memories = match (&params.project, &params.query) {
        (Some(project), Some(q)) => {
            state
                .hybrid_in_namespace(&namespace, Some(project), q, limit)
                .await
        }
        (Some(project), None) => {
            db::get_recent_memories_in_namespace(&state.db, &namespace, project, limit)
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        }
        (None, Some(q)) => state.hybrid_in_namespace(&namespace, None, q, limit).await,
        (None, None) => db::get_all_memories_in_namespace(&state.db, &namespace, limit)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
    };

    Ok(Json(memories))
}

async fn api_delete_memory(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let deleted =
        db::governed_delete_memory(&state.db, id, Some("ironmem:web-ui"), Some("api delete"))
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if deleted {
        if let Err(e) = vectorstore::purge_memory(&state.db, state.store.as_ref(), id).await {
            tracing::warn!("vector/meta cleanup failed for memory {id}: {e}");
        }
    }

    Ok(Json(serde_json::json!({ "deleted": deleted })))
}

#[derive(Deserialize)]
pub struct SessionsQuery {
    pub project: Option<String>,
    pub limit: Option<i64>,
}

async fn api_list_sessions(
    State(state): State<Arc<AppState>>,
    Query(params): Query<SessionsQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let limit = params.limit.unwrap_or(50);
    if let Some(project) = &params.project {
        let sessions = db::list_session_history(&state.db, project, limit)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        Ok(Json(serde_json::json!(sessions)))
    } else {
        let sessions = db::list_sessions(&state.db, limit)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        Ok(Json(serde_json::json!(sessions)))
    }
}
