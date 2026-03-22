use anyhow::Result;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::any::AnyPoolOptions;
use sqlx::{AnyPool, Row};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Backend {
    Sqlite,
    Postgres,
}

#[derive(Clone)]
pub struct Database {
    pub pool: AnyPool,
    pub backend: Backend,
}

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

impl Database {
    pub async fn new(url: &str) -> Result<Self> {
        sqlx::any::install_default_drivers();
        let (db_url, backend) =
            if url.starts_with("postgres://") || url.starts_with("postgresql://") {
                (url.to_string(), Backend::Postgres)
            } else if url.starts_with("sqlite://") {
                (url.to_string(), Backend::Sqlite)
            } else {
                if let Some(parent) = std::path::Path::new(url).parent() {
                    std::fs::create_dir_all(parent)?;
                }
                (format!("sqlite://{}?mode=rwc", url), Backend::Sqlite)
            };
        let pool = AnyPoolOptions::new()
            .max_connections(5)
            .connect(&db_url)
            .await?;
        Ok(Self { pool, backend })
    }

    pub async fn migrate(&self) -> Result<()> {
        // Sessions table (works for both backends)
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS sessions (
                id          TEXT PRIMARY KEY,
                project     TEXT NOT NULL,
                started_at  BIGINT NOT NULL,
                ended_at    BIGINT,
                compressed  BIGINT NOT NULL DEFAULT 0
            )",
        )
        .execute(&self.pool)
        .await?;

        // Observations table (branched for auto-increment)
        match self.backend {
            Backend::Sqlite => {
                sqlx::query(
                    "CREATE TABLE IF NOT EXISTS observations (
                        id          INTEGER PRIMARY KEY AUTOINCREMENT,
                        session_id  TEXT NOT NULL REFERENCES sessions(id),
                        project     TEXT NOT NULL,
                        tool        TEXT NOT NULL,
                        input       TEXT,
                        output      TEXT,
                        created_at  INTEGER NOT NULL
                    )",
                )
                .execute(&self.pool)
                .await?;
            }
            Backend::Postgres => {
                sqlx::query(
                    "CREATE TABLE IF NOT EXISTS observations (
                        id          BIGSERIAL PRIMARY KEY,
                        session_id  TEXT NOT NULL REFERENCES sessions(id),
                        project     TEXT NOT NULL,
                        tool        TEXT NOT NULL,
                        input       TEXT,
                        output      TEXT,
                        created_at  BIGINT NOT NULL
                    )",
                )
                .execute(&self.pool)
                .await?;
            }
        }

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_obs_session ON observations(session_id)")
            .execute(&self.pool)
            .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_obs_project ON observations(project)")
            .execute(&self.pool)
            .await?;

        // Memories table (branched for FTS5 vs tsvector)
        match self.backend {
            Backend::Sqlite => {
                sqlx::query(
                    "CREATE VIRTUAL TABLE IF NOT EXISTS memories USING fts5(
                        project,
                        session_id,
                        summary,
                        tags,
                        created_at UNINDEXED,
                        tokenize='porter ascii'
                    )",
                )
                .execute(&self.pool)
                .await?;
            }
            Backend::Postgres => {
                sqlx::query(
                    "CREATE TABLE IF NOT EXISTS memories (
                        id              BIGSERIAL PRIMARY KEY,
                        project         TEXT NOT NULL,
                        session_id      TEXT NOT NULL,
                        summary         TEXT NOT NULL,
                        tags            TEXT,
                        created_at      BIGINT NOT NULL,
                        search_vector   TSVECTOR
                    )",
                )
                .execute(&self.pool)
                .await?;

                sqlx::query(
                    "CREATE INDEX IF NOT EXISTS idx_memories_search
                     ON memories USING GIN(search_vector)",
                )
                .execute(&self.pool)
                .await?;
            }
        }

        Ok(())
    }
}

// Sessions

pub async fn create_session(db: &Database, project: &str) -> Result<String> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = Utc::now().timestamp();
    sqlx::query("INSERT INTO sessions (id, project, started_at) VALUES ($1, $2, $3)")
        .bind(&id)
        .bind(project)
        .bind(now)
        .execute(&db.pool)
        .await?;
    Ok(id)
}

pub async fn end_session(db: &Database, session_id: &str) -> Result<()> {
    let now = Utc::now().timestamp();
    sqlx::query("UPDATE sessions SET ended_at = $1 WHERE id = $2")
        .bind(now)
        .bind(session_id)
        .execute(&db.pool)
        .await?;
    Ok(())
}

pub async fn mark_compressed(db: &Database, session_id: &str) -> Result<()> {
    sqlx::query("UPDATE sessions SET compressed = 1 WHERE id = $1")
        .bind(session_id)
        .execute(&db.pool)
        .await?;
    Ok(())
}

pub async fn get_session(db: &Database, session_id: &str) -> Result<Option<Session>> {
    let row: Option<sqlx::any::AnyRow> = sqlx::query(
        "SELECT id, project, started_at, ended_at, compressed FROM sessions WHERE id = $1",
    )
    .bind(session_id)
    .fetch_optional(&db.pool)
    .await?;

    Ok(row.map(|r: sqlx::any::AnyRow| Session {
        id: r.get("id"),
        project: r.get("project"),
        started_at: r.get("started_at"),
        ended_at: r.try_get("ended_at").ok().flatten(),
        compressed: r.get::<i64, _>("compressed") != 0,
    }))
}

// Observations

pub async fn insert_observation(
    db: &Database,
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

    let row: sqlx::any::AnyRow = sqlx::query(
        "INSERT INTO observations (session_id, project, tool, input, output, created_at)
         VALUES ($1, $2, $3, $4, $5, $6)
         RETURNING id",
    )
    .bind(session_id)
    .bind(project)
    .bind(tool)
    .bind(input)
    .bind(truncated_output.as_deref())
    .bind(now)
    .fetch_one(&db.pool)
    .await?;

    Ok(row.get("id"))
}

pub async fn get_observations_for_session(
    db: &Database,
    session_id: &str,
) -> Result<Vec<Observation>> {
    let rows: Vec<sqlx::any::AnyRow> = sqlx::query(
        "SELECT id, session_id, project, tool, input, output, created_at
         FROM observations WHERE session_id = $1 ORDER BY created_at ASC",
    )
    .bind(session_id)
    .fetch_all(&db.pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r: sqlx::any::AnyRow| Observation {
            id: r.get("id"),
            session_id: r.get("session_id"),
            project: r.get("project"),
            tool: r.get("tool"),
            input: r.try_get("input").ok().flatten(),
            output: r.try_get("output").ok().flatten(),
            created_at: r.get("created_at"),
        })
        .collect())
}

pub async fn observation_count_for_session(db: &Database, session_id: &str) -> Result<i64> {
    let row: sqlx::any::AnyRow =
        sqlx::query("SELECT COUNT(*) as cnt FROM observations WHERE session_id = $1")
            .bind(session_id)
            .fetch_one(&db.pool)
            .await?;
    Ok(row.get("cnt"))
}

// Memories

pub async fn insert_memory(
    db: &Database,
    project: &str,
    session_id: &str,
    summary: &str,
    tags: Option<&str>,
) -> Result<i64> {
    let now = Utc::now().timestamp();

    match db.backend {
        Backend::Sqlite => {
            sqlx::query(
                "INSERT INTO memories (project, session_id, summary, tags, created_at)
                 VALUES ($1, $2, $3, $4, $5)",
            )
            .bind(project)
            .bind(session_id)
            .bind(summary)
            .bind(tags)
            .bind(now)
            .execute(&db.pool)
            .await?;

            let row: sqlx::any::AnyRow = sqlx::query("SELECT last_insert_rowid() as id")
                .fetch_one(&db.pool)
                .await?;
            Ok(row.get("id"))
        }
        Backend::Postgres => {
            let row: sqlx::any::AnyRow = sqlx::query(
                "INSERT INTO memories (project, session_id, summary, tags, created_at, search_vector)
                 VALUES ($1, $2, $3, $4, $5, to_tsvector('english', $6 || ' ' || COALESCE($7, '')))
                 RETURNING id",
            )
            .bind(project)
            .bind(session_id)
            .bind(summary)
            .bind(tags)
            .bind(now)
            .bind(summary)
            .bind(tags)
            .fetch_one(&db.pool)
            .await?;
            Ok(row.get("id"))
        }
    }
}

pub async fn get_recent_memories(db: &Database, project: &str, limit: i64) -> Result<Vec<Memory>> {
    let query_str = match db.backend {
        Backend::Sqlite => {
            "SELECT rowid as id, project, session_id, summary, tags, created_at
             FROM memories WHERE project = $1
             ORDER BY created_at DESC LIMIT $2"
        }
        Backend::Postgres => {
            "SELECT id, project, session_id, summary, tags, created_at
             FROM memories WHERE project = $1
             ORDER BY created_at DESC LIMIT $2"
        }
    };

    let rows: Vec<sqlx::any::AnyRow> = sqlx::query(query_str)
        .bind(project)
        .bind(limit)
        .fetch_all(&db.pool)
        .await?;

    Ok(rows
        .into_iter()
        .map(|r: sqlx::any::AnyRow| Memory {
            id: r.get("id"),
            project: r.get("project"),
            session_id: r.get("session_id"),
            summary: r.get("summary"),
            tags: r.try_get("tags").ok().flatten(),
            created_at: r.get("created_at"),
        })
        .collect())
}

pub async fn search_memories(
    db: &Database,
    project: &str,
    query: &str,
    limit: i64,
) -> Result<Vec<Memory>> {
    let query_str = match db.backend {
        Backend::Sqlite => {
            "SELECT rowid as id, project, session_id, summary, tags, created_at
             FROM memories WHERE memories MATCH $1 AND project = $2
             ORDER BY created_at DESC LIMIT $3"
        }
        Backend::Postgres => {
            "SELECT id, project, session_id, summary, tags, created_at
             FROM memories WHERE search_vector @@ plainto_tsquery($1) AND project = $2
             ORDER BY created_at DESC LIMIT $3"
        }
    };

    let rows: Vec<sqlx::any::AnyRow> = sqlx::query(query_str)
        .bind(query)
        .bind(project)
        .bind(limit)
        .fetch_all(&db.pool)
        .await?;

    Ok(rows
        .into_iter()
        .map(|r: sqlx::any::AnyRow| Memory {
            id: r.get("id"),
            project: r.get("project"),
            session_id: r.get("session_id"),
            summary: r.get("summary"),
            tags: r.try_get("tags").ok().flatten(),
            created_at: r.get("created_at"),
        })
        .collect())
}

pub async fn delete_memories_for_project(db: &Database, project: &str) -> Result<u64> {
    let result = sqlx::query("DELETE FROM memories WHERE project = $1")
        .bind(project)
        .execute(&db.pool)
        .await?;
    Ok(result.rows_affected())
}

// Stats

pub struct DbStats {
    pub total_sessions: i64,
    pub total_memories: i64,
    pub total_observations: i64,
}

pub async fn get_stats(db: &Database) -> Result<DbStats> {
    let r: sqlx::any::AnyRow = sqlx::query("SELECT COUNT(*) as cnt FROM sessions")
        .fetch_one(&db.pool)
        .await?;
    let sessions: i64 = r.get("cnt");

    let r: sqlx::any::AnyRow = sqlx::query("SELECT COUNT(*) as cnt FROM memories")
        .fetch_one(&db.pool)
        .await?;
    let memories: i64 = r.get("cnt");

    let r: sqlx::any::AnyRow = sqlx::query("SELECT COUNT(*) as cnt FROM observations")
        .fetch_one(&db.pool)
        .await?;
    let observations: i64 = r.get("cnt");

    Ok(DbStats {
        total_sessions: sessions,
        total_memories: memories,
        total_observations: observations,
    })
}
