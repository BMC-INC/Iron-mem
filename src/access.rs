//! Delivery telemetry. No candidate read or authorization decision records access.
//! Counts cover this installation's observation window, not unknown prior history.
use crate::db::{Database, Memory};
use anyhow::{ensure, Result};
use serde::Serialize;
use sqlx::Row;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy)]
pub enum Delivery {
    Recall,
    Expansion,
}
impl Delivery {
    fn name(self) -> &'static str {
        match self {
            Self::Recall => "recall",
            Self::Expansion => "expansion",
        }
    }
}

pub async fn migrate(db: &Database) -> Result<()> {
    sqlx::query("CREATE TABLE IF NOT EXISTS memory_access (memory_id BIGINT PRIMARY KEY REFERENCES memory_meta(memory_id) ON DELETE CASCADE, observed_since BIGINT NOT NULL, recall_count BIGINT NOT NULL DEFAULT 0, last_recalled_at BIGINT, expansion_count BIGINT NOT NULL DEFAULT 0, last_expanded_at BIGINT)").execute(&db.pool).await?;
    sqlx::query("CREATE TABLE IF NOT EXISTS access_receipts (operation_hash TEXT NOT NULL, kind TEXT NOT NULL, memory_id BIGINT NOT NULL REFERENCES memory_meta(memory_id) ON DELETE CASCADE, created_at BIGINT NOT NULL, PRIMARY KEY(operation_hash,kind,memory_id))").execute(&db.pool).await?;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_access_receipts_time ON access_receipts(created_at)",
    )
    .execute(&db.pool)
    .await?;
    // Establish an honest zero-count observation window for existing memories.
    sqlx::query("INSERT INTO memory_access(memory_id,observed_since) SELECT memory_id,$1 FROM memory_meta WHERE 1=1 ON CONFLICT(memory_id) DO NOTHING").bind(chrono::Utc::now().timestamp()).execute(&db.pool).await?;
    crate::access_mutations::migrate(db).await?;
    Ok(())
}

/// Deduplicates an operation for seven days. Call only for delivered, authorized IDs.
/// No operation ID means each completed handler invocation is a separate access.
pub async fn record(
    db: &Database,
    kind: Delivery,
    ids: &[i64],
    operation: Option<&str>,
    now: i64,
) -> Result<()> {
    use sha2::{Digest, Sha256};
    if ids.is_empty() {
        return Ok(());
    }
    ensure!(ids.len() <= 10_000, "too many delivery IDs");
    let op = operation.map(|s| format!("{:x}", Sha256::digest(s.as_bytes())));
    let mut tx = crate::db::begin_write(db).await?;
    sqlx::query("DELETE FROM access_receipts WHERE created_at < $1")
        .bind(now.saturating_sub(7 * 86400))
        .execute(&mut *tx)
        .await?;
    for id in ids.iter().copied().collect::<BTreeSet<_>>() {
        // Missing metadata (including concurrently deleted memories) is not fabricated.
        sqlx::query("INSERT INTO memory_access(memory_id,observed_since) SELECT memory_id,$2 FROM memory_meta WHERE memory_id=$1 ON CONFLICT(memory_id) DO NOTHING").bind(id).bind(now).execute(&mut *tx).await?;
        if let Some(op) = &op {
            let inserted = sqlx::query("INSERT INTO access_receipts(operation_hash,kind,memory_id,created_at) SELECT $1,$2,memory_id,$4 FROM memory_access WHERE memory_id=$3 ON CONFLICT(operation_hash,kind,memory_id) DO NOTHING").bind(op).bind(kind.name()).bind(id).bind(now).execute(&mut *tx).await?.rows_affected();
            if inserted == 0 {
                continue;
            }
        }
        let (count, last) = match kind {
            Delivery::Recall => ("recall_count", "last_recalled_at"),
            Delivery::Expansion => ("expansion_count", "last_expanded_at"),
        };
        sqlx::query(&format!("UPDATE memory_access SET {count}={count}+1,{last}=CASE WHEN {last} IS NULL OR {last}<$2 THEN $2 ELSE {last} END WHERE memory_id=$1")).bind(id).bind(now).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn delivered(db: &Database, kind: Delivery, ids: &[i64], operation: Option<&str>) {
    let started = std::time::Instant::now();
    if let Err(error) = record(db, kind, ids, operation, chrono::Utc::now().timestamp()).await {
        FAILURES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        tracing::warn!(%error, "Delivered content but access telemetry failed");
    }
    CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    NANOS.fetch_add(
        started.elapsed().as_nanos().min(u64::MAX as u128) as u64,
        std::sync::atomic::Ordering::Relaxed,
    );
}
static CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static NANOS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static FAILURES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub fn status() -> serde_json::Value {
    use std::sync::atomic::Ordering::Relaxed;
    let calls = CALLS.load(Relaxed);
    serde_json::json!({"handler_delivery_calls":calls,"failures":FAILURES.load(Relaxed),"mean_write_us":if calls==0 {None} else {Some(NANOS.load(Relaxed)/calls/1000)},"history_complete":false,"retry_window_days":7,"semantics":"authorized response prepared; transport acknowledgement unavailable"})
}
pub fn ids(memories: &[Memory], advisory: &[Memory]) -> Vec<i64> {
    memories.iter().chain(advisory).map(|m| m.id).collect()
}
pub fn gate_ids(gate: &crate::egress::GateResult) -> Vec<i64> {
    ids(&gate.authorized, &gate.advisory)
}

#[derive(Debug, Default, Serialize)]
pub struct Stats {
    pub memory_id: i64,
    pub observed_since: Option<i64>,
    pub recall_count: i64,
    pub last_recalled_at: Option<i64>,
    pub expansion_count: i64,
    pub last_expanded_at: Option<i64>,
    pub injection_count: i64,
    pub last_injected_at: Option<i64>,
    /// Committed row writes to mutable memory metadata/relations, since tracking began.
    pub mutation_count: Option<i64>,
    pub mutation_observed_since: Option<i64>,
    pub ledger_event_count: i64,
    pub logical_source_bytes: Option<i64>,
}
pub async fn stats(db: &Database, ids: &[i64]) -> Result<BTreeMap<i64, Stats>> {
    let mut result = BTreeMap::new();
    for batch in ids.chunks(400) {
        if batch.is_empty() {
            continue;
        }
        // Only numeric internal IDs are interpolated; no user strings enter SQL.
        let list = batch
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let query=format!("SELECT m.memory_id,a.observed_since,a.recall_count,a.last_recalled_at,a.expansion_count,a.last_expanded_at,b.orig_len FROM memory_meta m LEFT JOIN memory_access a ON a.memory_id=m.memory_id LEFT JOIN blobs b ON b.hash=m.session_blob WHERE m.memory_id IN ({list})");
        for row in sqlx::query(&query).fetch_all(&db.pool).await? {
            let id = row.get("memory_id");
            result.insert(
                id,
                Stats {
                    memory_id: id,
                    observed_since: row.try_get("observed_since")?,
                    recall_count: row.try_get::<Option<i64>, _>("recall_count")?.unwrap_or(0),
                    last_recalled_at: row.try_get("last_recalled_at")?,
                    expansion_count: row
                        .try_get::<Option<i64>, _>("expansion_count")?
                        .unwrap_or(0),
                    last_expanded_at: row.try_get("last_expanded_at")?,
                    logical_source_bytes: row.try_get("orig_len")?,
                    ..Default::default()
                },
            );
        }
        for row in sqlx::query(&format!("SELECT memory_id,COUNT(*) AS n,MAX(created_at) AS last FROM injection_events WHERE memory_id IN ({list}) GROUP BY memory_id")).fetch_all(&db.pool).await? {
            if let Some(s)=result.get_mut(&row.get::<i64,_>("memory_id")) { s.injection_count=row.get("n");s.last_injected_at=row.try_get("last")?; }
        }
        for row in sqlx::query(&format!("SELECT memory_id,observed_since,mutation_count FROM memory_mutations WHERE memory_id IN ({list})")).fetch_all(&db.pool).await? {
            if let Some(s)=result.get_mut(&row.get::<i64,_>("memory_id")) {s.mutation_count=Some(row.get("mutation_count"));s.mutation_observed_since=Some(row.get("observed_since"));}
        }
        for row in sqlx::query(&format!("SELECT memory_id,COUNT(*) AS n FROM memory_ledger WHERE memory_id IN ({list}) GROUP BY memory_id")).fetch_all(&db.pool).await? {
            if let Some(s)=result.get_mut(&row.get::<i64,_>("memory_id")) {s.ledger_event_count=row.get("n");}
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn access_overhead_microbenchmark() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let db = Database::new(dir.path().join("timing.db").to_str().unwrap()).await?;
        db.migrate().await?;
        let session = crate::db::create_session(&db, "synthetic-timing").await?;
        let id =
            crate::db::insert_memory(&db, "synthetic-timing", &session, "timing fixture", None)
                .await?;
        let mut samples = Vec::new();
        for i in 0..120 {
            let start = std::time::Instant::now();
            record(&db, Delivery::Recall, &[id], None, 100 + i).await?;
            if i >= 20 {
                samples.push(start.elapsed().as_micros() as u64);
            }
        }
        samples.sort_unstable();
        assert_eq!(stats(&db, &[id]).await?[&id].recall_count, 120);
        println!(
            "ACCESS_TIMING_JSON {}",
            serde_json::json!({"schema_version":1,"source_sha256":env!("IRONMEM_SOURCE_SHA"),"backend":"sqlite","profile":"debug without debug symbols","workload":"one memory, 120 sequential aggregate writes, discard first 20","samples":samples.len(),"p50_us":samples[49],"p95_us":samples[94],"cache":"warm process, uncontrolled OS cache","scored_accuracy":null})
        );
        db.pool.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn access_mutations_invalidation_rollback_and_snapshot_window() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let project = dir.path().to_str().unwrap();
        let db = Database::new(dir.path().join("access.db").to_str().unwrap()).await?;
        db.migrate().await?;
        exercise_mutations(&db, project).await?;
        db.pool.close().await;
        Ok(())
    }

    async fn exercise_mutations(db: &Database, project: &str) -> Result<()> {
        let session = crate::db::create_session(db, project).await?;
        let id = crate::db::insert_memory(db, project, &session, "synthetic fact", None).await?;
        let memory = crate::db::get_memory_by_id(db, id).await?.unwrap();
        let before = stats(db, &[id]).await?[&id].mutation_count.unwrap();
        let mut tx = crate::db::begin_write(db).await?;
        sqlx::query("UPDATE memory_meta SET importance=0.7 WHERE memory_id=$1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.rollback().await?;
        assert_eq!(stats(db, &[id]).await?[&id].mutation_count, Some(before));
        let revision = crate::hooks::context_revision(db).await?;
        crate::hooks::inject_with_budget(
            db,
            project,
            std::slice::from_ref(&memory),
            24000,
            Some(revision),
        )
        .await?;
        crate::db::set_memory_scope_kind(db, id, "project", "architecture").await?;
        let path = std::path::Path::new(project).join("IRONMEM.md");
        assert!(!path.exists());
        assert!(crate::hooks::inject_with_budget(
            db,
            project,
            std::slice::from_ref(&memory),
            24000,
            Some(revision)
        )
        .await
        .is_err());
        assert!(stats(db, &[id]).await?[&id].mutation_count.unwrap() > before);
        crate::hooks::inject_memories(db, project, std::slice::from_ref(&memory)).await?;
        std::fs::write(&path, "user-edited context")?;
        crate::db::set_memory_scope_kind(db, id, "project", "procedural").await?;
        assert_eq!(std::fs::read_to_string(&path)?, "user-edited context");
        std::fs::remove_file(&path)?;
        let full = crate::checkpoint::create(db, None, project, false).await?;
        record(db, Delivery::Recall, &[id], Some("snapshot-check"), 100).await?;
        let unchanged = crate::checkpoint::create(db, None, project, true).await?;
        assert_eq!(
            full.blob_hash, unchanged.blob_hash,
            "operational access does not change semantic snapshots"
        );
        let state = crate::checkpoint::load(db, &full.blob_hash).await?;
        crate::checkpoint::restore(db, &state, &full.id, false).await?;
        let after = stats(db, &[id]).await?.remove(&id).unwrap();
        assert_eq!(
            after.recall_count, 0,
            "restoration establishes a fresh access window"
        );
        assert!(after.observed_since.is_some());
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires isolated IRONMEM_TEST_POSTGRES_URL"]
    async fn access_postgres_delivery_and_mutations() -> Result<()> {
        let db = Database::new(&std::env::var("IRONMEM_TEST_POSTGRES_URL")?).await?;
        db.migrate().await?;
        let dir = tempfile::tempdir()?;
        exercise_mutations(&db, dir.path().to_str().unwrap()).await?;
        let session = crate::db::create_session(&db, "pg-access").await?;
        let id = crate::db::insert_memory(&db, "pg-access", &session, "pg synthetic", None).await?;
        let ids = [id];
        let (a, b) = tokio::join!(
            record(&db, Delivery::Recall, &ids, Some("pg-same"), 100),
            record(&db, Delivery::Recall, &ids, Some("pg-same"), 100)
        );
        a?;
        b?;
        assert_eq!(stats(&db, &ids).await?[&id].recall_count, 1);
        db.pool.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn access_counts_retries_concurrency_and_existing_injections() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let db = Database::new(dir.path().join("access.db").to_str().unwrap()).await?;
        db.migrate().await?;
        let session = crate::db::create_session(&db, "project").await?;
        let id = crate::db::insert_memory(&db, "project", &session, "fact", None).await?;
        let m = crate::db::get_memory_by_id(&db, id).await?.unwrap();
        crate::db::record_injection_events(&db, "project", None, None, &[m]).await?;
        record(&db, Delivery::Recall, &[id, id], Some("one"), 100).await?;
        record(&db, Delivery::Recall, &[id], Some("one"), 101).await?;
        let ids = [id];
        let (a, b) = tokio::join!(
            record(&db, Delivery::Recall, &ids, None, 102),
            record(&db, Delivery::Recall, &ids, None, 103)
        );
        a?;
        b?;
        record(&db, Delivery::Expansion, &[id], Some("one"), 104).await?;
        let s = stats(&db, &[id]).await?.remove(&id).unwrap();
        assert_eq!(s.recall_count, 3);
        assert_eq!(s.expansion_count, 1);
        assert_eq!(s.injection_count, 1);
        assert_eq!(s.last_recalled_at, Some(103));
        assert!(s.mutation_count.unwrap() > 0);
        record(&db, Delivery::Recall, &[id], Some("one"), 8 * 86400).await?;
        assert_eq!(stats(&db, &[id]).await?[&id].recall_count, 4);
        db.migrate().await?;
        assert_eq!(stats(&db, &[id]).await?[&id].recall_count, 4);
        db.pool.close().await;
        Ok(())
    }
}

/// Resolve the reference actually expanded (selector precedence matters), then
/// intersect with the authorized owners. Never credit unrelated supplied IDs.
pub async fn delivered_expansion(
    db: &Database,
    expanded: &crate::expansion::ExpandedOriginal,
    gate: &crate::egress::GateResult,
) {
    if expanded.hash.is_none() {
        return;
    }
    let mut allowed = gate_ids(gate).into_iter().collect::<BTreeSet<_>>();
    allowed.extend(gate.source_required.iter().map(|m| m.id));
    let resolved = if let Some(chunk) = expanded.chunk_id.as_deref() {
        crate::db::memory_ids_for_original_reference(db, None, None, None, Some(chunk)).await
    } else {
        crate::db::memory_ids_for_original_reference(db, None, None, expanded.hash.as_deref(), None)
            .await
    };
    match resolved {
        Ok(ids) => {
            delivered(
                db,
                Delivery::Expansion,
                &ids.into_iter()
                    .filter(|id| allowed.contains(id))
                    .collect::<Vec<_>>(),
                None,
            )
            .await
        }
        Err(error) => {
            tracing::warn!(%error,"Delivered source but could not resolve telemetry ownership")
        }
    }
}
