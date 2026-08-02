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
    #[serde(default)]
    pub evidence: Vec<SnapshotMemoryEvidence>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SnapshotMemoryEvidence {
    pub memory_id: i64,
    pub parent_memory_id: Option<i64>,
    pub evidence_root_id: String,
    pub derivation_depth: u32,
    pub roots: Vec<db::MemoryEvidenceRoot>,
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
    let mut evidence = Vec::with_capacity(memories.len());
    for memory in &memories {
        let meta = db::get_memory_meta_full(db, memory.id).await?;
        if let Some(evidence_root_id) = meta.evidence_root_id {
            evidence.push(SnapshotMemoryEvidence {
                memory_id: memory.id,
                parent_memory_id: meta.parent_memory_id,
                evidence_root_id,
                derivation_depth: meta.derivation_depth,
                roots: db::memory_evidence_roots(db, memory.id).await?,
            });
        }
    }
    let payload = SnapshotPayload {
        version: 2,
        project: project.map(ToOwned::to_owned),
        memories: memories.clone(),
        edges: edges.clone(),
        evidence,
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
    for evidence in &payload.evidence {
        let Some(new_id) = id_map.get(&evidence.memory_id).copied() else {
            continue;
        };
        let mapped_parent = evidence
            .parent_memory_id
            .and_then(|parent| id_map.get(&parent).copied());
        db::restore_memory_evidence(
            db,
            new_id,
            mapped_parent,
            &evidence.evidence_root_id,
            evidence.derivation_depth,
            &evidence.roots,
        )
        .await?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn snapshot_restore_preserves_evidence_roots_and_derivation_depth() -> Result<()> {
        let path = std::env::temp_dir().join(format!(
            "ironmem-snapshot-evidence-{}.db",
            uuid::Uuid::new_v4()
        ));
        let path = path.to_string_lossy().to_string();
        let db = Database::new(&path).await?;
        db.migrate().await?;
        let project = "/tmp/snapshot-evidence";
        let session = db::create_session(&db, project).await?;
        let parent = db::insert_memory(&db, project, &session, "snapshot parent", None).await?;
        db::apply_memory_governance(
            &db,
            parent,
            "project",
            "fact",
            &crate::governance::MemoryGovernance::explicit(),
            Some("test"),
            "remember",
        )
        .await?;
        let child = db::insert_memory(&db, project, &session, "snapshot child", None).await?;
        db::apply_memory_governance(
            &db,
            child,
            "project",
            "fact",
            &crate::governance::MemoryGovernance::derived_from(parent),
            Some("test"),
            "derive",
        )
        .await?;
        let original_root = db::get_memory_meta_full(&db, child)
            .await?
            .evidence_root_id
            .expect("child root");

        let snapshot = create(&db, Some("evidence"), Some(project)).await?;
        let payload = load_payload(&db, &snapshot.id).await?;
        assert_eq!(payload.version, 2);
        assert_eq!(payload.evidence.len(), 2);
        restore(&db, &snapshot.id, false).await?;

        let restored = db::get_recent_memories(&db, project, 10).await?;
        let child = restored
            .iter()
            .find(|memory| memory.summary == "snapshot child")
            .expect("restored child");
        let meta = db::get_memory_meta_full(&db, child.id).await?;
        assert_eq!(
            meta.evidence_root_id.as_deref(),
            Some(original_root.as_str())
        );
        assert_eq!(meta.derivation_depth, 1);
        assert!(meta.parent_memory_id.is_some());

        let _ = std::fs::remove_file(path);
        Ok(())
    }
}
