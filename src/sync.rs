//! Multi-agent sync primitives.
//!
//! IronMem stays local-first, but teams can point multiple agents at Postgres.
//! This module records idempotent CRDT-style operations in an append-only log.
//! Events are immutable, ordered by Lamport clock, and safe to import more than
//! once because `event_id` is the conflict-free identity.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::db::{self, Database, SyncEvent};

#[derive(Debug, Serialize, Deserialize)]
pub struct SyncPayload {
    pub kind: String,
    pub memory_id: Option<i64>,
    pub edge_id: Option<i64>,
    pub body: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PublishResult {
    pub event_id: String,
    pub inserted: bool,
    pub lamport: i64,
}

pub async fn next_lamport(db: &Database, project: Option<&str>) -> Result<i64> {
    let events = db::list_sync_events(db, project, 0, i64::MAX).await?;
    Ok(events.iter().map(|e| e.lamport).max().unwrap_or(0) + 1)
}

pub async fn publish(
    db: &Database,
    node_id: &str,
    project: Option<&str>,
    op_type: &str,
    payload: &SyncPayload,
) -> Result<PublishResult> {
    let lamport = next_lamport(db, project).await?;
    let event_id = format!("sync-{}", Uuid::new_v4());
    let raw = serde_json::to_string(payload)?;
    let inserted =
        db::insert_sync_event(db, &event_id, node_id, project, lamport, op_type, &raw).await?;
    Ok(PublishResult {
        event_id,
        inserted,
        lamport,
    })
}

#[allow(dead_code)]
pub async fn import_events(db: &Database, events: &[SyncEvent]) -> Result<usize> {
    let mut inserted = 0;
    for event in events {
        if db::insert_sync_event(
            db,
            &event.event_id,
            &event.node_id,
            event.project.as_deref(),
            event.lamport,
            &event.op_type,
            &event.payload,
        )
        .await?
        {
            inserted += 1;
        }
    }
    Ok(inserted)
}

pub async fn export_events(
    db: &Database,
    project: Option<&str>,
    after_lamport: i64,
    limit: i64,
) -> Result<Vec<SyncEvent>> {
    db::list_sync_events(db, project, after_lamport, limit).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn importing_same_event_is_idempotent() -> anyhow::Result<()> {
        let db_path = std::env::temp_dir().join(format!("ironmem-sync-test-{}.db", Uuid::new_v4()));
        let db_path_string = db_path.to_string_lossy().to_string();
        let db = Database::new(&db_path_string).await?;
        db.migrate().await?;
        let payload = SyncPayload {
            kind: "feedback".into(),
            memory_id: Some(1),
            edge_id: None,
            body: serde_json::json!({"signal":"used"}),
        };
        let published = publish(&db, "node-a", Some("/p"), "feedback", &payload).await?;
        let events = export_events(&db, Some("/p"), 0, 10).await?;
        assert_eq!(events.len(), 1);
        assert_eq!(import_events(&db, &events).await?, 0);
        assert!(published.inserted);
        let _ = std::fs::remove_file(db_path_string);
        Ok(())
    }
}
