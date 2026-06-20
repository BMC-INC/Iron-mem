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
    async fn hybrid(&self, project: Option<&str>, query: &str, limit: i64) -> Vec<db::Memory> {
        retrieval::hybrid_search(
            &self.db,
            self.embedder.as_deref(),
            self.store.as_ref(),
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
        .route("/status", get(get_status))
        .route("/retrieve_original", post(retrieve_original))
        .route("/remember", post(remember))
        .route("/profile", get(get_profile))
        .route("/refresh_profile", post(refresh_profile))
        .route("/corrections", get(list_corrections))
        .route("/graph", get(memory_graph))
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
                ok: true,
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
}

#[derive(Serialize)]
pub struct RetrieveOriginalResponse {
    pub hash: String,
    pub bytes: usize,
    pub original: String,
}

async fn retrieve_original(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RetrieveOriginalRequest>,
) -> Result<Json<RetrieveOriginalResponse>, (StatusCode, String)> {
    let hash = if let Some(h) = body.hash {
        h
    } else if let Some(oid) = body.observation_id {
        db::get_observation_output_blob(&state.db, oid)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
            .ok_or_else(|| {
                (
                    StatusCode::NOT_FOUND,
                    format!("observation {oid} has no stored original"),
                )
            })?
    } else if let Some(mid) = body.memory_id {
        db::get_memory_session_blob(&state.db, mid)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
            .ok_or_else(|| {
                (
                    StatusCode::NOT_FOUND,
                    format!("memory {mid} has no stored session transcript"),
                )
            })?
    } else {
        return Err((
            StatusCode::BAD_REQUEST,
            "provide 'observation_id', 'memory_id', or 'hash'".to_string(),
        ));
    };

    let bytes = crate::ccr::load_blob(&state.db, &hash)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?;

    Ok(Json(RetrieveOriginalResponse {
        hash,
        bytes: bytes.len(),
        original: String::from_utf8_lossy(&bytes).into_owned(),
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
}

#[derive(Serialize)]
pub struct RememberResponse {
    pub memory_id: i64,
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

    let memory_id = compress::remember(
        &state.db,
        state.embedder.as_deref(),
        state.store.as_ref(),
        &body.project,
        scope,
        kind,
        &body.text,
        body.tags.as_deref(),
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(RememberResponse {
        memory_id,
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

// GET /graph?entity=&project=&history=&limit=  (temporal memory graph)
#[derive(Deserialize)]
pub struct GraphQuery {
    pub entity: String,
    pub project: Option<String>,
    pub history: Option<bool>,
    pub limit: Option<usize>,
}

#[derive(Serialize)]
pub struct GraphResponse {
    pub entity: String,
    pub project: Option<String>,
    pub include_superseded: bool,
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
    let edges = db::memory_edges_for_entity(
        &state.db,
        params.project.as_deref(),
        &params.entity,
        include_superseded,
        params.limit.unwrap_or(20).max(1),
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(GraphResponse {
        entity: params.entity,
        project: params.project,
        include_superseded,
        edges,
    }))
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
    /// Per-request override for LLM reranking ("1"/"true"/"yes"/"on"). Absent ⇒
    /// fall back to the server's `rerank.enabled` config default.
    pub rerank: Option<String>,
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
}

async fn get_context(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ContextQuery>,
) -> Result<Json<ContextResponse>, (StatusCode, String)> {
    let limit = params.limit.unwrap_or(state.config.inject_limit as i64);
    let rerank_on = truthy(&params.rerank).unwrap_or(state.config.rerank.enabled);

    let memories = match &params.query {
        Some(q) if !q.is_empty() => {
            if rerank_on {
                // Re-anchored reranked retrieval: protect the base@limit ordering
                // (FTS-strong temporal answers) while letting the LLM promote
                // buried wide-pool answers. Failure falls back to base order.
                retrieval::rerank_search(
                    &state.db,
                    state.embedder.as_deref(),
                    state.store.as_ref(),
                    &state.config,
                    Some(&params.project),
                    q,
                    limit as usize,
                )
                .await
                .unwrap_or_default()
            } else {
                state.hybrid(Some(&params.project), q, limit).await
            }
        }
        _ => db::get_recent_memories(&state.db, &params.project, limit)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
    };

    Ok(Json(ContextResponse { memories }))
}

// GET /status
#[derive(Serialize)]
pub struct StatusResponse {
    pub ok: bool,
    pub sessions: i64,
    pub memories: i64,
    pub observations: i64,
    pub memory_edges: i64,
    pub db_path: String,
    pub ccr: serde_json::Value,
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
        db_path: state.config.db_path.clone(),
        ccr: stats.ccr_json(),
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

    let memories = match (&params.project, &params.query) {
        (Some(project), Some(q)) => state.hybrid(Some(project), q, limit).await,
        (Some(project), None) => db::get_recent_memories(&state.db, project, limit)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
        (None, Some(q)) => state.hybrid(None, q, limit).await,
        (None, None) => db::get_all_memories(&state.db, limit)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
    };

    Ok(Json(memories))
}

async fn api_delete_memory(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let deleted = db::delete_memory(&state.db, id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if deleted {
        // Best-effort: never fail the delete because cleanup hiccuped.
        if let Err(e) = db::decref_memory_session_blob(&state.db, id).await {
            tracing::warn!("CCR decref failed for memory {id}: {e}");
        }
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
