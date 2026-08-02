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
    #[serde(default)]
    pub influence_policies: Vec<SnapshotMemoryInfluencePolicy>,
    #[serde(default)]
    pub contradiction_sets: Vec<crate::contradiction::ContradictionSet>,
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
pub struct SnapshotMemoryInfluencePolicy {
    pub memory_id: i64,
    pub policy: crate::influence::MemoryInfluencePolicy,
    pub updated_by: Option<String>,
    pub updated_at: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RestoreReport {
    pub snapshot_id: String,
    pub memories_in_snapshot: usize,
    pub edges_in_snapshot: usize,
    pub influence_policies_in_snapshot: usize,
    pub contradiction_sets_in_snapshot: usize,
    pub dry_run: bool,
    pub restored_memories: usize,
    pub restored_edges: usize,
    pub restored_influence_policies: usize,
    pub restored_contradiction_sets: usize,
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
    let mut influence_policies = Vec::new();
    let mut contradiction_sets = std::collections::BTreeMap::new();
    for memory in &memories {
        let meta = db::get_memory_meta_full(db, memory.id).await?;
        if let Some(record) =
            db::get_explicit_memory_influence_policy(db, memory.id, &meta.namespace).await?
        {
            influence_policies.push(SnapshotMemoryInfluencePolicy {
                memory_id: memory.id,
                policy: record.policy,
                updated_by: record.updated_by,
                updated_at: record.updated_at,
            });
        }
        for set in db::contradiction_sets_for_memory(db, memory.id, &meta.namespace).await? {
            contradiction_sets.entry(set.id.clone()).or_insert(set);
        }
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
        version: 4,
        project: project.map(ToOwned::to_owned),
        memories: memories.clone(),
        edges: edges.clone(),
        evidence,
        influence_policies,
        contradiction_sets: contradiction_sets.into_values().collect(),
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
        influence_policies_in_snapshot: payload.influence_policies.len(),
        contradiction_sets_in_snapshot: payload.contradiction_sets.len(),
        dry_run,
        restored_memories: 0,
        restored_edges: 0,
        restored_influence_policies: 0,
        restored_contradiction_sets: 0,
    };
    if dry_run {
        return Ok(report);
    }

    let project = payload.project.as_deref();
    if let Some(p) = project {
        db::delete_project_contradiction_sets(db, p).await?;
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
    for stored_policy in &payload.influence_policies {
        let Some(new_id) = id_map.get(&stored_policy.memory_id).copied() else {
            continue;
        };
        db::restore_memory_influence_policy(
            db,
            new_id,
            &stored_policy.policy,
            stored_policy.updated_by.as_deref(),
            stored_policy.updated_at,
        )
        .await?;
        report.restored_influence_policies += 1;
    }
    for set in &payload.contradiction_sets {
        if db::restore_contradiction_set(db, set, &id_map).await? {
            report.restored_contradiction_sets += 1;
        }
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
        let policy = db::update_memory_influence_policy(
            &db,
            child,
            "local",
            &crate::influence::PolicyPrincipal::local_operator("snapshot:test"),
            &crate::influence::PolicyMutationRequest {
                expected_version: 1,
                patch: crate::influence::MemoryInfluencePolicyPatch {
                    state: Some(crate::influence::InfluenceState::ReasoningOnly),
                    requires_original_source: Some(true),
                    ..Default::default()
                },
                reason: "snapshot policy preservation".to_string(),
                request_id: "snapshot-policy-test".to_string(),
            },
        )
        .await?;
        crate::contradiction::create(
            &db,
            &crate::influence::PolicyPrincipal::local_operator("snapshot:test"),
            &crate::contradiction::CreateContradictionRequest {
                namespace: "local".into(),
                realm: "project".into(),
                project: Some(project.into()),
                claim_key: "snapshot.test.claim".into(),
                cardinality: crate::contradiction::ClaimCardinality::Single,
                members: vec![
                    crate::contradiction::ContradictionMember {
                        memory_id: parent,
                        stance: crate::contradiction::MemberStance::Competing,
                    },
                    crate::contradiction::ContradictionMember {
                        memory_id: child,
                        stance: crate::contradiction::MemberStance::Competing,
                    },
                ],
                reason: "snapshot contradiction preservation".into(),
            },
        )
        .await?;

        let snapshot = create(&db, Some("evidence"), Some(project)).await?;
        let payload = load_payload(&db, &snapshot.id).await?;
        assert_eq!(payload.version, 4);
        assert_eq!(payload.evidence.len(), 2);
        assert_eq!(payload.influence_policies.len(), 1);
        assert_eq!(payload.contradiction_sets.len(), 1);
        let report = restore(&db, &snapshot.id, false).await?;
        assert_eq!(report.restored_contradiction_sets, 1);

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
        let restored_policy = db::get_memory_influence_policy(&db, child.id, "local").await?;
        assert_eq!(restored_policy.policy, policy.policy);
        let restored_sets = db::contradiction_sets_for_memory(&db, child.id, "local").await?;
        assert_eq!(restored_sets.len(), 1);
        assert_eq!(restored_sets[0].members.len(), 2);

        let _ = std::fs::remove_file(path);
        Ok(())
    }
}
