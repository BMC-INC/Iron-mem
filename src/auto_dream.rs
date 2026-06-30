//! (#3) Heuristic auto-dream trigger.
//!
//! When `auto_dream.enabled`, a thin background watcher periodically scans
//! projects and, for any idle longer than `gap_minutes`, fires a consolidation
//! and synthesis pass, the same work `dream_memory` does with `apply=true`.
//! Every auto-triggered pass is recorded in the governance ledger with a
//! `trigger_reason`, so it is auditable; that auditability is the differentiator
//! over a black-box "dreaming" system. One signal only (idle gap); volume
//! triggers and depth scaling are deliberately deferred.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::config::Config;
use crate::db::{self, Database};
use crate::embedder::Embedder;
use crate::vectorstore::VectorStore;

/// Re-evaluate idle projects on this cadence. Checking far more often than the
/// gap buys nothing, so this is a coarse poll.
const POLL_SECS: u64 = 60;

/// Cap on projects scanned per poll.
const PROJECT_SCAN_LIMIT: i64 = 500;

/// Background loop: every `POLL_SECS`, consolidate any project idle past the gap.
/// Runs until the process exits. Spawned only when `auto_dream.enabled`.
pub async fn watch(
    db: Database,
    config: Config,
    embedder: Option<Arc<dyn Embedder>>,
    store: Arc<dyn VectorStore>,
) {
    let gap_secs = (config.auto_dream.gap_minutes as i64).max(1) * 60;
    // Last activity timestamp we already consolidated on, per project, so a
    // project that stays idle isn't re-dreamed every poll.
    let mut dreamed_at: HashMap<String, i64> = HashMap::new();
    tracing::info!(
        "auto-dream watcher started (idle gap {} min, poll {POLL_SECS}s)",
        config.auto_dream.gap_minutes
    );
    loop {
        tokio::time::sleep(Duration::from_secs(POLL_SECS)).await;
        let now = chrono::Utc::now().timestamp();
        let projects = match db::list_projects(&db, PROJECT_SCAN_LIMIT).await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("auto_dream: list_projects failed: {e}");
                continue;
            }
        };
        for p in projects {
            let idle = now - p.last_activity;
            if idle < gap_secs {
                continue;
            }
            // Dream once per idle period: skip if we already consolidated at or
            // after this project's current last-activity mark.
            if dreamed_at.get(&p.project).copied().unwrap_or(0) >= p.last_activity {
                continue;
            }
            run_pass(
                &db,
                &config,
                embedder.as_deref(),
                store.as_ref(),
                &p.project,
                idle,
            )
            .await;
            dreamed_at.insert(p.project.clone(), p.last_activity);
        }
    }
}

/// One consolidation + synthesis pass for an idle project, then a ledger entry
/// recording the trigger reason (the audit record).
async fn run_pass(
    db: &Database,
    config: &Config,
    embedder: Option<&dyn Embedder>,
    store: &dyn VectorStore,
    project: &str,
    idle_secs: i64,
) {
    tracing::info!("auto_dream: consolidating idle project {project} (idle {idle_secs}s)");

    // Consolidate textual duplicates (apply=true).
    let consolidated = match crate::reflection::run(
        db,
        embedder,
        store,
        Some(project),
        false, // dry_run
        true,  // apply
        200,
    )
    .await
    {
        Ok(r) => r.applied,
        Err(e) => {
            tracing::warn!("auto_dream: reflection::run failed for {project}: {e}");
            0
        }
    };

    // Synthesize derived (inferred) facts — governed + quarantined by synthesize.
    let derived = match crate::reflection::synthesize(
        db,
        embedder,
        store,
        config,
        Some(project),
        true, // apply
        400,
        12,
    )
    .await
    {
        Ok(r) => r.derived,
        Err(e) => {
            tracing::warn!("auto_dream: synthesize failed for {project}: {e}");
            0
        }
    };

    // Audit record: the differentiator over black-box dreaming. Namespace-level
    // ledger entry (no single memory_id), tagged with the trigger reason.
    let payload = serde_json::json!({
        "project": project,
        "trigger_reason": "gap",
        "idle_seconds": idle_secs,
        "consolidated": consolidated,
        "derived": derived,
    })
    .to_string();
    if let Err(e) = db::append_memory_ledger(
        db,
        crate::governance::DEFAULT_NAMESPACE,
        None,
        "auto_dream",
        Some("ironmem::auto_consolidation"),
        &payload,
    )
    .await
    {
        tracing::warn!("auto_dream: ledger append failed for {project}: {e}");
    }
}
