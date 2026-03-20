use anyhow::Result;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Session {
    pub id: String,
    pub project: String,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub compressed: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Observation {
    pub id: i64,
    pub session_id: String,
    pub project: String,
    pub tool: String,
    pub input: Option<String>,
    pub output: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Memory {
    pub id: i64,
    pub project: String,
    pub session_id: String,
    pub summary: String,
    pub tags: Option<String>,
    pub created_at: i64,
}

pub async fn init_db(db_path: &str) -> Result<SqlitePool> {
    // Ensure parent directory exists
    if let Some(parent) = std::path::Path::new(db_path).parent() {
        std::fs::create_dir_all(parent)?;
    }

    let url = format!("sqlite://{}?mode=rwc", db_path);
    let pool = SqlitePool::connect(&url).await?;

    // Create tables
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS sessions (
            id          TEXT PRIMARY KEY,
            project     TEXT NOT NULL,
            started_at  INTEGER NOT NULL,
            ended_at    INTEGER,
            compressed  INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS observations (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id  TEXT NOT NULL REFERENCES sessions(id),
            project     TEXT NOT NULL,
            tool        TEXT NOT NULL,
            input       TEXT,
            output      TEXT,
            created_at  INTEGER NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_obs_session ON observations(session_id);
        CREATE INDEX IF NOT EXISTS idx_obs_project ON observations(project);

        CREATE VIRTUAL TABLE IF NOT EXISTS memories USING fts5(
            project,
            session_id,
            summary,
            tags,
            created_at UNINDEXED,
            tokenize='porter ascii'
        );
        "#,
    )
    .execute(&pool)
    .await?;

    Ok(pool)
}

// Sessions

pub async fn create_session(pool: &SqlitePool, project: &str) -> Result<String> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = Utc::now().timestamp();
    sqlx::query("INSERT INTO sessions (id, project, started_at) VALUES (?, ?, ?)")
        .bind(&id)
        .bind(project)
        .bind(now)
        .execute(pool)
        .await?;
    Ok(id)
}

pub async fn end_session(pool: &SqlitePool, session_id: &str) -> Result<()> {
    let now = Utc::now().timestamp();
    sqlx::query("UPDATE sessions SET ended_at = ? WHERE id = ?")
        .bind(now)
        .bind(session_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn mark_compressed(pool: &SqlitePool, session_id: &str) -> Result<()> {
    sqlx::query("UPDATE sessions SET compressed = 1 WHERE id = ?")
        .bind(session_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn get_session(pool: &SqlitePool, session_id: &str) -> Result<Option<Session>> {
    let row = sqlx::query(
        "SELECT id, project, started_at, ended_at, compressed FROM sessions WHERE id = ?",
    )
    .bind(session_id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|r| Session {
        id: r.get("id"),
        project: r.get("project"),
        started_at: r.get("started_at"),
        ended_at: r.get("ended_at"),
        compressed: r.get::<i64, _>("compressed") != 0,
    }))
}

// Observations

pub async fn insert_observation(
    pool: &SqlitePool,
    session_id: &str,
    project: &str,
    tool: &str,
    input: Option<&str>,
    output: Option<&str>,
    max_bytes: usize,
) -> Result<i64> {
    let now = Utc::now().timestamp();

    // Truncate output to max_bytes
    let truncated_output = output.map(|o| {
        if o.len() > max_bytes {
            format!("{}... [truncated]", &o[..max_bytes])
        } else {
            o.to_string()
        }
    });

    let result = sqlx::query(
        "INSERT INTO observations (session_id, project, tool, input, output, created_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(session_id)
    .bind(project)
    .bind(tool)
    .bind(input)
    .bind(truncated_output)
    .bind(now)
    .execute(pool)
    .await?;

    Ok(result.last_insert_rowid())
}

pub async fn get_observations_for_session(
    pool: &SqlitePool,
    session_id: &str,
) -> Result<Vec<Observation>> {
    let rows = sqlx::query(
        "SELECT id, session_id, project, tool, input, output, created_at
         FROM observations WHERE session_id = ? ORDER BY created_at ASC",
    )
    .bind(session_id)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| Observation {
            id: r.get("id"),
            session_id: r.get("session_id"),
            project: r.get("project"),
            tool: r.get("tool"),
            input: r.get("input"),
            output: r.get("output"),
            created_at: r.get("created_at"),
        })
        .collect())
}

pub async fn observation_count_for_session(pool: &SqlitePool, session_id: &str) -> Result<i64> {
    let row = sqlx::query("SELECT COUNT(*) as cnt FROM observations WHERE session_id = ?")
        .bind(session_id)
        .fetch_one(pool)
        .await?;
    Ok(row.get("cnt"))
}

// Memories

pub async fn insert_memory(
    pool: &SqlitePool,
    project: &str,
    session_id: &str,
    summary: &str,
    tags: Option<&str>,
) -> Result<i64> {
    let now = Utc::now().timestamp();
    let result = sqlx::query(
        "INSERT INTO memories (project, session_id, summary, tags, created_at)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(project)
    .bind(session_id)
    .bind(summary)
    .bind(tags)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(result.last_insert_rowid())
}

pub async fn get_recent_memories(
    pool: &SqlitePool,
    project: &str,
    limit: i64,
) -> Result<Vec<Memory>> {
    let rows = sqlx::query(
        "SELECT rowid as id, project, session_id, summary, tags, created_at
         FROM memories WHERE project = ?
         ORDER BY created_at DESC LIMIT ?",
    )
    .bind(project)
    .bind(limit)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| Memory {
            id: r.get("id"),
            project: r.get("project"),
            session_id: r.get("session_id"),
            summary: r.get("summary"),
            tags: r.get("tags"),
            created_at: r.get("created_at"),
        })
        .collect())
}

pub async fn search_memories(
    pool: &SqlitePool,
    project: &str,
    query: &str,
    limit: i64,
) -> Result<Vec<Memory>> {
    // FTS5 search with project filter
    let rows = sqlx::query(
        "SELECT rowid as id, project, session_id, summary, tags, created_at
         FROM memories WHERE memories MATCH ? AND project = ?
         ORDER BY created_at DESC LIMIT ?",
    )
    .bind(query)
    .bind(project)
    .bind(limit)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| Memory {
            id: r.get("id"),
            project: r.get("project"),
            session_id: r.get("session_id"),
            summary: r.get("summary"),
            tags: r.get("tags"),
            created_at: r.get("created_at"),
        })
        .collect())
}

pub async fn delete_memories_for_project(pool: &SqlitePool, project: &str) -> Result<u64> {
    let result = sqlx::query("DELETE FROM memories WHERE project = ?")
        .bind(project)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

// Stats

pub struct DbStats {
    pub total_sessions: i64,
    pub total_memories: i64,
    pub total_observations: i64,
}

pub async fn get_stats(pool: &SqlitePool) -> Result<DbStats> {
    let sessions: i64 = sqlx::query("SELECT COUNT(*) as cnt FROM sessions")
        .fetch_one(pool)
        .await?
        .get("cnt");

    let memories: i64 = sqlx::query("SELECT COUNT(*) as cnt FROM memories")
        .fetch_one(pool)
        .await?
        .get("cnt");

    let observations: i64 = sqlx::query("SELECT COUNT(*) as cnt FROM observations")
        .fetch_one(pool)
        .await?
        .get("cnt");

    Ok(DbStats {
        total_sessions: sessions,
        total_memories: memories,
        total_observations: observations,
    })
}
