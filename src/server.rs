use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::sync::Arc;

use crate::{compress, config::Config, db};

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub config: Config,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/session/start", post(session_start))
        .route("/session/end", post(session_end))
        .route("/event", post(record_event))
        .route("/compress", post(compress_session))
        .route("/context", get(get_context))
        .route("/status", get(get_status))
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
    let session_id = db::create_session(&state.pool, &body.project)
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
    db::end_session(&state.pool, &body.session_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Check if there are any observations worth compressing
    let count = db::observation_count_for_session(&state.pool, &body.session_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if count == 0 {
        tracing::info!("Session {} ended with no observations, skipping compression", body.session_id);
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
        &state.pool,
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

    let memories = if let Some(q) = &params.query {
        if !q.is_empty() {
            db::search_memories(&state.pool, &params.project, q, limit)
                .await
                .unwrap_or_else(|_| vec![])
        } else {
            db::get_recent_memories(&state.pool, &params.project, limit)
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        }
    } else {
        db::get_recent_memories(&state.pool, &params.project, limit)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
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
    pub db_path: String,
}

async fn get_status(
    State(state): State<Arc<AppState>>,
) -> Result<Json<StatusResponse>, (StatusCode, String)> {
    let stats = db::get_stats(&state.pool)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(StatusResponse {
        ok: true,
        sessions: stats.total_sessions,
        memories: stats.total_memories,
        observations: stats.total_observations,
        db_path: state.config.db_path.clone(),
    }))
}

// Shared compression logic
async fn run_compression(state: &AppState, session_id: &str) -> anyhow::Result<i64> {
    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .map_err(|_| anyhow::anyhow!("ANTHROPIC_API_KEY not set"))?;

    let session = db::get_session(&state.pool, session_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Session not found: {}", session_id))?;

    let observations = db::get_observations_for_session(&state.pool, session_id).await?;

    let result = compress::compress_session(&observations, &state.config.model, &api_key).await?;

    let memory_id = db::insert_memory(
        &state.pool,
        &session.project,
        session_id,
        &result.summary,
        Some(&result.tags),
    )
    .await?;

    db::mark_compressed(&state.pool, session_id).await?;

    tracing::info!(
        "Session {} compressed → memory_id={}",
        session_id,
        memory_id
    );

    Ok(memory_id)
}
