use anyhow::Result;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::any::AnyPoolOptions;
use sqlx::{AnyPool, Row};
use std::collections::HashMap;

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

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ProjectSummary {
    pub project: String,
    pub memory_count: i64,
    pub last_activity: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct NewMemoryEdge {
    pub project: String,
    pub memory_id: i64,
    pub source: String,
    pub relation: String,
    pub target: String,
    pub valid_from: Option<String>,
    pub valid_until: Option<String>,
    pub confidence: f64,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct MemoryEdge {
    pub id: i64,
    pub project: String,
    pub memory_id: i64,
    pub source: String,
    pub relation: String,
    pub target: String,
    pub valid_from: Option<String>,
    pub valid_until: Option<String>,
    pub observed_at: i64,
    pub confidence: f64,
    pub superseded_by: Option<i64>,
    pub superseded_reason: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct ReconcileReport {
    pub scanned: usize,
    pub duplicates: usize,
    pub current_state_updates: usize,
    pub active_edges: usize,
    pub dry_run: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SessionHistoryEntry {
    pub id: String,
    pub project: String,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub compressed: bool,
    pub observation_count: i64,
    pub tags: Option<String>,
}

impl Database {
    pub async fn new(url: &str) -> Result<Self> {
        register_sqlite_vec();
        sqlx::any::install_default_drivers();
        let (db_url, backend) =
            if url.starts_with("postgres://") || url.starts_with("postgresql://") {
                (url.to_string(), Backend::Postgres)
            } else if url.starts_with("sqlite://") {
                (url.to_string(), Backend::Sqlite)
            } else {
                let path = std::path::Path::new(url);
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                (sqlite_file_url(path), Backend::Sqlite)
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

        // CCR: lossless pointer to the verbatim full output (when the inline
        // `output` is only a truncated FTS preview). Additive + backward-
        // compatible — existing DBs gain the column on next migrate. SQLite has
        // no ADD COLUMN IF NOT EXISTS, so the duplicate-column error is ignored
        // on re-run; Postgres uses the native idempotent form.
        match self.backend {
            Backend::Sqlite => {
                let _ = sqlx::query("ALTER TABLE observations ADD COLUMN output_blob TEXT")
                    .execute(&self.pool)
                    .await;
            }
            Backend::Postgres => {
                sqlx::query("ALTER TABLE observations ADD COLUMN IF NOT EXISTS output_blob TEXT")
                    .execute(&self.pool)
                    .await?;
            }
        }

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

        // ── Semantic retrieval: canonical embeddings + memory metadata ──
        let blob_type = match self.backend {
            Backend::Sqlite => "BLOB",
            Backend::Postgres => "BYTEA",
        };
        sqlx::query(&format!(
            "CREATE TABLE IF NOT EXISTS embeddings (
                owner_type TEXT NOT NULL,
                owner_id   BIGINT NOT NULL,
                model      TEXT NOT NULL,
                dim        INTEGER NOT NULL,
                embedding  {blob_type} NOT NULL,
                created_at BIGINT NOT NULL,
                PRIMARY KEY (owner_type, owner_id, model)
            )"
        ))
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_embeddings_owner ON embeddings(owner_type, owner_id)",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS memory_meta (
                memory_id  BIGINT NOT NULL PRIMARY KEY,
                importance REAL NOT NULL DEFAULT 0.5,
                created_at BIGINT NOT NULL
            )",
        )
        .execute(&self.pool)
        .await?;

        // ── CCR: content-addressed reversible blob store ──
        // Tool outputs / session transcripts are stored whole, deduplicated by
        // the sha256 of their ORIGINAL bytes, and compressed by a byte-exact
        // reversible codec. `refcount` tracks live references for GC (Chunk 3).
        sqlx::query(&format!(
            "CREATE TABLE IF NOT EXISTS blobs (
                hash         TEXT PRIMARY KEY,
                content_type TEXT NOT NULL,
                codec        TEXT NOT NULL,
                orig_len     BIGINT NOT NULL,
                comp_len     BIGINT NOT NULL,
                data         {blob_type} NOT NULL,
                refcount     BIGINT NOT NULL DEFAULT 0,
                created_at   BIGINT NOT NULL
            )"
        ))
        .execute(&self.pool)
        .await?;

        // CCR: content-addressed dictionaries for per-content-type codecs. Each
        // blob records which dict compressed it (blobs.dict_hash) so dicts can be
        // (re)trained freely without ever breaking an existing blob's round-trip.
        match self.backend {
            Backend::Sqlite => {
                let _ = sqlx::query("ALTER TABLE blobs ADD COLUMN dict_hash TEXT")
                    .execute(&self.pool)
                    .await;
            }
            Backend::Postgres => {
                sqlx::query("ALTER TABLE blobs ADD COLUMN IF NOT EXISTS dict_hash TEXT")
                    .execute(&self.pool)
                    .await?;
            }
        }
        sqlx::query(&format!(
            "CREATE TABLE IF NOT EXISTS ccr_dicts (
                hash         TEXT PRIMARY KEY,
                content_type TEXT NOT NULL,
                data         {blob_type} NOT NULL,
                created_at   BIGINT NOT NULL
            )"
        ))
        .execute(&self.pool)
        .await?;

        // CCR: link a memory to the verbatim pre-LLM session transcript blob
        // behind its lossy summary. Additive + backward-compatible.
        match self.backend {
            Backend::Sqlite => {
                let _ = sqlx::query("ALTER TABLE memory_meta ADD COLUMN session_blob TEXT")
                    .execute(&self.pool)
                    .await;
            }
            Backend::Postgres => {
                sqlx::query("ALTER TABLE memory_meta ADD COLUMN IF NOT EXISTS session_blob TEXT")
                    .execute(&self.pool)
                    .await?;
            }
        }

        // Supermemory model: scope (project|user) + kind (typed) on memory_meta.
        // Additive with constant non-null defaults, so every existing row reads
        // back as a project-scoped session memory — legacy behavior unchanged.
        // SQLite has no ADD COLUMN IF NOT EXISTS, so the duplicate-column error
        // on re-run is ignored; Postgres uses the native idempotent form.
        match self.backend {
            Backend::Sqlite => {
                let _ = sqlx::query(
                    "ALTER TABLE memory_meta ADD COLUMN scope TEXT NOT NULL DEFAULT 'project'",
                )
                .execute(&self.pool)
                .await;
                let _ = sqlx::query(
                    "ALTER TABLE memory_meta ADD COLUMN kind TEXT NOT NULL DEFAULT 'session'",
                )
                .execute(&self.pool)
                .await;
            }
            Backend::Postgres => {
                sqlx::query(
                    "ALTER TABLE memory_meta ADD COLUMN IF NOT EXISTS scope TEXT NOT NULL DEFAULT 'project'",
                )
                .execute(&self.pool)
                .await?;
                sqlx::query(
                    "ALTER TABLE memory_meta ADD COLUMN IF NOT EXISTS kind TEXT NOT NULL DEFAULT 'session'",
                )
                .execute(&self.pool)
                .await?;
            }
        }

        // Temporal tag: the event time a memory describes (a date/range stated in
        // the session), distinct from created_at (wall-clock write time). Nullable
        // and additive — undated memories read back as None and the time-aware
        // retrieval boost simply skips them.
        match self.backend {
            Backend::Sqlite => {
                let _ = sqlx::query("ALTER TABLE memory_meta ADD COLUMN event_time TEXT")
                    .execute(&self.pool)
                    .await;
            }
            Backend::Postgres => {
                sqlx::query("ALTER TABLE memory_meta ADD COLUMN IF NOT EXISTS event_time TEXT")
                    .execute(&self.pool)
                    .await?;
            }
        }

        // Entity inverted index: one row per (memory, normalized proper-noun
        // token), so name-anchored questions resolve by direct lookup even when a
        // memory ranks low on keyword/vector. Additive — empty for legacy data.
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS memory_entities (
                memory_id BIGINT NOT NULL,
                entity    TEXT NOT NULL
            )",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_memory_entities_entity ON memory_entities(entity)",
        )
        .execute(&self.pool)
        .await?;

        // Temporal graph lite: structured edges derived from memories/facts.
        // Edges are append-only provenance records; reconciliation marks stale
        // rows via superseded_by / superseded_reason instead of deleting history.
        match self.backend {
            Backend::Sqlite => {
                sqlx::query(
                    "CREATE TABLE IF NOT EXISTS memory_edges (
                        id                 INTEGER PRIMARY KEY AUTOINCREMENT,
                        project            TEXT NOT NULL,
                        memory_id          BIGINT NOT NULL,
                        source             TEXT NOT NULL,
                        source_norm        TEXT NOT NULL,
                        relation           TEXT NOT NULL,
                        relation_norm      TEXT NOT NULL,
                        target             TEXT NOT NULL,
                        target_norm        TEXT NOT NULL,
                        valid_from         TEXT,
                        valid_until        TEXT,
                        observed_at        BIGINT NOT NULL,
                        confidence         REAL NOT NULL DEFAULT 0.5,
                        superseded_by      BIGINT,
                        superseded_reason  TEXT,
                        created_at         BIGINT NOT NULL
                    )",
                )
                .execute(&self.pool)
                .await?;
            }
            Backend::Postgres => {
                sqlx::query(
                    "CREATE TABLE IF NOT EXISTS memory_edges (
                        id                 BIGSERIAL PRIMARY KEY,
                        project            TEXT NOT NULL,
                        memory_id          BIGINT NOT NULL,
                        source             TEXT NOT NULL,
                        source_norm        TEXT NOT NULL,
                        relation           TEXT NOT NULL,
                        relation_norm      TEXT NOT NULL,
                        target             TEXT NOT NULL,
                        target_norm        TEXT NOT NULL,
                        valid_from         TEXT,
                        valid_until        TEXT,
                        observed_at        BIGINT NOT NULL,
                        confidence         DOUBLE PRECISION NOT NULL DEFAULT 0.5,
                        superseded_by      BIGINT,
                        superseded_reason  TEXT,
                        created_at         BIGINT NOT NULL
                    )",
                )
                .execute(&self.pool)
                .await?;
            }
        }
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_memory_edges_source ON memory_edges(project, source_norm, relation_norm)",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_memory_edges_target ON memory_edges(project, target_norm)",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_memory_edges_memory ON memory_edges(memory_id)",
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Lazily create the per-backend ANN structure sized for `dim`.
    /// SQLite: a `vec0` virtual table. Postgres: pgvector table + HNSW index.
    pub async fn ensure_ann(&self, dim: usize) -> Result<()> {
        match self.backend {
            Backend::Sqlite => {
                sqlx::query(&format!(
                    "CREATE VIRTUAL TABLE IF NOT EXISTS vec_memories USING vec0(memory_id INTEGER PRIMARY KEY, embedding float[{dim}] distance_metric=cosine)"
                ))
                .execute(&self.pool)
                .await?;
            }
            Backend::Postgres => {
                // .ok(): degrade to brute-force if the server lacks pgvector / privileges.
                sqlx::query("CREATE EXTENSION IF NOT EXISTS vector")
                    .execute(&self.pool)
                    .await
                    .ok();
                sqlx::query(&format!(
                    "CREATE TABLE IF NOT EXISTS memory_embeddings (memory_id BIGINT PRIMARY KEY, embedding vector({dim}))"
                ))
                .execute(&self.pool)
                .await?;
                sqlx::query(
                    "CREATE INDEX IF NOT EXISTS idx_memory_embeddings_hnsw ON memory_embeddings USING hnsw (embedding vector_cosine_ops)",
                )
                .execute(&self.pool)
                .await
                .ok();
            }
        }
        Ok(())
    }

    /// Drop ANN structures (used by `embed --force` on a dim change).
    pub async fn drop_ann(&self) -> Result<()> {
        let q = match self.backend {
            Backend::Sqlite => "DROP TABLE IF EXISTS vec_memories",
            Backend::Postgres => "DROP TABLE IF EXISTS memory_embeddings",
        };
        sqlx::query(q).execute(&self.pool).await?;
        Ok(())
    }
}

/// Register the sqlite-vec extension so every new SQLite connection gets the
/// `vec0` virtual table. Must run before any pool connection is opened.
/// Registered once; safe to call repeatedly.
// FFI: transmute a bare fn pointer to sqlite's entry-point signature. The
// explicit annotation would hard-code platform-specific c_char/c_int widths,
// so we allow the lint here rather than pin those types.
#[allow(clippy::missing_transmute_annotations)]
fn register_sqlite_vec() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        libsqlite3_sys::sqlite3_auto_extension(Some(std::mem::transmute(
            sqlite_vec::sqlite3_vec_init as *const (),
        )));
    });
}

fn sqlite_file_url(path: &std::path::Path) -> String {
    let normalized = path.to_string_lossy().replace('\\', "/");

    if normalized.len() >= 2 && normalized.as_bytes()[1] == b':' {
        format!("sqlite:///{}?mode=rwc", normalized)
    } else {
        format!("sqlite://{}?mode=rwc", normalized)
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

    // Truncate output to max_bytes on a UTF-8 char boundary. A raw `&o[..n]`
    // slice panics when `n` lands inside a multibyte char — and under the
    // release profile's `panic="abort"` that takes the whole MCP server down.
    let truncated_output = output.map(|o| crate::strutil::safe_truncate(o, max_bytes));

    // CCR: when truncation would lose bytes, preserve the verbatim original in
    // the content-addressed blob store and keep only the short FTS preview
    // inline. Below the cap there is nothing to lose, so we skip the blob.
    let output_blob: Option<String> = match output {
        Some(o) if o.len() > max_bytes => {
            Some(crate::ccr::store_blob(db, o.as_bytes(), None).await?.hash)
        }
        _ => None,
    };

    let row: sqlx::any::AnyRow = sqlx::query(
        "INSERT INTO observations (session_id, project, tool, input, output, output_blob, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7)
         RETURNING id",
    )
    .bind(session_id)
    .bind(project)
    .bind(tool)
    .bind(input)
    .bind(truncated_output.as_deref())
    .bind(output_blob.as_deref())
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

/// The CCR blob hash backing an observation's verbatim full output, if one was
/// stored (only set when the inline output was truncated). Returns `None` for
/// an unknown id or an observation that fit under the preview cap.
pub async fn get_observation_output_blob(
    db: &Database,
    observation_id: i64,
) -> Result<Option<String>> {
    let row: Option<sqlx::any::AnyRow> =
        sqlx::query("SELECT output_blob FROM observations WHERE id = $1")
            .bind(observation_id)
            .fetch_optional(&db.pool)
            .await?;
    Ok(row.and_then(|r| r.try_get::<Option<String>, _>("output_blob").ok().flatten()))
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
            // `memories` is an FTS5 virtual table (no RETURNING support), and
            // `last_insert_rowid()` is per-connection — so the INSERT and the
            // rowid read MUST run on the same pooled connection or a 5-way pool
            // can hand back a wrong/zero id.
            let mut conn = db.pool.acquire().await?;
            sqlx::query(
                "INSERT INTO memories (project, session_id, summary, tags, created_at)
                 VALUES ($1, $2, $3, $4, $5)",
            )
            .bind(project)
            .bind(session_id)
            .bind(summary)
            .bind(tags)
            .bind(now)
            .execute(&mut *conn)
            .await?;

            let row: sqlx::any::AnyRow = sqlx::query("SELECT last_insert_rowid() as id")
                .fetch_one(&mut *conn)
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

/// Build a safe FTS5 MATCH expression from free-text.
///
/// FTS5 reads bare punctuation (`?`, `:`, `*`, quotes, parens) as query syntax,
/// so an unsanitized question like "When did X happen?" raises a syntax error —
/// which callers swallow into an empty result, silently breaking retrieval.
/// We extract word tokens, quote each (FTS5 then treats them as literals), and
/// OR them for keyword recall; the vector side + RRF fusion handle precision.
/// Returns an empty string when the input has no word characters.
fn fts5_match_query(raw: &str) -> String {
    raw.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{t}\""))
        .collect::<Vec<_>>()
        .join(" OR ")
}

pub async fn search_memories(
    db: &Database,
    project: &str,
    query: &str,
    limit: i64,
) -> Result<Vec<Memory>> {
    // FTS5 needs sanitizing; Postgres plainto_tsquery already parses safely.
    let match_query = match db.backend {
        Backend::Sqlite => fts5_match_query(query),
        Backend::Postgres => query.to_string(),
    };
    if matches!(db.backend, Backend::Sqlite) && match_query.is_empty() {
        return Ok(Vec::new());
    }
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
        .bind(match_query)
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

pub async fn search_all_memories(db: &Database, query: &str, limit: i64) -> Result<Vec<Memory>> {
    let match_query = match db.backend {
        Backend::Sqlite => fts5_match_query(query),
        Backend::Postgres => query.to_string(),
    };
    if matches!(db.backend, Backend::Sqlite) && match_query.is_empty() {
        return Ok(Vec::new());
    }
    let query_str = match db.backend {
        Backend::Sqlite => {
            "SELECT rowid as id, project, session_id, summary, tags, created_at
             FROM memories WHERE memories MATCH $1
             ORDER BY created_at DESC LIMIT $2"
        }
        Backend::Postgres => {
            "SELECT id, project, session_id, summary, tags, created_at
             FROM memories WHERE search_vector @@ plainto_tsquery($1)
             ORDER BY created_at DESC LIMIT $2"
        }
    };

    let rows: Vec<sqlx::any::AnyRow> = sqlx::query(query_str)
        .bind(match_query)
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

pub async fn list_projects(db: &Database, limit: i64) -> Result<Vec<ProjectSummary>> {
    let rows: Vec<sqlx::any::AnyRow> = sqlx::query(
        "SELECT m.project,
                COUNT(*) AS memory_count,
                MAX(COALESCE(s.ended_at, s.started_at, m.created_at)) AS last_activity
         FROM memories m
         LEFT JOIN sessions s ON s.id = m.session_id
         GROUP BY m.project
         ORDER BY last_activity DESC
         LIMIT $1",
    )
    .bind(limit)
    .fetch_all(&db.pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r: sqlx::any::AnyRow| ProjectSummary {
            project: r.get("project"),
            memory_count: r.get("memory_count"),
            last_activity: r.get("last_activity"),
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

/// Collect all memory ids for a project (rowid in sqlite / id in pg). Used to
/// purge each memory's vectors + metadata after a project wipe.
pub async fn memory_ids_for_project(db: &Database, project: &str) -> Result<Vec<i64>> {
    let id_col = match db.backend {
        Backend::Sqlite => "rowid",
        Backend::Postgres => "id",
    };
    let sql = format!("SELECT {id_col} AS id FROM memories WHERE project = $1");
    let rows = sqlx::query(&sql).bind(project).fetch_all(&db.pool).await?;
    Ok(rows.into_iter().map(|r| r.get::<i64, _>("id")).collect())
}

// List sessions

pub async fn list_sessions(db: &Database, limit: i64) -> Result<Vec<Session>> {
    let rows: Vec<sqlx::any::AnyRow> = sqlx::query(
        "SELECT id, project, started_at, ended_at, compressed
         FROM sessions ORDER BY started_at DESC LIMIT $1",
    )
    .bind(limit)
    .fetch_all(&db.pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r: sqlx::any::AnyRow| Session {
            id: r.get("id"),
            project: r.get("project"),
            started_at: r.get("started_at"),
            ended_at: r.try_get("ended_at").ok().flatten(),
            compressed: r.get::<i64, _>("compressed") != 0,
        })
        .collect())
}

pub async fn list_session_history(
    db: &Database,
    project: &str,
    limit: i64,
) -> Result<Vec<SessionHistoryEntry>> {
    let rows: Vec<sqlx::any::AnyRow> = sqlx::query(
        "SELECT s.id,
                s.project,
                s.started_at,
                s.ended_at,
                s.compressed,
                (SELECT COUNT(*) FROM observations o WHERE o.session_id = s.id) AS observation_count,
                (SELECT m.tags
                 FROM memories m
                 WHERE m.session_id = s.id
                 ORDER BY m.created_at DESC
                 LIMIT 1) AS tags
         FROM sessions s
         WHERE s.project = $1
         ORDER BY s.started_at DESC
         LIMIT $2",
    )
    .bind(project)
    .bind(limit)
    .fetch_all(&db.pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r: sqlx::any::AnyRow| SessionHistoryEntry {
            id: r.get("id"),
            project: r.get("project"),
            started_at: r.get("started_at"),
            ended_at: r.try_get("ended_at").ok().flatten(),
            compressed: r.get::<i64, _>("compressed") != 0,
            observation_count: r.get("observation_count"),
            tags: r.try_get("tags").ok().flatten(),
        })
        .collect())
}

pub async fn delete_memory(db: &Database, memory_id: i64) -> Result<bool> {
    let query_str = match db.backend {
        Backend::Sqlite => "DELETE FROM memories WHERE rowid = $1",
        Backend::Postgres => "DELETE FROM memories WHERE id = $1",
    };
    let result = sqlx::query(query_str)
        .bind(memory_id)
        .execute(&db.pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn get_all_memories(db: &Database, limit: i64) -> Result<Vec<Memory>> {
    let query_str = match db.backend {
        Backend::Sqlite => {
            "SELECT rowid as id, project, session_id, summary, tags, created_at
             FROM memories ORDER BY created_at DESC LIMIT $1"
        }
        Backend::Postgres => {
            "SELECT id, project, session_id, summary, tags, created_at
             FROM memories ORDER BY created_at DESC LIMIT $1"
        }
    };

    let rows: Vec<sqlx::any::AnyRow> = sqlx::query(query_str)
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

// Embeddings & memory metadata

/// Upsert the canonical embedding row for a memory/fact/entity.
pub async fn upsert_embedding(
    db: &Database,
    owner_type: &str,
    owner_id: i64,
    model: &str,
    dim: i64,
    embedding: &[u8],
) -> Result<()> {
    let now = Utc::now().timestamp();
    sqlx::query(
        "INSERT INTO embeddings(owner_type, owner_id, model, dim, embedding, created_at)
         VALUES($1, $2, $3, $4, $5, $6)
         ON CONFLICT(owner_type, owner_id, model)
         DO UPDATE SET embedding = excluded.embedding, dim = excluded.dim, created_at = excluded.created_at",
    )
    .bind(owner_type)
    .bind(owner_id)
    .bind(model)
    .bind(dim)
    .bind(embedding.to_vec())
    .bind(now)
    .execute(&db.pool)
    .await?;
    Ok(())
}

/// Fetch a stored embedding blob. Currently exercised only by tests; kept as a
/// first-class accessor for the embeddings table.
#[cfg(test)]
pub async fn get_embedding(
    db: &Database,
    owner_type: &str,
    owner_id: i64,
    model: &str,
) -> Result<Option<Vec<u8>>> {
    let row: Option<sqlx::any::AnyRow> = sqlx::query(
        "SELECT embedding FROM embeddings WHERE owner_type = $1 AND owner_id = $2 AND model = $3",
    )
    .bind(owner_type)
    .bind(owner_id)
    .bind(model)
    .fetch_optional(&db.pool)
    .await?;
    Ok(row.map(|r| r.get::<Vec<u8>, _>("embedding")))
}

pub async fn delete_embedding(db: &Database, owner_type: &str, owner_id: i64) -> Result<()> {
    sqlx::query("DELETE FROM embeddings WHERE owner_type = $1 AND owner_id = $2")
        .bind(owner_type)
        .bind(owner_id)
        .execute(&db.pool)
        .await?;
    Ok(())
}

/// Remove all memory embeddings for a model (used by `embed --force` before a
/// full re-index).
pub async fn clear_embeddings_for_model(db: &Database, model: &str) -> Result<()> {
    sqlx::query("DELETE FROM embeddings WHERE owner_type = 'memory' AND model = $1")
        .bind(model)
        .execute(&db.pool)
        .await?;
    Ok(())
}

pub async fn upsert_memory_meta(db: &Database, memory_id: i64, importance: f64) -> Result<()> {
    let now = Utc::now().timestamp();
    sqlx::query(
        "INSERT INTO memory_meta(memory_id, importance, created_at)
         VALUES($1, $2, $3)
         ON CONFLICT(memory_id) DO UPDATE SET importance = excluded.importance",
    )
    .bind(memory_id)
    .bind(importance)
    .bind(now)
    .execute(&db.pool)
    .await?;
    Ok(())
}

/// Insert a default importance row only if none exists. Never overwrites an
/// importance already recorded by compression (used during embedding backfill).
pub async fn ensure_memory_meta(
    db: &Database,
    memory_id: i64,
    default_importance: f64,
) -> Result<()> {
    let now = Utc::now().timestamp();
    sqlx::query(
        "INSERT INTO memory_meta(memory_id, importance, created_at)
         VALUES($1, $2, $3)
         ON CONFLICT(memory_id) DO NOTHING",
    )
    .bind(memory_id)
    .bind(default_importance)
    .bind(now)
    .execute(&db.pool)
    .await?;
    Ok(())
}

/// Importance-only accessor. Production ranking reads importance + scope + kind
/// together via [`get_memory_meta_full`]; this single-value form is retained for
/// test assertions (mirrors the `#[cfg(test)]` `get_embedding`).
#[cfg(test)]
pub async fn get_memory_meta(db: &Database, memory_id: i64) -> Result<f64> {
    let row: Option<sqlx::any::AnyRow> =
        sqlx::query("SELECT importance FROM memory_meta WHERE memory_id = $1")
            .bind(memory_id)
            .fetch_optional(&db.pool)
            .await?;
    Ok(row.map(|r| r.get::<f64, _>("importance")).unwrap_or(0.5))
}

// ── Memory model: scope + kind ────────────────────────────────────────────────

/// Canonical typed-memory kinds. `session` is the default for auto-compressed
/// memories; the rest classify explicitly-curated or mined memories.
pub const MEMORY_KINDS: &[&str] = &[
    "session",
    "error_solution",
    "preference",
    "procedural",
    "architecture",
    "learned_pattern",
    "project_config",
    "profile",
    // Atomic facts extracted from a session by dual-output compression. Stored
    // as their own searchable memories so dates/names/quantities survive and
    // resolve on direct lookup (see compress::persist).
    "fact",
];

/// Clamp an arbitrary kind string to the known set, case-insensitively.
/// Anything unrecognized collapses to `session` (the safe default).
pub fn clamp_kind(kind: &str) -> &'static str {
    let k = kind.trim().to_ascii_lowercase();
    MEMORY_KINDS
        .iter()
        .copied()
        .find(|&v| v == k)
        .unwrap_or("session")
}

/// Clamp a scope string to `project` (default) or `user`.
pub fn clamp_scope(scope: &str) -> &'static str {
    match scope.trim().to_ascii_lowercase().as_str() {
        "user" => "user",
        _ => "project",
    }
}

/// Full metadata for a memory: importance plus its scope + kind. Defaults
/// (importance 0.5, scope `project`, kind `session`) apply when no row exists
/// or a legacy row predates the column.
#[derive(Debug, Clone)]
pub struct MemoryMetaInfo {
    pub importance: f64,
    // Populated + test-verified, but production scope selection happens at the
    // SQL layer (get_recent_memories_scoped), so nothing reads this field yet.
    #[allow(dead_code)]
    pub scope: String,
    pub kind: String,
    /// Event time the memory describes (a date/range stated in the session),
    /// `None` when undated. Distinct from `created_at` (write time). Production
    /// time-aware retrieval filters at the SQL layer (`memories_by_event_time`),
    /// so this field is read only in tests today.
    #[allow(dead_code)]
    pub event_time: Option<String>,
}

impl Default for MemoryMetaInfo {
    fn default() -> Self {
        Self {
            importance: 0.5,
            scope: "project".to_string(),
            kind: "session".to_string(),
            event_time: None,
        }
    }
}

/// Read a memory's importance + scope + kind in one query. Missing rows / null
/// columns fall back to the defaults in [`MemoryMetaInfo::default`].
pub async fn get_memory_meta_full(db: &Database, memory_id: i64) -> Result<MemoryMetaInfo> {
    let row: Option<sqlx::any::AnyRow> = sqlx::query(
        "SELECT importance, scope, kind, event_time FROM memory_meta WHERE memory_id = $1",
    )
    .bind(memory_id)
    .fetch_optional(&db.pool)
    .await?;
    Ok(match row {
        Some(r) => MemoryMetaInfo {
            importance: r.try_get::<f64, _>("importance").unwrap_or(0.5),
            scope: r
                .try_get::<Option<String>, _>("scope")
                .ok()
                .flatten()
                .unwrap_or_else(|| "project".to_string()),
            kind: r
                .try_get::<Option<String>, _>("kind")
                .ok()
                .flatten()
                .unwrap_or_else(|| "session".to_string()),
            event_time: r.try_get::<Option<String>, _>("event_time").ok().flatten(),
        },
        None => MemoryMetaInfo::default(),
    })
}

/// Set a memory's scope + kind, clamping both to the known sets. Upserts the
/// meta row so it works whether or not `upsert_memory_meta` ran first; a fresh
/// row gets the default importance and never clobbers an existing one.
pub async fn set_memory_scope_kind(
    db: &Database,
    memory_id: i64,
    scope: &str,
    kind: &str,
) -> Result<()> {
    let now = Utc::now().timestamp();
    sqlx::query(
        "INSERT INTO memory_meta(memory_id, importance, created_at, scope, kind)
         VALUES($1, 0.5, $2, $3, $4)
         ON CONFLICT(memory_id) DO UPDATE SET scope = excluded.scope, kind = excluded.kind",
    )
    .bind(memory_id)
    .bind(now)
    .bind(clamp_scope(scope))
    .bind(clamp_kind(kind))
    .execute(&db.pool)
    .await?;
    Ok(())
}

/// Set a memory's `event_time` (a date/range stated in the session). Upserts the
/// meta row so it works whether or not `upsert_memory_meta`/`set_memory_scope_kind`
/// ran first; a fresh row gets the default importance and never clobbers an
/// existing one's importance/scope/kind.
pub async fn set_memory_event_time(db: &Database, memory_id: i64, event_time: &str) -> Result<()> {
    let now = Utc::now().timestamp();
    sqlx::query(
        "INSERT INTO memory_meta(memory_id, importance, created_at, event_time)
         VALUES($1, 0.5, $2, $3)
         ON CONFLICT(memory_id) DO UPDATE SET event_time = excluded.event_time",
    )
    .bind(memory_id)
    .bind(now)
    .bind(event_time)
    .execute(&db.pool)
    .await?;
    Ok(())
}

/// Memory ids whose recorded `event_time` contains `needle` (typically a year),
/// scoped to `project` when given, most-recent first and capped at `limit`.
/// Undated memories (NULL event_time) never match, so this signal is purely
/// additive to keyword/vector retrieval. Powers the time-aware retrieval boost.
pub async fn memories_by_event_time(
    db: &Database,
    project: Option<&str>,
    needle: &str,
    limit: usize,
) -> Result<Vec<i64>> {
    let id_col = match db.backend {
        Backend::Sqlite => "m.rowid",
        Backend::Postgres => "m.id",
    };
    let like = format!("%{needle}%");
    let rows: Vec<sqlx::any::AnyRow> = match project {
        Some(p) => {
            sqlx::query(&format!(
                "SELECT {id_col} AS id FROM memories m
                 JOIN memory_meta mm ON mm.memory_id = {id_col}
                 WHERE mm.event_time LIKE $1 AND m.project = $2
                 ORDER BY m.created_at DESC LIMIT $3"
            ))
            .bind(&like)
            .bind(p)
            .bind(limit as i64)
            .fetch_all(&db.pool)
            .await?
        }
        None => {
            sqlx::query(&format!(
                "SELECT {id_col} AS id FROM memories m
                 JOIN memory_meta mm ON mm.memory_id = {id_col}
                 WHERE mm.event_time LIKE $1
                 ORDER BY m.created_at DESC LIMIT $2"
            ))
            .bind(&like)
            .bind(limit as i64)
            .fetch_all(&db.pool)
            .await?
        }
    };
    Ok(rows.into_iter().map(|r| r.get::<i64, _>("id")).collect())
}

/// Index a memory under an entity (proper noun). The entity is split into word
/// tokens, lowercased, and stored one row per token (≥3 chars) so a single-token
/// query resolves any token of a multi-word entity ("York" of "New York").
/// Duplicate tokens within one call are collapsed.
pub async fn insert_memory_entity(db: &Database, memory_id: i64, entity: &str) -> Result<()> {
    let mut seen = std::collections::HashSet::new();
    for tok in entity.split(|c: char| !c.is_alphanumeric()) {
        let t = tok.to_lowercase();
        if t.chars().count() < 3 || !seen.insert(t.clone()) {
            continue;
        }
        sqlx::query("INSERT INTO memory_entities(memory_id, entity) VALUES($1, $2)")
            .bind(memory_id)
            .bind(&t)
            .execute(&db.pool)
            .await?;
    }
    Ok(())
}

/// Memory ids indexed under `entity` (matched case-insensitively against a single
/// normalized token), scoped to `project` when given, most-recent first and
/// capped at `limit`. Powers the entity-aware retrieval signal.
pub async fn memories_for_entity(
    db: &Database,
    project: Option<&str>,
    entity: &str,
    limit: usize,
) -> Result<Vec<i64>> {
    let needle = entity.trim().to_lowercase();
    if needle.chars().count() < 3 {
        return Ok(Vec::new());
    }
    let id_col = match db.backend {
        Backend::Sqlite => "m.rowid",
        Backend::Postgres => "m.id",
    };
    let rows: Vec<sqlx::any::AnyRow> = match project {
        Some(p) => {
            sqlx::query(&format!(
                "SELECT {id_col} AS id, m.created_at AS ca
                 FROM memories m JOIN memory_entities me ON me.memory_id = {id_col}
                 WHERE me.entity = $1 AND m.project = $2
                 GROUP BY {id_col}, m.created_at
                 ORDER BY ca DESC LIMIT $3"
            ))
            .bind(&needle)
            .bind(p)
            .bind(limit as i64)
            .fetch_all(&db.pool)
            .await?
        }
        None => {
            sqlx::query(&format!(
                "SELECT {id_col} AS id, m.created_at AS ca
                 FROM memories m JOIN memory_entities me ON me.memory_id = {id_col}
                 WHERE me.entity = $1
                 GROUP BY {id_col}, m.created_at
                 ORDER BY ca DESC LIMIT $2"
            ))
            .bind(&needle)
            .bind(limit as i64)
            .fetch_all(&db.pool)
            .await?
        }
    };
    Ok(rows.into_iter().map(|r| r.get::<i64, _>("id")).collect())
}

fn normalize_graph_text(value: &str) -> String {
    value
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .collect::<Vec<_>>()
        .join(" ")
}

fn normalize_relation(value: &str) -> String {
    normalize_graph_text(value).replace(' ', "_")
}

fn is_current_state_relation(relation_norm: &str) -> bool {
    relation_norm.starts_with("current_")
        || matches!(
            relation_norm,
            "is" | "status"
                | "state"
                | "role"
                | "location"
                | "works_at"
                | "lives_at"
                | "assigned_to"
                | "owner"
                | "primary"
        )
}

fn memory_edge_from_row(r: sqlx::any::AnyRow) -> MemoryEdge {
    MemoryEdge {
        id: r.get("id"),
        project: r.get("project"),
        memory_id: r.get("memory_id"),
        source: r.get("source"),
        relation: r.get("relation"),
        target: r.get("target"),
        valid_from: r.try_get("valid_from").ok().flatten(),
        valid_until: r.try_get("valid_until").ok().flatten(),
        observed_at: r.get("observed_at"),
        confidence: r.try_get::<f64, _>("confidence").unwrap_or(0.5),
        superseded_by: r.try_get("superseded_by").ok().flatten(),
        superseded_reason: r.try_get("superseded_reason").ok().flatten(),
        created_at: r.get("created_at"),
    }
}

/// Insert a temporal graph edge derived from a memory/fact and reconcile older
/// active edges in-place. Reconciliation never deletes history:
/// - exact older duplicates are marked `duplicate`;
/// - older active current-state edges for the same source+relation but a
///   different target are marked `current_state_update` and closed with
///   `valid_until`.
pub async fn insert_memory_edge(db: &Database, edge: &NewMemoryEdge) -> Result<i64> {
    let source_norm = normalize_graph_text(&edge.source);
    let relation_norm = normalize_relation(&edge.relation);
    let target_norm = normalize_graph_text(&edge.target);
    if source_norm.is_empty() || relation_norm.is_empty() || target_norm.is_empty() {
        anyhow::bail!("memory edge source, relation, and target must not be empty");
    }

    let now = Utc::now().timestamp();
    let confidence = edge.confidence.clamp(0.0, 1.0);
    let id = match db.backend {
        Backend::Sqlite => {
            let mut conn = db.pool.acquire().await?;
            sqlx::query(
                "INSERT INTO memory_edges
                 (project, memory_id, source, source_norm, relation, relation_norm,
                  target, target_norm, valid_from, valid_until, observed_at,
                  confidence, created_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
            )
            .bind(&edge.project)
            .bind(edge.memory_id)
            .bind(edge.source.trim())
            .bind(&source_norm)
            .bind(edge.relation.trim())
            .bind(&relation_norm)
            .bind(edge.target.trim())
            .bind(&target_norm)
            .bind(edge.valid_from.as_deref())
            .bind(edge.valid_until.as_deref())
            .bind(now)
            .bind(confidence)
            .bind(now)
            .execute(&mut *conn)
            .await?;

            let row: sqlx::any::AnyRow = sqlx::query("SELECT last_insert_rowid() AS id")
                .fetch_one(&mut *conn)
                .await?;
            row.get("id")
        }
        Backend::Postgres => {
            let row: sqlx::any::AnyRow = sqlx::query(
                "INSERT INTO memory_edges
                 (project, memory_id, source, source_norm, relation, relation_norm,
                  target, target_norm, valid_from, valid_until, observed_at,
                  confidence, created_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
                 RETURNING id",
            )
            .bind(&edge.project)
            .bind(edge.memory_id)
            .bind(edge.source.trim())
            .bind(&source_norm)
            .bind(edge.relation.trim())
            .bind(&relation_norm)
            .bind(edge.target.trim())
            .bind(&target_norm)
            .bind(edge.valid_from.as_deref())
            .bind(edge.valid_until.as_deref())
            .bind(now)
            .bind(confidence)
            .bind(now)
            .fetch_one(&db.pool)
            .await?;
            row.get("id")
        }
    };

    reconcile_inserted_memory_edge(
        db,
        id,
        edge,
        &source_norm,
        &relation_norm,
        &target_norm,
        now,
    )
    .await?;
    Ok(id)
}

async fn reconcile_inserted_memory_edge(
    db: &Database,
    new_id: i64,
    edge: &NewMemoryEdge,
    source_norm: &str,
    relation_norm: &str,
    target_norm: &str,
    observed_at: i64,
) -> Result<()> {
    sqlx::query(
        "UPDATE memory_edges
         SET superseded_by = $1, superseded_reason = 'duplicate'
         WHERE project = $2
           AND source_norm = $3
           AND relation_norm = $4
           AND target_norm = $5
           AND COALESCE(valid_from, '') = COALESCE($6, '')
           AND id <> $1
           AND superseded_by IS NULL",
    )
    .bind(new_id)
    .bind(&edge.project)
    .bind(source_norm)
    .bind(relation_norm)
    .bind(target_norm)
    .bind(edge.valid_from.as_deref())
    .execute(&db.pool)
    .await?;

    if edge.valid_until.is_none() && is_current_state_relation(relation_norm) {
        let closes_at = edge
            .valid_from
            .clone()
            .unwrap_or_else(|| observed_at.to_string());
        sqlx::query(
            "UPDATE memory_edges
             SET superseded_by = $1,
                 superseded_reason = 'current_state_update',
                 valid_until = COALESCE(valid_until, $2)
             WHERE project = $3
               AND source_norm = $4
               AND relation_norm = $5
               AND target_norm <> $6
               AND id <> $1
               AND superseded_by IS NULL",
        )
        .bind(new_id)
        .bind(closes_at)
        .bind(&edge.project)
        .bind(source_norm)
        .bind(relation_norm)
        .bind(target_norm)
        .execute(&db.pool)
        .await?;
    }

    Ok(())
}

/// Query graph edges by entity. By default this returns active/current edges
/// only; set `include_superseded=true` to inspect provenance history.
pub async fn memory_edges_for_entity(
    db: &Database,
    project: Option<&str>,
    entity: &str,
    include_superseded: bool,
    limit: usize,
) -> Result<Vec<MemoryEdge>> {
    let entity_norm = normalize_graph_text(entity);
    if entity_norm.is_empty() {
        return Ok(Vec::new());
    }
    let mut sql =
        "SELECT id, project, memory_id, source, relation, target, valid_from, valid_until,
                          observed_at, confidence, superseded_by, superseded_reason, created_at
                   FROM memory_edges
                   WHERE (source_norm = $1 OR target_norm = $1)"
            .to_string();
    if !include_superseded {
        sql.push_str(" AND superseded_by IS NULL");
    }
    let limit_ph = if project.is_some() {
        sql.push_str(" AND project = $2");
        "$3"
    } else {
        "$2"
    };
    sql.push_str(&format!(
        " ORDER BY observed_at DESC, id DESC LIMIT {limit_ph}"
    ));

    let mut q = sqlx::query(&sql).bind(&entity_norm);
    if let Some(p) = project {
        q = q.bind(p);
    }
    let rows: Vec<sqlx::any::AnyRow> = q.bind(limit as i64).fetch_all(&db.pool).await?;
    Ok(rows.into_iter().map(memory_edge_from_row).collect())
}

/// Query graph edges by entity at a valid-time instant. `at_time` must be a
/// YYYY-MM-DD string validated by the caller/parser. When absent, this behaves
/// like [`memory_edges_for_entity`].
pub async fn memory_edges_for_entity_at(
    db: &Database,
    project: Option<&str>,
    entity: &str,
    include_superseded: bool,
    at_time: Option<&str>,
    limit: usize,
) -> Result<Vec<MemoryEdge>> {
    let entity_norm = normalize_graph_text(entity);
    if entity_norm.is_empty() {
        return Ok(Vec::new());
    }
    let mut sql =
        "SELECT id, project, memory_id, source, relation, target, valid_from, valid_until,
                          observed_at, confidence, superseded_by, superseded_reason, created_at
                   FROM memory_edges
                   WHERE (source_norm = $1 OR target_norm = $1)"
            .to_string();
    if !include_superseded {
        sql.push_str(" AND superseded_by IS NULL");
    }

    let mut next = 2;
    let at_ph = if at_time.is_some() {
        let ph = format!("${next}");
        next += 1;
        sql.push_str(&format!(
            " AND (valid_from IS NULL OR valid_from <= {ph})
              AND (valid_until IS NULL OR {ph} < valid_until)"
        ));
        Some(ph)
    } else {
        None
    };
    let project_ph = if project.is_some() {
        let ph = format!("${next}");
        next += 1;
        sql.push_str(&format!(" AND project = {ph}"));
        Some(ph)
    } else {
        None
    };
    let limit_ph = format!("${next}");
    let _ = at_ph;
    let _ = project_ph;
    sql.push_str(&format!(
        " ORDER BY observed_at DESC, id DESC LIMIT {limit_ph}"
    ));

    let mut q = sqlx::query(&sql).bind(&entity_norm);
    if let Some(at) = at_time {
        q = q.bind(at);
    }
    if let Some(p) = project {
        q = q.bind(p);
    }
    let rows: Vec<sqlx::any::AnyRow> = q.bind(limit as i64).fetch_all(&db.pool).await?;
    Ok(rows.into_iter().map(memory_edge_from_row).collect())
}

async fn all_memory_edges(db: &Database, project: Option<&str>) -> Result<Vec<MemoryEdge>> {
    let mut sql =
        "SELECT id, project, memory_id, source, relation, target, valid_from, valid_until,
                observed_at, confidence, superseded_by, superseded_reason, created_at
         FROM memory_edges"
            .to_string();
    if project.is_some() {
        sql.push_str(" WHERE project = $1");
    }
    sql.push_str(" ORDER BY project ASC, source ASC, relation ASC, observed_at ASC, id ASC");
    let mut q = sqlx::query(&sql);
    if let Some(p) = project {
        q = q.bind(p);
    }
    let rows: Vec<sqlx::any::AnyRow> = q.fetch_all(&db.pool).await?;
    Ok(rows.into_iter().map(memory_edge_from_row).collect())
}

fn edge_exact_key(edge: &MemoryEdge) -> String {
    format!(
        "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
        edge.project,
        normalize_graph_text(&edge.source),
        normalize_relation(&edge.relation),
        normalize_graph_text(&edge.target),
        edge.valid_from.clone().unwrap_or_default()
    )
}

fn edge_state_key(edge: &MemoryEdge) -> String {
    format!(
        "{}\u{1f}{}\u{1f}{}",
        edge.project,
        normalize_graph_text(&edge.source),
        normalize_relation(&edge.relation)
    )
}

fn edge_sort_newest(edges: &mut [&MemoryEdge]) {
    edges.sort_by(|a, b| {
        b.observed_at
            .cmp(&a.observed_at)
            .then_with(|| b.created_at.cmp(&a.created_at))
            .then_with(|| b.id.cmp(&a.id))
    });
}

/// Re-run graph reconciliation over existing rows. This is useful after imports,
/// migrations, or older databases that predate insert-time reconciliation.
pub async fn reconcile_memory_graph(
    db: &Database,
    project: Option<&str>,
    dry_run: bool,
) -> Result<ReconcileReport> {
    let edges = all_memory_edges(db, project).await?;
    let scanned = edges.len();

    let mut exact: HashMap<String, Vec<&MemoryEdge>> = HashMap::new();
    for edge in edges.iter().filter(|e| e.superseded_by.is_none()) {
        exact.entry(edge_exact_key(edge)).or_default().push(edge);
    }

    let mut duplicate_updates: Vec<(i64, i64)> = Vec::new();
    for group in exact.values_mut() {
        if group.len() <= 1 {
            continue;
        }
        edge_sort_newest(group);
        let winner = group[0].id;
        for duplicate in group.iter().skip(1) {
            duplicate_updates.push((duplicate.id, winner));
        }
    }

    let mut active_after_duplicates: Vec<&MemoryEdge> = edges
        .iter()
        .filter(|e| e.superseded_by.is_none())
        .filter(|e| !duplicate_updates.iter().any(|(id, _)| *id == e.id))
        .collect();

    let mut state_groups: HashMap<String, Vec<&MemoryEdge>> = HashMap::new();
    for edge in active_after_duplicates.drain(..) {
        let relation_norm = normalize_relation(&edge.relation);
        if is_current_state_relation(&relation_norm) && edge.valid_until.is_none() {
            state_groups
                .entry(edge_state_key(edge))
                .or_default()
                .push(edge);
        }
    }

    let mut state_updates: Vec<(i64, i64, String)> = Vec::new();
    for group in state_groups.values_mut() {
        if group.len() <= 1 {
            continue;
        }
        edge_sort_newest(group);
        let winner = group[0];
        let winner_target = normalize_graph_text(&winner.target);
        let closes_at = winner
            .valid_from
            .clone()
            .unwrap_or_else(|| winner.observed_at.to_string());
        for older in group.iter().skip(1) {
            if normalize_graph_text(&older.target) != winner_target {
                state_updates.push((older.id, winner.id, closes_at.clone()));
            }
        }
    }

    if !dry_run {
        for (id, winner) in &duplicate_updates {
            sqlx::query(
                "UPDATE memory_edges
                 SET superseded_by = $1, superseded_reason = 'duplicate'
                 WHERE id = $2 AND superseded_by IS NULL",
            )
            .bind(winner)
            .bind(id)
            .execute(&db.pool)
            .await?;
        }
        for (id, winner, closes_at) in &state_updates {
            sqlx::query(
                "UPDATE memory_edges
                 SET superseded_by = $1,
                     superseded_reason = 'current_state_update',
                     valid_until = COALESCE(valid_until, $2)
                 WHERE id = $3 AND superseded_by IS NULL",
            )
            .bind(winner)
            .bind(closes_at)
            .bind(id)
            .execute(&db.pool)
            .await?;
        }
    }

    let active_edges = if dry_run {
        scanned
            .saturating_sub(duplicate_updates.len())
            .saturating_sub(state_updates.len())
    } else {
        count_active_memory_edges(db, project).await?
    };

    Ok(ReconcileReport {
        scanned,
        duplicates: duplicate_updates.len(),
        current_state_updates: state_updates.len(),
        active_edges,
        dry_run,
    })
}

pub async fn count_active_memory_edges(db: &Database, project: Option<&str>) -> Result<usize> {
    let row: sqlx::any::AnyRow = if let Some(p) = project {
        sqlx::query(
            "SELECT COUNT(*) AS cnt FROM memory_edges WHERE superseded_by IS NULL AND project = $1",
        )
        .bind(p)
        .fetch_one(&db.pool)
        .await?
    } else {
        sqlx::query("SELECT COUNT(*) AS cnt FROM memory_edges WHERE superseded_by IS NULL")
            .fetch_one(&db.pool)
            .await?
    };
    Ok(row.get::<i64, _>("cnt").max(0) as usize)
}

/// Existing memories that have no graph edges yet, newest first. Used by
/// graph-backfill. Summaries are not mutated; returned ids become edge provenance.
pub async fn memories_without_edges(
    db: &Database,
    project: Option<&str>,
    limit: usize,
) -> Result<Vec<Memory>> {
    let id_col = match db.backend {
        Backend::Sqlite => "m.rowid",
        Backend::Postgres => "m.id",
    };
    let rows: Vec<sqlx::any::AnyRow> = match project {
        Some(p) => {
            sqlx::query(&format!(
                "SELECT {id_col} AS id, m.project, m.session_id, m.summary, m.tags, m.created_at
                 FROM memories m
                 WHERE m.project = $1
                   AND NOT EXISTS (SELECT 1 FROM memory_edges e WHERE e.memory_id = {id_col})
                 ORDER BY m.created_at DESC LIMIT $2"
            ))
            .bind(p)
            .bind(limit as i64)
            .fetch_all(&db.pool)
            .await?
        }
        None => {
            sqlx::query(&format!(
                "SELECT {id_col} AS id, m.project, m.session_id, m.summary, m.tags, m.created_at
                 FROM memories m
                 WHERE NOT EXISTS (SELECT 1 FROM memory_edges e WHERE e.memory_id = {id_col})
                 ORDER BY m.created_at DESC LIMIT $1"
            ))
            .bind(limit as i64)
            .fetch_all(&db.pool)
            .await?
        }
    };
    Ok(rows
        .into_iter()
        .map(|r| Memory {
            id: r.get("id"),
            project: r.get("project"),
            session_id: r.get("session_id"),
            summary: r.get("summary"),
            tags: r.try_get("tags").ok().flatten(),
            created_at: r.get("created_at"),
        })
        .collect())
}

#[cfg(test)]
pub async fn memory_edges_for_memory(db: &Database, memory_id: i64) -> Result<Vec<MemoryEdge>> {
    let rows: Vec<sqlx::any::AnyRow> = sqlx::query(
        "SELECT id, project, memory_id, source, relation, target, valid_from, valid_until,
                observed_at, confidence, superseded_by, superseded_reason, created_at
         FROM memory_edges
         WHERE memory_id = $1
         ORDER BY id ASC",
    )
    .bind(memory_id)
    .fetch_all(&db.pool)
    .await?;
    Ok(rows.into_iter().map(memory_edge_from_row).collect())
}

pub async fn delete_memory_edges(db: &Database, memory_id: i64) -> Result<()> {
    sqlx::query("DELETE FROM memory_edges WHERE memory_id = $1")
        .bind(memory_id)
        .execute(&db.pool)
        .await?;
    Ok(())
}

/// Kind for each of `ids`, in one query. Ids absent from `memory_meta` (legacy
/// rows / no meta) are simply omitted — callers treat a missing id as a narrative
/// (non-fact) default. Used by the retrieval narrative-reserve quota.
pub async fn kinds_for_memories(
    db: &Database,
    ids: &[i64],
) -> Result<std::collections::HashMap<i64, String>> {
    let mut out = std::collections::HashMap::new();
    if ids.is_empty() {
        return Ok(out);
    }
    // i64 values are safe to inline; avoids per-backend variadic IN binding.
    let in_list = ids
        .iter()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!("SELECT memory_id, kind FROM memory_meta WHERE memory_id IN ({in_list})");
    let rows: Vec<sqlx::any::AnyRow> = sqlx::query(&sql).fetch_all(&db.pool).await?;
    for r in rows {
        let id: i64 = r.get("memory_id");
        if let Some(kind) = r.try_get::<Option<String>, _>("kind").ok().flatten() {
            out.insert(id, kind);
        }
    }
    Ok(out)
}

/// Recent memories filtered by scope. `user`-scope memories are global (the
/// `project` argument is ignored); `project`-scope returns the project's
/// memories — including legacy rows with no meta or a null scope, which read as
/// `project` via COALESCE so existing data keeps surfacing.
pub async fn get_recent_memories_scoped(
    db: &Database,
    scope: &str,
    project: Option<&str>,
    limit: i64,
) -> Result<Vec<Memory>> {
    let id_col = match db.backend {
        Backend::Sqlite => "m.rowid",
        Backend::Postgres => "m.id",
    };
    let mut sql = format!(
        "SELECT {id_col} AS id, m.project, m.session_id, m.summary, m.tags, m.created_at
         FROM memories m
         LEFT JOIN memory_meta mm ON mm.memory_id = {id_col}
         WHERE COALESCE(mm.scope, 'project') = $1"
    );
    let limit_ph = if project.is_some() {
        sql.push_str(" AND m.project = $2");
        "$3"
    } else {
        "$2"
    };
    sql.push_str(&format!(" ORDER BY m.created_at DESC LIMIT {limit_ph}"));

    let mut q = sqlx::query(&sql).bind(clamp_scope(scope));
    if let Some(p) = project {
        q = q.bind(p);
    }
    let rows: Vec<sqlx::any::AnyRow> = q.bind(limit).fetch_all(&db.pool).await?;

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

/// The singleton user profile memory (scope=user, kind=profile), newest first if
/// duplicates somehow exist. `None` when no profile has been generated yet.
pub async fn get_profile_memory(db: &Database) -> Result<Option<Memory>> {
    let id_col = match db.backend {
        Backend::Sqlite => "m.rowid",
        Backend::Postgres => "m.id",
    };
    let sql = format!(
        "SELECT {id_col} AS id, m.project, m.session_id, m.summary, m.tags, m.created_at
         FROM memories m
         JOIN memory_meta mm ON mm.memory_id = {id_col}
         WHERE mm.scope = 'user' AND mm.kind = 'profile'
         ORDER BY m.created_at DESC LIMIT 1"
    );
    let row: Option<sqlx::any::AnyRow> = sqlx::query(&sql).fetch_optional(&db.pool).await?;
    Ok(row.map(|r: sqlx::any::AnyRow| Memory {
        id: r.get("id"),
        project: r.get("project"),
        session_id: r.get("session_id"),
        summary: r.get("summary"),
        tags: r.try_get("tags").ok().flatten(),
        created_at: r.get("created_at"),
    }))
}

/// Count user-scoped memories that are NOT the profile itself — the signal for
/// when to regenerate the profile.
pub async fn count_user_memories(db: &Database) -> Result<i64> {
    let id_col = match db.backend {
        Backend::Sqlite => "m.rowid",
        Backend::Postgres => "m.id",
    };
    let sql = format!(
        "SELECT COUNT(*) AS cnt
         FROM memories m
         JOIN memory_meta mm ON mm.memory_id = {id_col}
         WHERE mm.scope = 'user' AND mm.kind <> 'profile'"
    );
    let row: sqlx::any::AnyRow = sqlx::query(&sql).fetch_one(&db.pool).await?;
    Ok(row.get("cnt"))
}

/// Recent memories of a given `kind` (clamped), newest first. With `project`
/// set, restricts to that project; otherwise returns matches across all
/// projects. Used to surface typed memories like `error_solution` corrections.
pub async fn get_memories_by_kind(
    db: &Database,
    project: Option<&str>,
    kind: &str,
    limit: i64,
) -> Result<Vec<Memory>> {
    let id_col = match db.backend {
        Backend::Sqlite => "m.rowid",
        Backend::Postgres => "m.id",
    };
    let mut sql = format!(
        "SELECT {id_col} AS id, m.project, m.session_id, m.summary, m.tags, m.created_at
         FROM memories m
         JOIN memory_meta mm ON mm.memory_id = {id_col}
         WHERE mm.kind = $1"
    );
    let limit_ph = if project.is_some() {
        sql.push_str(" AND m.project = $2");
        "$3"
    } else {
        "$2"
    };
    sql.push_str(&format!(" ORDER BY m.created_at DESC LIMIT {limit_ph}"));

    let mut q = sqlx::query(&sql).bind(clamp_kind(kind));
    if let Some(p) = project {
        q = q.bind(p);
    }
    let rows: Vec<sqlx::any::AnyRow> = q.bind(limit).fetch_all(&db.pool).await?;
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

pub async fn delete_memory_meta(db: &Database, memory_id: i64) -> Result<()> {
    sqlx::query("DELETE FROM memory_meta WHERE memory_id = $1")
        .bind(memory_id)
        .execute(&db.pool)
        .await?;
    Ok(())
}

/// Remove a memory's entity-index rows. Called from `purge_memory` so the
/// inverted index never retains rows for a deleted memory.
pub async fn delete_memory_entities(db: &Database, memory_id: i64) -> Result<()> {
    sqlx::query("DELETE FROM memory_entities WHERE memory_id = $1")
        .bind(memory_id)
        .execute(&db.pool)
        .await?;
    Ok(())
}

/// Link a memory to its verbatim pre-LLM session transcript blob (CCR).
pub async fn set_memory_session_blob(db: &Database, memory_id: i64, hash: &str) -> Result<()> {
    sqlx::query("UPDATE memory_meta SET session_blob = $1 WHERE memory_id = $2")
        .bind(hash)
        .bind(memory_id)
        .execute(&db.pool)
        .await?;
    Ok(())
}

/// The CCR blob hash of a memory's session transcript, if one was stored.
pub async fn get_memory_session_blob(db: &Database, memory_id: i64) -> Result<Option<String>> {
    let row: Option<sqlx::any::AnyRow> =
        sqlx::query("SELECT session_blob FROM memory_meta WHERE memory_id = $1")
            .bind(memory_id)
            .fetch_optional(&db.pool)
            .await?;
    Ok(row.and_then(|r| {
        r.try_get::<Option<String>, _>("session_blob")
            .ok()
            .flatten()
    }))
}

// ── CCR blob store accessors ──────────────────────────────────────────────────

/// A row from the content-addressed `blobs` table.
#[derive(Debug, Clone)]
#[allow(dead_code)] // some fields consumed only by tests / later chunks
pub struct BlobRow {
    pub hash: String,
    pub content_type: String,
    pub codec: String,
    pub orig_len: i64,
    pub comp_len: i64,
    pub data: Vec<u8>,
    pub refcount: i64,
    pub created_at: i64,
    /// Content hash of the dictionary used to compress `data`, if any.
    pub dict_hash: Option<String>,
}

/// Insert a compressed blob, content-addressed by `hash` (hex sha256 of the
/// ORIGINAL bytes). Idempotent: re-inserting the same hash does not duplicate
/// the row or rewrite its bytes — it just bumps the reference count. A fresh
/// row starts at `refcount = 1` (the caller that stored it holds one reference).
#[allow(clippy::too_many_arguments)] // one bind per blobs column; a wrapper struct would just add ceremony
pub async fn insert_blob(
    db: &Database,
    hash: &str,
    content_type: &str,
    codec: &str,
    orig_len: i64,
    comp_len: i64,
    data: &[u8],
    dict_hash: Option<&str>,
) -> Result<()> {
    let now = Utc::now().timestamp();
    sqlx::query(
        "INSERT INTO blobs(hash, content_type, codec, orig_len, comp_len, data, refcount, created_at, dict_hash)
         VALUES($1, $2, $3, $4, $5, $6, 1, $7, $8)
         ON CONFLICT(hash) DO UPDATE SET refcount = refcount + 1",
    )
    .bind(hash)
    .bind(content_type)
    .bind(codec)
    .bind(orig_len)
    .bind(comp_len)
    .bind(data.to_vec())
    .bind(now)
    .bind(dict_hash)
    .execute(&db.pool)
    .await?;
    Ok(())
}

/// Fetch a blob row by its content hash.
pub async fn get_blob(db: &Database, hash: &str) -> Result<Option<BlobRow>> {
    let row: Option<sqlx::any::AnyRow> = sqlx::query(
        "SELECT hash, content_type, codec, orig_len, comp_len, data, refcount, created_at, dict_hash
         FROM blobs WHERE hash = $1",
    )
    .bind(hash)
    .fetch_optional(&db.pool)
    .await?;
    Ok(row.map(|r| BlobRow {
        hash: r.get::<String, _>("hash"),
        content_type: r.get::<String, _>("content_type"),
        codec: r.get::<String, _>("codec"),
        orig_len: r.get::<i64, _>("orig_len"),
        comp_len: r.get::<i64, _>("comp_len"),
        data: r.get::<Vec<u8>, _>("data"),
        refcount: r.get::<i64, _>("refcount"),
        created_at: r.get::<i64, _>("created_at"),
        dict_hash: r.try_get::<Option<String>, _>("dict_hash").ok().flatten(),
    }))
}

/// Store a content-addressed dictionary (idempotent by hash). Dictionaries are
/// stored verbatim (not compressed) so they can always be reconstructed.
pub async fn insert_dict(db: &Database, hash: &str, content_type: &str, data: &[u8]) -> Result<()> {
    let now = Utc::now().timestamp();
    sqlx::query(
        "INSERT INTO ccr_dicts(hash, content_type, data, created_at)
         VALUES($1, $2, $3, $4)
         ON CONFLICT(hash) DO NOTHING",
    )
    .bind(hash)
    .bind(content_type)
    .bind(data.to_vec())
    .bind(now)
    .execute(&db.pool)
    .await?;
    Ok(())
}

/// Fetch dictionary bytes by content hash.
pub async fn get_dict(db: &Database, hash: &str) -> Result<Option<Vec<u8>>> {
    let row: Option<sqlx::any::AnyRow> = sqlx::query("SELECT data FROM ccr_dicts WHERE hash = $1")
        .bind(hash)
        .fetch_optional(&db.pool)
        .await?;
    Ok(row.map(|r| r.get::<Vec<u8>, _>("data")))
}

/// The hash of the most recent dictionary trained for `content_type`, if any.
pub async fn latest_dict_hash(db: &Database, content_type: &str) -> Result<Option<String>> {
    let row: Option<sqlx::any::AnyRow> = sqlx::query(
        "SELECT hash FROM ccr_dicts WHERE content_type = $1 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(content_type)
    .fetch_optional(&db.pool)
    .await?;
    Ok(row.map(|r| r.get::<String, _>("hash")))
}

/// Recent blob hashes of a given content type (newest first) — the sample pool
/// for training a per-type dictionary.
pub async fn recent_blob_hashes_by_type(
    db: &Database,
    content_type: &str,
    limit: i64,
) -> Result<Vec<String>> {
    let rows: Vec<sqlx::any::AnyRow> = sqlx::query(
        "SELECT hash FROM blobs WHERE content_type = $1 ORDER BY created_at DESC LIMIT $2",
    )
    .bind(content_type)
    .bind(limit)
    .fetch_all(&db.pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| r.get::<String, _>("hash"))
        .collect())
}

/// Decrement a blob's reference count, floored at zero. A blob at `refcount = 0`
/// is left in place for `gc_blobs` to reclaim; it is never deleted here.
pub async fn decref_blob(db: &Database, hash: &str) -> Result<()> {
    sqlx::query("UPDATE blobs SET refcount = refcount - 1 WHERE hash = $1 AND refcount > 0")
        .bind(hash)
        .execute(&db.pool)
        .await?;
    Ok(())
}

/// Release a memory's session-transcript blob reference (if any). Must be called
/// while the memory_meta row still exists, i.e. before purging it.
pub async fn decref_memory_session_blob(db: &Database, memory_id: i64) -> Result<()> {
    if let Some(hash) = get_memory_session_blob(db, memory_id).await? {
        decref_blob(db, &hash).await?;
    }
    Ok(())
}

/// Delete every blob with no remaining references. Returns
/// `(blobs_removed, compressed_bytes_freed)`.
pub async fn gc_blobs(db: &Database) -> Result<(i64, i64)> {
    let r: sqlx::any::AnyRow = sqlx::query(
        "SELECT COUNT(*) AS cnt, COALESCE(SUM(comp_len), 0) AS bytes FROM blobs WHERE refcount <= 0",
    )
    .fetch_one(&db.pool)
    .await?;
    let count: i64 = r.get("cnt");
    let bytes: i64 = r.get("bytes");
    sqlx::query("DELETE FROM blobs WHERE refcount <= 0")
        .execute(&db.pool)
        .await?;
    Ok((count, bytes))
}

/// Memory ids (rowid in sqlite / id in pg) with no embedding row for `model`,
/// along with their summary + tags (the text to embed).
pub async fn memory_ids_missing_embedding(
    db: &Database,
    model: &str,
    project: Option<&str>,
) -> Result<Vec<(i64, String, Option<String>)>> {
    let id_col = match db.backend {
        Backend::Sqlite => "rowid",
        Backend::Postgres => "id",
    };
    let mut sql = format!(
        "SELECT m.{id_col} AS id, m.summary AS summary, m.tags AS tags FROM memories m
         WHERE NOT EXISTS (
            SELECT 1 FROM embeddings e
            WHERE e.owner_type = 'memory' AND e.owner_id = m.{id_col} AND e.model = $1
         )"
    );
    if project.is_some() {
        sql.push_str(" AND m.project = $2");
    }
    let mut q = sqlx::query(&sql).bind(model);
    if let Some(p) = project {
        q = q.bind(p);
    }
    let rows = q.fetch_all(&db.pool).await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            (
                r.get::<i64, _>("id"),
                r.get::<String, _>("summary"),
                r.try_get::<Option<String>, _>("tags").ok().flatten(),
            )
        })
        .collect())
}

/// Fetch a single memory by id (rowid in sqlite / id in pg). Used by hybrid
/// search to materialize vector-only hits in their fused order.
pub async fn get_memory_by_id(db: &Database, id: i64) -> Result<Option<Memory>> {
    let query_str = match db.backend {
        Backend::Sqlite => {
            "SELECT rowid as id, project, session_id, summary, tags, created_at
             FROM memories WHERE rowid = $1"
        }
        Backend::Postgres => {
            "SELECT id, project, session_id, summary, tags, created_at
             FROM memories WHERE id = $1"
        }
    };
    let row: Option<sqlx::any::AnyRow> = sqlx::query(query_str)
        .bind(id)
        .fetch_optional(&db.pool)
        .await?;
    Ok(row.map(|r| Memory {
        id: r.get("id"),
        project: r.get("project"),
        session_id: r.get("session_id"),
        summary: r.get("summary"),
        tags: r.try_get("tags").ok().flatten(),
        created_at: r.get("created_at"),
    }))
}

/// All memory ids + their text (for `embed --force` full re-index).
pub async fn all_memory_ids_with_text(
    db: &Database,
    project: Option<&str>,
) -> Result<Vec<(i64, String, Option<String>)>> {
    let id_col = match db.backend {
        Backend::Sqlite => "rowid",
        Backend::Postgres => "id",
    };
    let mut sql =
        format!("SELECT m.{id_col} AS id, m.summary AS summary, m.tags AS tags FROM memories m");
    if project.is_some() {
        sql.push_str(" WHERE m.project = $1");
    }
    let mut q = sqlx::query(&sql);
    if let Some(p) = project {
        q = q.bind(p);
    }
    let rows = q.fetch_all(&db.pool).await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            (
                r.get::<i64, _>("id"),
                r.get::<String, _>("summary"),
                r.try_get::<Option<String>, _>("tags").ok().flatten(),
            )
        })
        .collect())
}

// Stats

pub struct DbStats {
    pub total_sessions: i64,
    pub total_memories: i64,
    pub total_observations: i64,
    pub total_memory_edges: i64,
    /// Distinct CCR blobs stored.
    pub ccr_blobs: i64,
    /// Sum of original (uncompressed) bytes across distinct blobs.
    pub ccr_orig_bytes: i64,
    /// Sum of stored compressed bytes across distinct blobs.
    pub ccr_comp_bytes: i64,
    /// Sum of original bytes weighted by refcount — what would have been stored
    /// uncompressed and without dedup. `logical / orig` is the dedup factor.
    pub ccr_logical_bytes: i64,
}

impl DbStats {
    /// CCR storage stats as JSON: blob count, original vs stored bytes,
    /// compression %, dedup factor, and total bytes saved vs naive storage.
    pub fn ccr_json(&self) -> serde_json::Value {
        let comp_pct = if self.ccr_orig_bytes > 0 {
            100.0 * (1.0 - self.ccr_comp_bytes as f64 / self.ccr_orig_bytes as f64)
        } else {
            0.0
        };
        let dedup_factor = if self.ccr_orig_bytes > 0 {
            self.ccr_logical_bytes as f64 / self.ccr_orig_bytes as f64
        } else {
            1.0
        };
        serde_json::json!({
            "blobs": self.ccr_blobs,
            "original_bytes": self.ccr_orig_bytes,
            "stored_bytes": self.ccr_comp_bytes,
            "logical_bytes": self.ccr_logical_bytes,
            "compression_pct": (comp_pct * 10.0).round() / 10.0,
            "dedup_factor": (dedup_factor * 100.0).round() / 100.0,
            "bytes_saved": self.ccr_logical_bytes - self.ccr_comp_bytes,
        })
    }
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

    let r: sqlx::any::AnyRow = sqlx::query("SELECT COUNT(*) as cnt FROM memory_edges")
        .fetch_one(&db.pool)
        .await?;
    let memory_edges: i64 = r.get("cnt");

    let r: sqlx::any::AnyRow = sqlx::query(
        "SELECT COUNT(*) AS cnt,
                COALESCE(SUM(orig_len), 0) AS orig,
                COALESCE(SUM(comp_len), 0) AS comp,
                COALESCE(SUM(orig_len * refcount), 0) AS logical
         FROM blobs",
    )
    .fetch_one(&db.pool)
    .await?;

    Ok(DbStats {
        total_sessions: sessions,
        total_memories: memories,
        total_observations: observations,
        total_memory_edges: memory_edges,
        ccr_blobs: r.get("cnt"),
        ccr_orig_bytes: r.get("orig"),
        ccr_comp_bytes: r.get("comp"),
        ccr_logical_bytes: r.get("logical"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: `insert_memory` must return the rowid of the row it just
    /// inserted. Before the fix it read `last_insert_rowid()` on a *separate*
    /// pool query, which on a multi-connection pool could land on another
    /// connection and return a wrong/zero/duplicate id. We interleave unrelated
    /// pool queries to churn connection hand-out and assert the invariant.
    #[tokio::test]
    async fn insert_memory_returns_correct_distinct_ids() {
        let path = std::env::temp_dir().join(format!("ironmem-insid-{}.db", uuid::Uuid::new_v4()));
        let db = Database::new(&path.to_string_lossy()).await.unwrap();
        db.migrate().await.unwrap();
        let session = create_session(&db, "/tmp/p").await.unwrap();

        let mut ids = Vec::new();
        for i in 0..5 {
            // Unrelated pool query between inserts to encourage the pool to
            // hand out a different connection (the conditions that exposed the bug).
            let _ = get_recent_memories(&db, "/tmp/p", 10).await.unwrap();
            let id = insert_memory(&db, "/tmp/p", &session, &format!("summary {i}"), Some("t"))
                .await
                .unwrap();
            ids.push(id);
        }

        // All ids are non-zero and distinct.
        assert!(
            ids.iter().all(|&id| id > 0),
            "ids must be non-zero: {ids:?}"
        );
        let mut distinct = ids.clone();
        distinct.sort();
        distinct.dedup();
        assert_eq!(distinct.len(), ids.len(), "ids must be distinct: {ids:?}");

        // Each returned id maps to exactly the row we inserted under it.
        for (i, &id) in ids.iter().enumerate() {
            let m = get_memory_by_id(&db, id)
                .await
                .unwrap()
                .unwrap_or_else(|| panic!("no memory for returned id {id}"));
            assert_eq!(m.summary, format!("summary {i}"));
        }

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn sqlite_file_url_formats_windows_and_unix_paths() {
        assert_eq!(
            sqlite_file_url(std::path::Path::new("/tmp/ironmem.db")),
            "sqlite:///tmp/ironmem.db?mode=rwc"
        );
        assert_eq!(
            sqlite_file_url(std::path::Path::new(r"C:\Users\runneradmin\ironmem.db")),
            "sqlite:///C:/Users/runneradmin/ironmem.db?mode=rwc"
        );
    }

    #[test]
    fn fts5_match_query_sanitizes_punctuation() {
        assert_eq!(
            fts5_match_query("When did Caroline go to the LGBTQ group?"),
            "\"When\" OR \"did\" OR \"Caroline\" OR \"go\" OR \"to\" OR \"the\" OR \"LGBTQ\" OR \"group\""
        );
        // All-punctuation input yields an empty match (callers short-circuit).
        assert_eq!(fts5_match_query("???"), "");
        assert_eq!(fts5_match_query(""), "");
    }

    /// Regression: a natural question with a trailing '?' used to raise an FTS5
    /// syntax error (swallowed into an empty result), silently breaking search.
    #[tokio::test]
    async fn search_memories_tolerates_question_punctuation() -> Result<()> {
        let (db, path) = test_db().await?;
        let s = create_session(&db, "/p").await?;
        insert_memory(
            &db,
            "/p",
            &s,
            "Caroline joined the LGBTQ support group on 7 May 2023",
            Some("t"),
        )
        .await?;
        let hits =
            search_memories(&db, "/p", "When did Caroline join the LGBTQ group?", 10).await?;
        assert!(
            hits.iter().any(|m| m.summary.contains("LGBTQ")),
            "expected the LGBTQ memory, got {hits:?}"
        );
        let _ = std::fs::remove_file(path);
        Ok(())
    }

    async fn test_db() -> Result<(Database, String)> {
        let db_path =
            std::env::temp_dir().join(format!("ironmem-test-{}.db", uuid::Uuid::new_v4()));
        let db_path_string = db_path.to_string_lossy().to_string();
        let db = Database::new(&db_path_string).await?;
        db.migrate().await?;
        Ok((db, db_path_string))
    }

    #[tokio::test]
    async fn sqlite_vec_extension_loads_and_knn_runs() -> Result<()> {
        let (db, path) = test_db().await?;
        sqlx::query(
            "CREATE VIRTUAL TABLE IF NOT EXISTS vt_smoke USING vec0(id INTEGER PRIMARY KEY, embedding float[3])",
        )
        .execute(&db.pool)
        .await?;
        let blob =
            crate::embedding_codec::encode(&crate::embedding_codec::normalize(&[1.0, 0.0, 0.0]));
        sqlx::query("INSERT INTO vt_smoke(id, embedding) VALUES (1, ?)")
            .bind(blob.clone())
            .execute(&db.pool)
            .await?;
        let rows: Vec<sqlx::any::AnyRow> = sqlx::query(
            "SELECT id, distance FROM vt_smoke WHERE embedding MATCH ? AND k = 1 ORDER BY distance",
        )
        .bind(blob)
        .fetch_all(&db.pool)
        .await?;
        assert_eq!(rows.len(), 1);
        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[tokio::test]
    async fn embeddings_meta_and_ann_roundtrip() -> Result<()> {
        let (db, path) = test_db().await?;
        let s = create_session(&db, "/tmp/p").await?;
        insert_memory(&db, "/tmp/p", &s, "auth middleware fix", Some("auth")).await?;

        let emb =
            crate::embedding_codec::encode(&crate::embedding_codec::normalize(&[1.0, 0.0, 0.0]));
        upsert_embedding(&db, "memory", 1, "m", 3, &emb).await?;
        assert_eq!(
            get_embedding(&db, "memory", 1, "m").await?,
            Some(emb.clone())
        );

        // meta: default when absent, then upsert
        assert_eq!(get_memory_meta(&db, 999).await?, 0.5);
        upsert_memory_meta(&db, 1, 0.8).await?;
        assert!((get_memory_meta(&db, 1).await? - 0.8).abs() < 1e-9);

        // ANN table usable
        db.ensure_ann(3).await?;
        sqlx::query("INSERT INTO vec_memories(memory_id, embedding) VALUES (1, ?)")
            .bind(emb)
            .execute(&db.pool)
            .await?;

        // missing-embedding listing keys off the model
        let missing = memory_ids_missing_embedding(&db, "other", None).await?;
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].0, 1);
        assert!(memory_ids_missing_embedding(&db, "m", None)
            .await?
            .is_empty());

        // delete cleans up
        delete_embedding(&db, "memory", 1).await?;
        delete_memory_meta(&db, 1).await?;
        assert_eq!(get_embedding(&db, "memory", 1, "m").await?, None);

        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[tokio::test]
    async fn blobs_insert_is_idempotent_and_refcounts() -> Result<()> {
        let (db, path) = test_db().await?;

        // First insert creates the row at refcount 1 and stores all fields.
        insert_blob(
            &db,
            "abc123",
            "text",
            "zstd",
            10,
            4,
            b"\x01\x02\x03\x04",
            None,
        )
        .await?;
        let row = get_blob(&db, "abc123")
            .await?
            .expect("row exists after insert");
        assert_eq!(row.hash, "abc123");
        assert_eq!(row.content_type, "text");
        assert_eq!(row.codec, "zstd");
        assert_eq!(row.orig_len, 10);
        assert_eq!(row.comp_len, 4);
        assert_eq!(row.data, b"\x01\x02\x03\x04");
        assert_eq!(row.refcount, 1);

        // Re-inserting the same hash dedups (single row) and bumps the refcount.
        insert_blob(
            &db,
            "abc123",
            "text",
            "zstd",
            10,
            4,
            b"\x01\x02\x03\x04",
            None,
        )
        .await?;
        assert_eq!(get_blob(&db, "abc123").await?.unwrap().refcount, 2);

        // decref to 0 leaves the row in place for gc_blobs to reclaim.
        decref_blob(&db, "abc123").await?; // 1
        decref_blob(&db, "abc123").await?; // 0
        let row = get_blob(&db, "abc123")
            .await?
            .expect("row still present at refcount 0 (left for GC)");
        assert_eq!(row.refcount, 0);

        // Decref is floored at zero — never goes negative.
        decref_blob(&db, "abc123").await?;
        assert_eq!(get_blob(&db, "abc123").await?.unwrap().refcount, 0);

        // Unknown hash → None.
        assert!(get_blob(&db, "does-not-exist").await?.is_none());

        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[tokio::test]
    async fn ccr_dicts_store_fetch_and_blob_dict_hash() -> Result<()> {
        let (db, path) = test_db().await?;

        insert_dict(&db, "h1", "json", b"dictionary-bytes-1").await?;
        assert_eq!(
            get_dict(&db, "h1").await?,
            Some(b"dictionary-bytes-1".to_vec())
        );
        assert!(get_dict(&db, "missing").await?.is_none());
        assert_eq!(latest_dict_hash(&db, "json").await?, Some("h1".to_string()));
        assert_eq!(latest_dict_hash(&db, "log").await?, None);

        // A blob records which dictionary compressed it.
        insert_blob(&db, "b1", "json", "dict+zstd", 100, 40, b"zzz", Some("h1")).await?;
        assert_eq!(
            get_blob(&db, "b1").await?.unwrap().dict_hash,
            Some("h1".to_string())
        );
        // A dict-less blob has no dict_hash.
        insert_blob(&db, "b2", "text", "zstd", 10, 8, b"plain", None).await?;
        assert_eq!(get_blob(&db, "b2").await?.unwrap().dict_hash, None);

        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[tokio::test]
    async fn gc_reclaims_only_unreferenced_blobs() -> Result<()> {
        let (db, path) = test_db().await?;
        insert_blob(&db, "g1", "text", "zstd", 5, 3, b"abc", None).await?; // rc 1
        insert_blob(&db, "g2", "text", "zstd", 5, 3, b"xyz", None).await?; // rc 1
        insert_blob(&db, "shared", "text", "zstd", 5, 3, b"sha", None).await?; // rc 1
        insert_blob(&db, "shared", "text", "zstd", 5, 3, b"sha", None).await?; // rc 2

        assert_eq!(gc_blobs(&db).await?.0, 0);

        decref_blob(&db, "g1").await?;
        assert_eq!(gc_blobs(&db).await?.0, 1);
        assert!(get_blob(&db, "g1").await?.is_none());
        assert!(get_blob(&db, "g2").await?.is_some());

        // Shared blob survives until the last reference is released.
        decref_blob(&db, "shared").await?;
        assert_eq!(gc_blobs(&db).await?.0, 0);
        assert!(get_blob(&db, "shared").await?.is_some());
        decref_blob(&db, "shared").await?;
        assert_eq!(gc_blobs(&db).await?.0, 1);
        assert!(get_blob(&db, "shared").await?.is_none());

        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[tokio::test]
    async fn deleting_memory_releases_its_session_blob() -> Result<()> {
        let (db, path) = test_db().await?;
        let s = create_session(&db, "/tmp/p").await?;
        let mid = insert_memory(&db, "/tmp/p", &s, "sum", None).await?;
        upsert_memory_meta(&db, mid, 0.5).await?;
        insert_blob(&db, "tx", "text", "zstd", 10, 5, b"hello", None).await?; // rc 1
        set_memory_session_blob(&db, mid, "tx").await?;
        assert_eq!(
            get_memory_session_blob(&db, mid).await?,
            Some("tx".to_string())
        );

        decref_memory_session_blob(&db, mid).await?;
        assert_eq!(gc_blobs(&db).await?.0, 1);
        assert!(get_blob(&db, "tx").await?.is_none());

        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[tokio::test]
    async fn ccr_stats_report_blob_totals_and_dedup() -> Result<()> {
        let (db, path) = test_db().await?;
        insert_blob(&db, "s1", "json", "zstd", 100, 40, b"....", None).await?; // rc 1
        insert_blob(&db, "s2", "json", "zstd", 200, 50, b"....", None).await?; // rc 1
        insert_blob(&db, "s2", "json", "zstd", 200, 50, b"....", None).await?; // dedup → rc 2

        let stats = get_stats(&db).await?;
        assert_eq!(stats.ccr_blobs, 2);
        assert_eq!(stats.ccr_orig_bytes, 300);
        assert_eq!(stats.ccr_comp_bytes, 90);
        assert_eq!(stats.ccr_logical_bytes, 100 + 200 * 2); // refcount-weighted

        let json = stats.ccr_json();
        assert_eq!(json["blobs"], 2);
        assert_eq!(json["bytes_saved"], 500 - 90);

        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[test]
    fn clamp_kind_and_scope_normalize_to_known_sets() {
        assert_eq!(clamp_kind("error_solution"), "error_solution");
        assert_eq!(clamp_kind("  PREFERENCE  "), "preference");
        assert_eq!(clamp_kind("procedural"), "procedural");
        assert_eq!(clamp_kind("not-a-kind"), "session");
        assert_eq!(clamp_kind(""), "session");
        assert_eq!(clamp_scope("user"), "user");
        assert_eq!(clamp_scope("USER"), "user");
        assert_eq!(clamp_scope("project"), "project");
        assert_eq!(clamp_scope("garbage"), "project");
    }

    #[tokio::test]
    async fn event_time_round_trips_and_queries() -> Result<()> {
        let (db, path) = test_db().await?;
        let p = "/tmp/temporal";
        let s = create_session(&db, p).await?;

        // One dated memory, one undated, same project.
        let dated = insert_memory(&db, p, &s, "Caroline joined a support group", Some("t")).await?;
        set_memory_event_time(&db, dated, "2023-05-07").await?;
        let undated = insert_memory(&db, p, &s, "some other note", Some("t")).await?;

        // event_time round-trips through the meta read; undated reads as None.
        let info = get_memory_meta_full(&db, dated).await?;
        assert_eq!(info.event_time.as_deref(), Some("2023-05-07"));
        assert!(get_memory_meta_full(&db, undated)
            .await?
            .event_time
            .is_none());

        // Year query matches only the dated memory; a non-matching year finds none.
        assert_eq!(
            memories_by_event_time(&db, Some(p), "2023", 10).await?,
            vec![dated]
        );
        assert!(memories_by_event_time(&db, Some(p), "1999", 10)
            .await?
            .is_empty());
        // Project scoping: a different project sees nothing.
        assert!(memories_by_event_time(&db, Some("/tmp/other"), "2023", 10)
            .await?
            .is_empty());

        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[tokio::test]
    async fn memory_entities_insert_and_lookup() -> Result<()> {
        let (db, path) = test_db().await?;
        let p = "/tmp/ent";
        let s = create_session(&db, p).await?;
        let caro = insert_memory(&db, p, &s, "Caroline did things in New York", Some("t")).await?;
        insert_memory_entity(&db, caro, "Caroline").await?;
        insert_memory_entity(&db, caro, "New York").await?;

        // Case-insensitive single-token lookup returns the id.
        assert_eq!(
            memories_for_entity(&db, Some(p), "caroline", 10).await?,
            vec![caro]
        );
        assert_eq!(
            memories_for_entity(&db, Some(p), "CAROLINE", 10).await?,
            vec![caro]
        );
        // Either token of a multi-word entity resolves.
        assert_eq!(
            memories_for_entity(&db, Some(p), "York", 10).await?,
            vec![caro]
        );
        // Unknown entity / too-short token / wrong project ⇒ nothing.
        assert!(memories_for_entity(&db, Some(p), "Melanie", 10)
            .await?
            .is_empty());
        assert!(memories_for_entity(&db, Some(p), "of", 10)
            .await?
            .is_empty());
        assert!(memories_for_entity(&db, Some("/tmp/other"), "caroline", 10)
            .await?
            .is_empty());

        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[tokio::test]
    async fn memory_edges_insert_and_query_active_by_entity() -> Result<()> {
        let (db, path) = test_db().await?;
        let p = "/tmp/edge";
        let s = create_session(&db, p).await?;
        let mid = insert_memory(&db, p, &s, "Caroline works at Acme", Some("t")).await?;

        let edge_id = insert_memory_edge(
            &db,
            &NewMemoryEdge {
                project: p.into(),
                memory_id: mid,
                source: "Caroline".into(),
                relation: "works at".into(),
                target: "Acme Corp".into(),
                valid_from: Some("2026-01-01".into()),
                valid_until: None,
                confidence: 0.92,
            },
        )
        .await?;

        let edges = memory_edges_for_entity(&db, Some(p), "Caroline", false, 10).await?;
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].id, edge_id);
        assert_eq!(edges[0].source, "Caroline");
        assert_eq!(edges[0].relation, "works at");
        assert_eq!(edges[0].target, "Acme Corp");
        assert_eq!(edges[0].valid_from.as_deref(), Some("2026-01-01"));
        assert!(edges[0].superseded_by.is_none());

        let by_memory = memory_edges_for_memory(&db, mid).await?;
        assert_eq!(by_memory.len(), 1);
        assert_eq!(by_memory[0].id, edge_id);

        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[tokio::test]
    async fn memory_edges_reconcile_duplicates_and_current_state_updates() -> Result<()> {
        let (db, path) = test_db().await?;
        let p = "/tmp/edge-reconcile";
        let s = create_session(&db, p).await?;
        let first = insert_memory(&db, p, &s, "Caroline status draft", Some("t")).await?;
        let second =
            insert_memory(&db, p, &s, "Caroline status draft duplicate", Some("t")).await?;
        let third = insert_memory(&db, p, &s, "Caroline status approved", Some("t")).await?;

        let e1 = insert_memory_edge(
            &db,
            &NewMemoryEdge {
                project: p.into(),
                memory_id: first,
                source: "Caroline".into(),
                relation: "status".into(),
                target: "draft".into(),
                valid_from: Some("2026-06-01".into()),
                valid_until: None,
                confidence: 0.8,
            },
        )
        .await?;
        let e2 = insert_memory_edge(
            &db,
            &NewMemoryEdge {
                project: p.into(),
                memory_id: second,
                source: "Caroline".into(),
                relation: "status".into(),
                target: "draft".into(),
                valid_from: Some("2026-06-01".into()),
                valid_until: None,
                confidence: 0.9,
            },
        )
        .await?;
        let e3 = insert_memory_edge(
            &db,
            &NewMemoryEdge {
                project: p.into(),
                memory_id: third,
                source: "Caroline".into(),
                relation: "status".into(),
                target: "approved".into(),
                valid_from: Some("2026-06-05".into()),
                valid_until: None,
                confidence: 0.95,
            },
        )
        .await?;

        let active = memory_edges_for_entity(&db, Some(p), "Caroline", false, 10).await?;
        assert_eq!(
            active.len(),
            1,
            "only the latest current-state edge stays active"
        );
        assert_eq!(active[0].id, e3);
        assert_eq!(active[0].target, "approved");

        let history = memory_edges_for_entity(&db, Some(p), "Caroline", true, 10).await?;
        let old_duplicate = history.iter().find(|e| e.id == e1).unwrap();
        assert_eq!(old_duplicate.superseded_by, Some(e2));
        assert_eq!(
            old_duplicate.superseded_reason.as_deref(),
            Some("duplicate")
        );

        let replaced = history.iter().find(|e| e.id == e2).unwrap();
        assert_eq!(replaced.superseded_by, Some(e3));
        assert_eq!(
            replaced.superseded_reason.as_deref(),
            Some("current_state_update")
        );
        assert_eq!(replaced.valid_until.as_deref(), Some("2026-06-05"));

        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[tokio::test]
    async fn reconcile_memory_graph_dry_run_then_marks_legacy_edges() -> Result<()> {
        let (db, path) = test_db().await?;
        let p = "/tmp/reconcile-scan";
        let s = create_session(&db, p).await?;
        let first = insert_memory(&db, p, &s, "Caroline status draft", Some("t")).await?;
        let second = insert_memory(&db, p, &s, "Caroline status draft again", Some("t")).await?;
        let third = insert_memory(&db, p, &s, "Caroline status approved", Some("t")).await?;

        let e1 = insert_memory_edge(
            &db,
            &NewMemoryEdge {
                project: p.into(),
                memory_id: first,
                source: "Caroline".into(),
                relation: "status".into(),
                target: "draft".into(),
                valid_from: Some("2026-06-01".into()),
                valid_until: None,
                confidence: 0.8,
            },
        )
        .await?;
        let e2 = insert_memory_edge(
            &db,
            &NewMemoryEdge {
                project: p.into(),
                memory_id: second,
                source: "Caroline".into(),
                relation: "status".into(),
                target: "draft".into(),
                valid_from: Some("2026-06-01".into()),
                valid_until: None,
                confidence: 0.9,
            },
        )
        .await?;
        let e3 = insert_memory_edge(
            &db,
            &NewMemoryEdge {
                project: p.into(),
                memory_id: third,
                source: "Caroline".into(),
                relation: "status".into(),
                target: "approved".into(),
                valid_from: Some("2026-06-05".into()),
                valid_until: None,
                confidence: 0.95,
            },
        )
        .await?;

        // Simulate legacy/imported graph rows that never went through
        // insert-time reconciliation.
        sqlx::query(
            "UPDATE memory_edges
             SET superseded_by = NULL, superseded_reason = NULL, valid_until = NULL",
        )
        .execute(&db.pool)
        .await?;

        let dry = reconcile_memory_graph(&db, Some(p), true).await?;
        assert_eq!(dry.scanned, 3);
        assert_eq!(dry.duplicates, 1);
        assert_eq!(dry.current_state_updates, 1);
        assert_eq!(dry.active_edges, 1);
        assert!(dry.dry_run);
        assert!(memory_edges_for_entity(&db, Some(p), "Caroline", false, 10)
            .await?
            .iter()
            .all(|e| e.superseded_by.is_none()));

        let report = reconcile_memory_graph(&db, Some(p), false).await?;
        assert_eq!(report.duplicates, 1);
        assert_eq!(report.current_state_updates, 1);
        assert_eq!(report.active_edges, 1);

        let history = memory_edges_for_entity(&db, Some(p), "Caroline", true, 10).await?;
        assert_eq!(
            history.iter().find(|e| e.id == e1).unwrap().superseded_by,
            Some(e2)
        );
        assert_eq!(
            history.iter().find(|e| e.id == e2).unwrap().superseded_by,
            Some(e3)
        );

        let current =
            memory_edges_for_entity_at(&db, Some(p), "Caroline", false, Some("2026-06-06"), 10)
                .await?;
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].id, e3);
        let past =
            memory_edges_for_entity_at(&db, Some(p), "Caroline", true, Some("2026-06-03"), 10)
                .await?;
        assert!(
            past.iter().any(|e| e.target == "draft"),
            "history at 2026-06-03 should include draft state: {past:?}"
        );

        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[tokio::test]
    async fn memories_without_edges_skips_existing_graph_provenance() -> Result<()> {
        let (db, path) = test_db().await?;
        let p = "/tmp/backfill";
        let s = create_session(&db, p).await?;
        let with_edge = insert_memory(&db, p, &s, "Caroline uses IronMem", Some("a")).await?;
        let missing = insert_memory(&db, p, &s, "Operator OS uses IronMem", Some("b")).await?;
        insert_memory_edge(
            &db,
            &NewMemoryEdge {
                project: p.into(),
                memory_id: with_edge,
                source: "Caroline".into(),
                relation: "uses".into(),
                target: "IronMem".into(),
                valid_from: None,
                valid_until: None,
                confidence: 0.8,
            },
        )
        .await?;

        let candidates = memories_without_edges(&db, Some(p), 10).await?;
        assert_eq!(
            candidates.iter().map(|m| m.id).collect::<Vec<_>>(),
            vec![missing]
        );

        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[tokio::test]
    async fn memory_scope_kind_defaults_roundtrip_and_filter() -> Result<()> {
        let (db, path) = test_db().await?;
        let alpha = "/tmp/alpha";
        let beta = "/tmp/beta";
        let sa = create_session(&db, alpha).await?;
        let sb = create_session(&db, beta).await?;

        // m1: alpha, explicit meta row → defaults project/session.
        let m1 = insert_memory(&db, alpha, &sa, "alpha session work", Some("a")).await?;
        upsert_memory_meta(&db, m1, 0.5).await?;
        // m2: alpha, promoted to a user-scope preference.
        let m2 = insert_memory(&db, alpha, &sa, "user prefers tabs", Some("pref")).await?;
        set_memory_scope_kind(&db, m2, "user", "preference").await?;
        // m3: beta, user-scope profile.
        let m3 = insert_memory(&db, beta, &sb, "user is a rust dev", Some("profile")).await?;
        set_memory_scope_kind(&db, m3, "user", "profile").await?;
        // m4: beta, NO meta row at all (legacy) → must read as project scope.
        let m4 = insert_memory(&db, beta, &sb, "beta legacy memory", None).await?;

        // Defaults: a plain upsert_memory_meta row is project/session.
        let i1 = get_memory_meta_full(&db, m1).await?;
        assert_eq!(
            (i1.scope.as_str(), i1.kind.as_str()),
            ("project", "session")
        );
        // Round-trip of explicit scope/kind.
        let i2 = get_memory_meta_full(&db, m2).await?;
        assert_eq!(
            (i2.scope.as_str(), i2.kind.as_str()),
            ("user", "preference")
        );
        // set_memory_scope_kind upserted a row even though none existed before,
        // with the default importance preserved.
        assert!((i2.importance - 0.5).abs() < 1e-9);
        // Missing row → defaults.
        let none = get_memory_meta_full(&db, 9999).await?;
        assert_eq!(
            (none.scope.as_str(), none.kind.as_str()),
            ("project", "session")
        );

        // User-scope query is global (ignores project) → m2 + m3.
        let users = get_recent_memories_scoped(&db, "user", None, 50).await?;
        let user_ids: Vec<i64> = users.iter().map(|m| m.id).collect();
        assert!(
            user_ids.contains(&m2) && user_ids.contains(&m3),
            "{user_ids:?}"
        );
        assert!(
            !user_ids.contains(&m1) && !user_ids.contains(&m4),
            "{user_ids:?}"
        );

        // Project-scope for beta → m4 (legacy, no meta) but NOT m3 (user-scope).
        let beta_proj = get_recent_memories_scoped(&db, "project", Some(beta), 50).await?;
        let beta_ids: Vec<i64> = beta_proj.iter().map(|m| m.id).collect();
        assert!(
            beta_ids.contains(&m4),
            "legacy memory must count as project: {beta_ids:?}"
        );
        assert!(
            !beta_ids.contains(&m3),
            "user-scope excluded from project: {beta_ids:?}"
        );

        // Project-scope for alpha → m1 only (m2 is user-scope).
        let alpha_proj = get_recent_memories_scoped(&db, "project", Some(alpha), 50).await?;
        let alpha_ids: Vec<i64> = alpha_proj.iter().map(|m| m.id).collect();
        assert!(
            alpha_ids.contains(&m1) && !alpha_ids.contains(&m2),
            "{alpha_ids:?}"
        );

        // set_memory_scope_kind clamps unknown values.
        set_memory_scope_kind(&db, m1, "bogus-scope", "bogus-kind").await?;
        let i1b = get_memory_meta_full(&db, m1).await?;
        assert_eq!(
            (i1b.scope.as_str(), i1b.kind.as_str()),
            ("project", "session")
        );

        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[tokio::test]
    async fn get_memories_by_kind_filters_by_kind_and_project() -> Result<()> {
        let (db, path) = test_db().await?;
        let a = "/tmp/a";
        let b = "/tmp/b";
        let sa = create_session(&db, a).await?;
        let sb = create_session(&db, b).await?;

        let e1 = insert_memory(&db, a, &sa, "Error: x failed; Fix: edited y", Some("fix")).await?;
        set_memory_scope_kind(&db, e1, "project", "error_solution").await?;
        let e2 = insert_memory(&db, b, &sb, "Error: z failed; Fix: edited w", Some("fix")).await?;
        set_memory_scope_kind(&db, e2, "project", "error_solution").await?;
        let plain = insert_memory(&db, a, &sa, "ordinary session", Some("s")).await?;
        set_memory_scope_kind(&db, plain, "project", "session").await?;

        // Project-scoped: only a's error_solution.
        let a_fixes = get_memories_by_kind(&db, Some(a), "error_solution", 10).await?;
        let a_ids: Vec<i64> = a_fixes.iter().map(|m| m.id).collect();
        assert_eq!(a_ids, vec![e1]);

        // Global: both error_solutions, not the plain session.
        let all_fixes = get_memories_by_kind(&db, None, "error_solution", 10).await?;
        let all_ids: Vec<i64> = all_fixes.iter().map(|m| m.id).collect();
        assert!(all_ids.contains(&e1) && all_ids.contains(&e2));
        assert!(!all_ids.contains(&plain));

        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[tokio::test]
    async fn project_discovery_queries_return_cross_project_results() -> Result<()> {
        let (db, db_path) = test_db().await?;

        let alpha = "/tmp/alpha";
        let beta = "/tmp/beta";

        let alpha_session = create_session(&db, alpha).await?;
        insert_observation(
            &db,
            &alpha_session,
            alpha,
            "Read",
            Some("file"),
            Some("notes about auth middleware"),
            1024,
        )
        .await?;
        end_session(&db, &alpha_session).await?;
        insert_memory(
            &db,
            alpha,
            &alpha_session,
            "Fixed auth middleware bug and updated tunnel docs",
            Some("auth,docs"),
        )
        .await?;

        let beta_session = create_session(&db, beta).await?;
        insert_observation(
            &db,
            &beta_session,
            beta,
            "Edit",
            Some("search"),
            Some("global search plan"),
            1024,
        )
        .await?;
        end_session(&db, &beta_session).await?;
        insert_memory(
            &db,
            beta,
            &beta_session,
            "Added project discovery and global search ideas",
            Some("search,discovery"),
        )
        .await?;

        let projects = list_projects(&db, 10).await?;
        assert_eq!(projects.len(), 2);
        assert!(projects
            .iter()
            .any(|p| p.project == alpha && p.memory_count == 1));
        assert!(projects
            .iter()
            .any(|p| p.project == beta && p.memory_count == 1));

        let global = search_all_memories(&db, "auth", 10).await?;
        assert_eq!(global.len(), 1);
        assert_eq!(global[0].project, alpha);

        let sessions = list_session_history(&db, alpha, 10).await?;
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].project, alpha);
        assert_eq!(sessions[0].observation_count, 1);
        assert_eq!(sessions[0].tags.as_deref(), Some("auth,docs"));

        let _ = std::fs::remove_file(db_path);
        Ok(())
    }

    #[tokio::test]
    async fn insert_observation_truncates_multibyte_without_panicking() -> Result<()> {
        let (db, path) = test_db().await?;
        let s = create_session(&db, "/tmp/p").await?;

        // 'a'..'g' = 7 ASCII bytes, then '✓' (3 bytes) starts at byte 7, so a
        // cap of 8 lands in the MIDDLE of '✓'. The old `&o[..8]` slice panicked
        // here; under release `panic="abort"` that kills the MCP process.
        let output = "abcdefg✓✓✓✓✓ tail";
        let id = insert_observation(&db, &s, "/tmp/p", "Read", None, Some(output), 8).await?;
        assert!(id > 0, "insert must succeed, not panic");

        let obs = get_observations_for_session(&db, &s).await?;
        assert_eq!(obs.len(), 1);
        let stored = obs[0].output.as_deref().unwrap();
        assert!(stored.starts_with("abcdefg"));
        assert!(stored.ends_with("… [truncated]"));

        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[tokio::test]
    async fn insert_observation_backs_large_output_with_lossless_blob() -> Result<()> {
        let (db, path) = test_db().await?;
        let s = create_session(&db, "/tmp/p").await?;

        // ~66 KB of varied multibyte content; cap the inline preview at 2048 B.
        let big = "héllo ✓ wörld 日本語 🦀 ".repeat(2000);
        assert!(big.len() > 50_000);
        let id = insert_observation(&db, &s, "/tmp/p", "Read", None, Some(&big), 2048).await?;

        // Inline output is just a truncated preview, not the full original.
        let obs = get_observations_for_session(&db, &s).await?;
        let preview = obs[0].output.as_deref().unwrap();
        assert!(preview.len() < big.len());
        assert!(preview.ends_with("… [truncated]"));

        // output_blob resolves via CCR to the byte-exact full original.
        let blob_hash: Option<String> =
            sqlx::query("SELECT output_blob FROM observations WHERE id = $1")
                .bind(id)
                .fetch_one(&db.pool)
                .await?
                .try_get("output_blob")
                .ok()
                .flatten();
        let blob_hash = blob_hash.expect("large output must record an output_blob");
        let restored = crate::ccr::load_blob(&db, &blob_hash).await?;
        assert_eq!(
            restored,
            big.as_bytes(),
            "CCR must return the exact original"
        );

        // A small output below the cap stores no blob (no overhead).
        let small_id =
            insert_observation(&db, &s, "/tmp/p", "Read", None, Some("tiny"), 2048).await?;
        let small_blob: Option<String> =
            sqlx::query("SELECT output_blob FROM observations WHERE id = $1")
                .bind(small_id)
                .fetch_one(&db.pool)
                .await?
                .try_get("output_blob")
                .ok()
                .flatten();
        assert!(
            small_blob.is_none(),
            "small output should not allocate a blob"
        );

        let _ = std::fs::remove_file(path);
        Ok(())
    }
}
