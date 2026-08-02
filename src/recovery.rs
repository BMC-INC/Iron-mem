//! Offline extraction receipts, health, and restart-safe transcript backfill.

use anyhow::{Context, Result};
use chrono::Utc;
use serde::Serialize;
use sqlx::Row;

use crate::config::{CompressionMode, Config};
use crate::db::{self, Backend, Database, Observation};
use crate::embedder::Embedder;
use crate::governance::{
    parse_classification, parse_consent_state, MemoryGovernance, MemorySourceType, TrustTier,
};
use crate::provider::CompressionResult;
use crate::vectorstore::VectorStore;

pub const EXTRACTOR_VERSION: &str = "local-extractive-v1";

pub async fn migrate(db: &Database) -> Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS extraction_receipts (
            source_memory_id BIGINT NOT NULL,
            transcript_hash TEXT NOT NULL,
            extractor_version TEXT NOT NULL,
            status TEXT NOT NULL,
            observation_count BIGINT NOT NULL DEFAULT 0,
            fact_count BIGINT NOT NULL DEFAULT 0,
            procedure_count BIGINT NOT NULL DEFAULT 0,
            supported_count BIGINT NOT NULL DEFAULT 0,
            zero_yield_reason TEXT,
            error TEXT,
            started_at BIGINT NOT NULL,
            completed_at BIGINT,
            PRIMARY KEY(source_memory_id, transcript_hash, extractor_version)
        )",
    )
    .execute(&db.pool)
    .await?;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_extraction_receipts_status
         ON extraction_receipts(status, completed_at)",
    )
    .execute(&db.pool)
    .await?;
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct ExtractionStatus {
    pub extractor_version: &'static str,
    pub completed: i64,
    pub failed: i64,
    pub total_facts: i64,
    pub total_procedures: i64,
    pub zero_yield_streak: i64,
    pub last_success_at: Option<i64>,
    pub archived_candidates_remaining: i64,
    pub uncompressed_sessions_remaining: i64,
}

pub async fn status(db: &Database) -> Result<ExtractionStatus> {
    let row = sqlx::query(
        "SELECT
            SUM(CASE WHEN status = 'complete' THEN 1 ELSE 0 END) completed,
            SUM(CASE WHEN status = 'failed' THEN 1 ELSE 0 END) failed,
            COALESCE(SUM(fact_count), 0) total_facts,
            COALESCE(SUM(procedure_count), 0) total_procedures,
            MAX(CASE WHEN status = 'complete' THEN completed_at END) last_success_at
         FROM extraction_receipts WHERE extractor_version = $1",
    )
    .bind(EXTRACTOR_VERSION)
    .fetch_one(&db.pool)
    .await?;
    let recent = sqlx::query(
        "SELECT fact_count, procedure_count FROM extraction_receipts
         WHERE extractor_version = $1 AND status = 'complete'
         ORDER BY completed_at DESC, source_memory_id DESC LIMIT 1000",
    )
    .bind(EXTRACTOR_VERSION)
    .fetch_all(&db.pool)
    .await?;
    let zero_yield_streak = recent
        .iter()
        .take_while(|r| r.get::<i64, _>("fact_count") + r.get::<i64, _>("procedure_count") == 0)
        .count() as i64;
    let archived_candidates_remaining = archived_candidates(db, 0, i64::MAX, 1, 0, None)
        .await?
        .len() as i64;
    let uncompressed_sessions_remaining = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM sessions s WHERE s.compressed = 0 AND EXISTS
         (SELECT 1 FROM observations o WHERE o.session_id = s.id)",
    )
    .fetch_one(&db.pool)
    .await?;
    Ok(ExtractionStatus {
        extractor_version: EXTRACTOR_VERSION,
        completed: row.try_get::<i64, _>("completed").unwrap_or(0),
        failed: row.try_get::<i64, _>("failed").unwrap_or(0),
        total_facts: row.get("total_facts"),
        total_procedures: row.get("total_procedures"),
        zero_yield_streak,
        last_success_at: row.try_get("last_success_at").ok().flatten(),
        archived_candidates_remaining,
        uncompressed_sessions_remaining,
    })
}

pub async fn record_compression(
    db: &Database,
    memory_id: i64,
    transcript_hash: &str,
    observation_count: usize,
    result: &CompressionResult,
) -> Result<()> {
    complete_receipt(
        db,
        memory_id,
        transcript_hash,
        observation_count,
        result.facts.len(),
        result.procedures.len(),
        result.facts.len() + result.procedures.len(),
        zero_reason(
            observation_count,
            result.facts.len(),
            result.procedures.len(),
        ),
    )
    .await
}

#[derive(Debug, Clone)]
pub struct BackfillOptions {
    pub dry_run: bool,
    pub after_memory_id: i64,
    pub since_timestamp: i64,
    pub limit: i64,
    pub min_observations: i64,
    pub project: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct BackfillReport {
    pub dry_run: bool,
    pub extractor_version: &'static str,
    pub scanned: usize,
    pub completed: usize,
    pub skipped_receipted: usize,
    pub failed: usize,
    pub facts: usize,
    pub procedures: usize,
    pub supported_items: usize,
    pub observations: usize,
    pub next_after_memory_id: i64,
    pub failures: Vec<String>,
}

#[derive(Debug)]
struct Candidate {
    memory_id: i64,
    project: String,
    session_id: String,
    transcript_hash: String,
    observation_count: i64,
}

pub async fn backfill(
    db: &Database,
    embedder: Option<&dyn Embedder>,
    store: &dyn VectorStore,
    cfg: &Config,
    options: &BackfillOptions,
) -> Result<BackfillReport> {
    let mut report = BackfillReport {
        dry_run: options.dry_run,
        extractor_version: EXTRACTOR_VERSION,
        next_after_memory_id: options.after_memory_id,
        ..Default::default()
    };
    let candidates = archived_candidates(
        db,
        options.after_memory_id,
        options.limit,
        options.min_observations,
        options.since_timestamp,
        options.project.as_deref(),
    )
    .await?;
    for candidate in candidates {
        report.scanned += 1;
        report.next_after_memory_id = candidate.memory_id;
        let loaded = crate::ccr::load_blob(db, &candidate.transcript_hash).await;
        let transcript = match loaded
            .and_then(|bytes| String::from_utf8(bytes).context("CCR transcript is not UTF-8"))
        {
            Ok(transcript) => transcript,
            Err(error) => {
                report.failed += 1;
                report
                    .failures
                    .push(format!("memory {}: {error}", candidate.memory_id));
                if !options.dry_run {
                    fail_receipt(db, &candidate, &error.to_string()).await?;
                }
                continue;
            }
        };
        let mut observations = db::get_observations_for_session(db, &candidate.session_id).await?;
        if observations.is_empty() {
            observations.push(Observation {
                id: 0,
                session_id: candidate.session_id.clone(),
                project: candidate.project.clone(),
                tool: "Archive".into(),
                input: Some(transcript.clone()),
                output: None,
                created_at: 0,
            });
        }
        let extraction = crate::local_extractor::extract(&observations);
        let supported = extraction
            .facts
            .iter()
            .chain(&extraction.procedures)
            .filter(|item| source_supports(item, &transcript))
            .count();
        report.observations += candidate.observation_count as usize;
        report.facts += extraction.facts.len();
        report.procedures += extraction.procedures.len();
        report.supported_items += supported;
        if options.dry_run {
            continue;
        }
        if receipt_complete(db, &candidate).await? {
            report.skipped_receipted += 1;
            continue;
        }
        start_receipt(db, &candidate).await?;
        let result = persist_children(
            db,
            embedder,
            store,
            &candidate,
            &extraction.facts,
            &extraction.procedures,
        )
        .await;
        match result {
            Ok(()) => {
                complete_receipt(
                    db,
                    candidate.memory_id,
                    &candidate.transcript_hash,
                    observations.len(),
                    extraction.facts.len(),
                    extraction.procedures.len(),
                    supported,
                    zero_reason(
                        observations.len(),
                        extraction.facts.len(),
                        extraction.procedures.len(),
                    ),
                )
                .await?;
                report.completed += 1;
            }
            Err(error) => {
                fail_receipt(db, &candidate, &error.to_string()).await?;
                report.failed += 1;
                report
                    .failures
                    .push(format!("memory {}: {error}", candidate.memory_id));
            }
        }
    }

    let remaining = options.limit.saturating_sub(report.scanned as i64);
    if remaining > 0 {
        let sessions = uncompressed_candidates(
            db,
            remaining,
            options.min_observations,
            options.since_timestamp,
            options.project.as_deref(),
        )
        .await?;
        for session in sessions {
            report.scanned += 1;
            let observations = db::get_observations_for_session(db, &session.id).await?;
            let extraction = crate::local_extractor::extract(&observations);
            report.observations += observations.len();
            report.facts += extraction.facts.len();
            report.procedures += extraction.procedures.len();
            if !options.dry_run {
                let mut local_cfg = cfg.clone();
                local_cfg.compression.mode = CompressionMode::Local;
                crate::compress::run(db, embedder, store, &local_cfg, &session.id).await?;
                report.completed += 1;
            }
        }
    }
    Ok(report)
}

async fn archived_candidates(
    db: &Database,
    after: i64,
    limit: i64,
    min_obs: i64,
    since_timestamp: i64,
    project: Option<&str>,
) -> Result<Vec<Candidate>> {
    let id = if db.backend == Backend::Sqlite {
        "m.rowid"
    } else {
        "m.id"
    };
    let sql = format!(
        "SELECT {id} memory_id, m.project, m.session_id, mm.session_blob transcript_hash,
                (SELECT COUNT(*) FROM observations o WHERE o.session_id = m.session_id) observation_count
         FROM memories m JOIN memory_meta mm ON mm.memory_id = {id}
         WHERE {id} > $1 AND mm.kind = 'session' AND mm.session_blob IS NOT NULL
           AND ($2 IS NULL OR m.project = $2)
           AND LOWER(m.project) NOT LIKE '%locomo%'
           AND LOWER(m.project) NOT LIKE '%longmemeval%'
           AND LOWER(m.project) NOT LIKE '%benchmark%'
           AND m.created_at >= $3
           AND (SELECT COUNT(*) FROM observations o WHERE o.session_id = m.session_id) >= $4
           AND NOT EXISTS (SELECT 1 FROM extraction_receipts er
               WHERE er.source_memory_id = {id} AND er.transcript_hash = mm.session_blob
                 AND er.extractor_version = $5 AND er.status = 'complete')
         ORDER BY {id} ASC LIMIT $6"
    );
    let rows = sqlx::query(&sql)
        .bind(after)
        .bind(project)
        .bind(since_timestamp)
        .bind(min_obs)
        .bind(EXTRACTOR_VERSION)
        .bind(limit)
        .fetch_all(&db.pool)
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| Candidate {
            memory_id: r.get("memory_id"),
            project: r.get("project"),
            session_id: r.get("session_id"),
            transcript_hash: r.get("transcript_hash"),
            observation_count: r.get("observation_count"),
        })
        .collect())
}

async fn uncompressed_candidates(
    db: &Database,
    limit: i64,
    min_obs: i64,
    since_timestamp: i64,
    project: Option<&str>,
) -> Result<Vec<db::Session>> {
    let rows = sqlx::query(
        "SELECT s.id, s.project, s.started_at, s.ended_at, s.compressed FROM sessions s
         WHERE s.compressed = 0 AND ($1 IS NULL OR s.project = $1)
           AND LOWER(s.project) NOT LIKE '%locomo%'
           AND LOWER(s.project) NOT LIKE '%longmemeval%'
           AND LOWER(s.project) NOT LIKE '%benchmark%'
           AND s.started_at >= $2
           AND (SELECT COUNT(*) FROM observations o WHERE o.session_id = s.id) >= $3
         ORDER BY s.started_at ASC LIMIT $4",
    )
    .bind(project)
    .bind(since_timestamp)
    .bind(min_obs)
    .bind(limit)
    .fetch_all(&db.pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| db::Session {
            id: r.get("id"),
            project: r.get("project"),
            started_at: r.get("started_at"),
            ended_at: r.try_get("ended_at").ok().flatten(),
            compressed: r.get::<i64, _>("compressed") != 0,
        })
        .collect())
}

async fn persist_children(
    db: &Database,
    embedder: Option<&dyn Embedder>,
    store: &dyn VectorStore,
    candidate: &Candidate,
    facts: &[String],
    procedures: &[String],
) -> Result<()> {
    let parent = db::get_memory_meta_full(db, candidate.memory_id).await?;
    for (kind, items) in [("fact", facts), ("procedural", procedures)] {
        for (index, text) in items.iter().enumerate() {
            let source_ref = format!(
                "extract:{EXTRACTOR_VERSION}:{}:{kind}:{}",
                candidate.transcript_hash,
                index + 1
            );
            let existing = recoverable_child_id(
                db,
                &parent.namespace,
                candidate.memory_id,
                &candidate.session_id,
                kind,
                text,
                &source_ref,
            )
            .await?;
            let id = match existing {
                Some((_, true)) => continue,
                Some((id, false)) => id,
                None => {
                    db::insert_memory(
                        db,
                        &candidate.project,
                        &candidate.session_id,
                        text,
                        Some(&format!(
                            "{kind} recovered session:{}",
                            candidate.session_id
                        )),
                    )
                    .await?
                }
            };
            db::upsert_memory_meta(
                db,
                id,
                if kind == "procedural" {
                    0.75
                } else {
                    parent.importance
                },
            )
            .await?;
            db::set_memory_scope_kind(db, id, "project", kind).await?;
            let governance = MemoryGovernance {
                namespace: parent.namespace.clone(),
                source_type: MemorySourceType::Derived,
                trust_tier: TrustTier::Medium,
                writer_identity: Some("ironmem:offline-recovery".into()),
                source_ref: Some(source_ref),
                parent_memory_id: Some(candidate.memory_id),
                classification: parse_classification(&parent.classification),
                consent_state: parent
                    .consent_state
                    .as_deref()
                    .and_then(parse_consent_state),
                residency: parent.residency.clone(),
                retention_policy_id: parent.retention_policy_id.clone(),
                expires_at: parent.expires_at,
                legal_hold: parent.legal_hold,
            };
            db::apply_memory_governance(
                db,
                id,
                "project",
                kind,
                &governance,
                Some("ironmem:offline-recovery"),
                "derive",
            )
            .await?;
            if let Some(event_time) = parent.event_time.as_deref() {
                db::set_memory_event_time(db, id, event_time).await?;
            }
            if let Some(emb) = embedder {
                if let Ok(mut vectors) = emb.embed(std::slice::from_ref(text)).await {
                    if let Some(vector) = vectors.drain(..).next() {
                        store.upsert(db, id, emb.id(), emb.dim(), &vector).await?;
                    }
                }
            }
        }
    }
    Ok(())
}

/// Find an already governed child by receipt source-ref, or adopt an identical
/// same-session child left behind before its governance write completed. This
/// closes the insert→metadata crash window without deleting any record.
async fn recoverable_child_id(
    db: &Database,
    namespace: &str,
    parent_id: i64,
    session_id: &str,
    kind: &str,
    text: &str,
    source_ref: &str,
) -> Result<Option<(i64, bool)>> {
    let id = if db.backend == Backend::Sqlite {
        "m.rowid"
    } else {
        "m.id"
    };
    let sql = format!(
        "SELECT {id} memory_id,
                CASE WHEN mm.namespace = $1 AND mm.source_ref = $2 THEN 1 ELSE 0 END exact_ref
         FROM memories m LEFT JOIN memory_meta mm ON mm.memory_id = {id}
         WHERE (mm.namespace = $1 AND mm.source_ref = $2)
            OR (m.session_id = $3 AND m.summary = $4 AND
                (mm.memory_id IS NULL OR (mm.kind = $5 AND mm.parent_memory_id = $6)))
         ORDER BY exact_ref DESC, {id} ASC LIMIT 1"
    );
    Ok(sqlx::query(&sql)
        .bind(namespace)
        .bind(source_ref)
        .bind(session_id)
        .bind(text)
        .bind(kind)
        .bind(parent_id)
        .fetch_optional(&db.pool)
        .await?
        .map(|row| (row.get("memory_id"), row.get::<i64, _>("exact_ref") != 0)))
}

async fn receipt_complete(db: &Database, c: &Candidate) -> Result<bool> {
    Ok(sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM extraction_receipts WHERE source_memory_id=$1 AND transcript_hash=$2 AND extractor_version=$3 AND status='complete'")
        .bind(c.memory_id).bind(&c.transcript_hash).bind(EXTRACTOR_VERSION).fetch_one(&db.pool).await? > 0)
}

async fn start_receipt(db: &Database, c: &Candidate) -> Result<()> {
    sqlx::query("INSERT INTO extraction_receipts(source_memory_id,transcript_hash,extractor_version,status,observation_count,started_at) VALUES($1,$2,$3,'running',$4,$5) ON CONFLICT(source_memory_id,transcript_hash,extractor_version) DO UPDATE SET status='running', error=NULL, started_at=excluded.started_at")
        .bind(c.memory_id).bind(&c.transcript_hash).bind(EXTRACTOR_VERSION).bind(c.observation_count).bind(Utc::now().timestamp()).execute(&db.pool).await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn complete_receipt(
    db: &Database,
    memory_id: i64,
    hash: &str,
    observations: usize,
    facts: usize,
    procedures: usize,
    supported: usize,
    reason: Option<&'static str>,
) -> Result<()> {
    let now = Utc::now().timestamp();
    sqlx::query("INSERT INTO extraction_receipts(source_memory_id,transcript_hash,extractor_version,status,observation_count,fact_count,procedure_count,supported_count,zero_yield_reason,started_at,completed_at) VALUES($1,$2,$3,'complete',$4,$5,$6,$7,$8,$9,$9) ON CONFLICT(source_memory_id,transcript_hash,extractor_version) DO UPDATE SET status='complete',observation_count=excluded.observation_count,fact_count=excluded.fact_count,procedure_count=excluded.procedure_count,supported_count=excluded.supported_count,zero_yield_reason=excluded.zero_yield_reason,error=NULL,completed_at=excluded.completed_at")
        .bind(memory_id).bind(hash).bind(EXTRACTOR_VERSION).bind(observations as i64).bind(facts as i64).bind(procedures as i64).bind(supported as i64).bind(reason).bind(now).execute(&db.pool).await?;
    if facts + procedures == 0 && observations > 0 {
        tracing::warn!(
            "zero-yield extraction: memory={memory_id} observations={observations} reason={}",
            reason.unwrap_or("unknown")
        );
    }
    Ok(())
}

async fn fail_receipt(db: &Database, c: &Candidate, error: &str) -> Result<()> {
    start_receipt(db, c).await?;
    sqlx::query("UPDATE extraction_receipts SET status='failed', error=$1, completed_at=$2 WHERE source_memory_id=$3 AND transcript_hash=$4 AND extractor_version=$5")
        .bind(error).bind(Utc::now().timestamp()).bind(c.memory_id).bind(&c.transcript_hash).bind(EXTRACTOR_VERSION).execute(&db.pool).await?;
    Ok(())
}

fn zero_reason(observations: usize, facts: usize, procedures: usize) -> Option<&'static str> {
    if facts + procedures > 0 {
        None
    } else if observations == 0 {
        Some("content_free")
    } else {
        Some("no_durable_signals")
    }
}

fn source_supports(item: &str, transcript: &str) -> bool {
    let lower = transcript.to_ascii_lowercase();
    if lower.contains(&item.to_ascii_lowercase()) {
        return true;
    }
    let terms: Vec<String> = item
        .split(|c: char| !c.is_alphanumeric())
        .filter(|v| v.len() >= 4)
        .map(str::to_ascii_lowercase)
        .collect();
    terms.len() >= 2
        && terms
            .iter()
            .filter(|term| lower.contains(term.as_str()))
            .count()
            * 2
            >= terms.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::CompressionResult;
    use crate::vectorstore::BruteForceStore;

    #[tokio::test]
    async fn backfill_is_dry_run_first_receipted_and_idempotent() -> Result<()> {
        let path = std::env::temp_dir().join(format!(
            "ironmem-extraction-recovery-{}.db",
            uuid::Uuid::new_v4()
        ));
        let db = Database::new(&path.to_string_lossy()).await?;
        db.migrate().await?;
        let session = db::create_session(&db, "/tmp/recovery").await?;
        db::insert_observation(
            &db,
            &session,
            "/tmp/recovery",
            "TaskCreate",
            Some("IronMem must stay offline first"),
            None,
            2048,
        )
        .await?;
        db::insert_observation(
            &db,
            &session,
            "/tmp/recovery",
            "Bash",
            Some("cargo test"),
            Some("test result: ok. 10 passed"),
            2048,
        )
        .await?;
        let observations = db::get_observations_for_session(&db, &session).await?;
        let parent = crate::compress::persist(
            &db,
            None,
            &BruteForceStore,
            "/tmp/recovery",
            &session,
            CompressionResult {
                summary: "legacy local archive".into(),
                tags: "local-compression session-archive".into(),
                ..Default::default()
            },
        )
        .await?;
        crate::compress::store_session_transcript(&db, parent, &observations).await?;

        let options = BackfillOptions {
            dry_run: true,
            after_memory_id: 0,
            since_timestamp: 0,
            limit: 5,
            min_observations: 1,
            project: None,
        };
        let dry = backfill(&db, None, &BruteForceStore, &Config::default(), &options).await?;
        assert_eq!(dry.scanned, 1);
        assert_eq!(dry.facts, 1);
        assert_eq!(dry.procedures, 2);
        assert_eq!(status(&db).await?.completed, 0);

        let applied = backfill(
            &db,
            None,
            &BruteForceStore,
            &Config::default(),
            &BackfillOptions {
                dry_run: false,
                ..options.clone()
            },
        )
        .await?;
        assert_eq!(applied.completed, 1);
        assert_eq!(status(&db).await?.completed, 1);
        let child_count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM memory_meta WHERE parent_memory_id = $1",
        )
        .bind(parent)
        .fetch_one(&db.pool)
        .await?;
        assert_eq!(child_count, 3);
        for row in sqlx::query("SELECT memory_id FROM memory_meta WHERE parent_memory_id = $1")
            .bind(parent)
            .fetch_all(&db.pool)
            .await?
        {
            let child = db::get_memory_meta_full(&db, row.get("memory_id")).await?;
            assert_eq!(child.parent_memory_id, Some(parent));
            assert_eq!(child.derivation_depth, 1);
            assert!(child.evidence_root_id.is_some());
        }

        let repeated = backfill(
            &db,
            None,
            &BruteForceStore,
            &Config::default(),
            &BackfillOptions {
                dry_run: false,
                ..options
            },
        )
        .await?;
        assert_eq!(repeated.scanned, 0);
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM memory_meta WHERE parent_memory_id = $1"
            )
            .bind(parent)
            .fetch_one(&db.pool)
            .await?,
            3
        );
        let _ = std::fs::remove_file(path);
        Ok(())
    }
}
