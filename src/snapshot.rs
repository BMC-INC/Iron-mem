//! Git-like brain-state snapshots backed by CCR.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::db::{self, BrainSnapshot, Database, Memory, MemoryEdge};

#[derive(Debug, Serialize, Deserialize)]
pub struct SnapshotPayload {
    pub version: u32,
    pub project: Option<String>,
    pub memories: Vec<Memory>,
    pub edges: Vec<MemoryEdge>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RestoreReport {
    pub snapshot_id: String,
    pub memories_in_snapshot: usize,
    pub edges_in_snapshot: usize,
    pub dry_run: bool,
    pub restored_memories: usize,
    pub restored_edges: usize,
}

pub async fn create(
    db: &Database,
    label: Option<&str>,
    project: Option<&str>,
) -> Result<BrainSnapshot> {
    let memories = match project {
        Some(p) => db::get_recent_memories(db, p, i64::MAX).await?,
        None => db::get_all_memories(db, i64::MAX).await?,
    };
    let edges = db::all_memory_edges(db, project).await?;
    let payload = SnapshotPayload {
        version: 1,
        project: project.map(ToOwned::to_owned),
        memories: memories.clone(),
        edges: edges.clone(),
    };
    let bytes = serde_json::to_vec_pretty(&payload)?;
    let blob = crate::ccr::store_blob(db, &bytes, Some("json")).await?;
    let id = format!("snap-{}", Uuid::new_v4());
    db::insert_brain_snapshot(
        db,
        &id,
        label,
        project,
        memories.len() as i64,
        edges.len() as i64,
        &blob.hash,
    )
    .await?;
    Ok(BrainSnapshot {
        id,
        label: label.map(ToOwned::to_owned),
        project: project.map(ToOwned::to_owned),
        memory_count: memories.len() as i64,
        edge_count: edges.len() as i64,
        blob_hash: blob.hash,
        created_at: chrono::Utc::now().timestamp(),
    })
}

pub async fn load_payload(db: &Database, snapshot_id: &str) -> Result<SnapshotPayload> {
    let snap = db::brain_snapshot(db, snapshot_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("snapshot not found: {snapshot_id}"))?;
    let bytes = crate::ccr::load_blob(db, &snap.blob_hash).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

pub async fn restore(db: &Database, snapshot_id: &str, dry_run: bool) -> Result<RestoreReport> {
    let payload = load_payload(db, snapshot_id).await?;
    let mut report = RestoreReport {
        snapshot_id: snapshot_id.to_string(),
        memories_in_snapshot: payload.memories.len(),
        edges_in_snapshot: payload.edges.len(),
        dry_run,
        restored_memories: 0,
        restored_edges: 0,
    };
    if dry_run {
        return Ok(report);
    }

    let project = payload.project.as_deref();
    if let Some(p) = project {
        let existing = db::memory_ids_for_project(db, p).await?;
        for id in existing {
            let _ = db::decref_memory_session_blob(db, id).await;
            let _ = db::delete_memory(db, id).await;
            let _ = db::delete_memory_edges(db, id).await;
            let _ = db::delete_memory_chunks(db, id).await;
            let _ = db::delete_embedding(db, "memory", id).await;
            let _ = db::delete_memory_meta(db, id).await;
        }
    } else {
        anyhow::bail!("global restore is intentionally blocked; restore a project snapshot");
    }

    let mut id_map = std::collections::HashMap::new();
    for memory in &payload.memories {
        let new_id = db::insert_memory(
            db,
            &memory.project,
            &memory.session_id,
            &memory.summary,
            memory.tags.as_deref(),
        )
        .await?;
        id_map.insert(memory.id, new_id);
        report.restored_memories += 1;
    }
    for edge in &payload.edges {
        let new_edge = db::NewMemoryEdge {
            project: edge.project.clone(),
            memory_id: id_map
                .get(&edge.memory_id)
                .copied()
                .unwrap_or(edge.memory_id),
            source: edge.source.clone(),
            relation: edge.relation.clone(),
            target: edge.target.clone(),
            valid_from: edge.valid_from.clone(),
            valid_until: edge.valid_until.clone(),
            confidence: edge.confidence,
        };
        let _ = db::insert_memory_edge(db, &new_edge).await;
        report.restored_edges += 1;
    }
    Ok(report)
}
