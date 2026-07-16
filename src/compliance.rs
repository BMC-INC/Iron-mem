//! Compliance reporting (`ironmem compliance-report`, `GET /compliance/report`,
//! `GET /memory/{id}/lineage`).
//!
//! IronMem's governance layer already records everything an auditor asks for —
//! hash-chained write ledger, memory→injection lineage, consent/classification/
//! retention/legal-hold metadata, versioned brain snapshots. This module turns
//! those tables into one verifiable report, mapped to the EU AI Act's
//! record-keeping (Art. 12) and transparency (Art. 13) obligations, so the
//! moat is a document a customer can hand to their compliance team — not a
//! claim in a README.

use anyhow::Result;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

use crate::db::{self, Database};
use crate::governance::{ledger_entry_hash, sha256_hex};

/// Result of walking one namespace's ledger and re-deriving every hash.
#[derive(Debug, Clone, Serialize)]
pub struct ChainVerification {
    pub namespace: String,
    pub entries: usize,
    pub valid: bool,
    /// First ledger id whose linkage or recomputed hash failed, when invalid.
    pub first_broken_id: Option<i64>,
    /// Current forward-only epoch, when historical branches were migrated.
    pub epoch: Option<i64>,
    /// Immutable entries committed by the migration evidence bundle.
    pub historical_entries: usize,
    /// Historical fork points preserved in that bundle.
    pub historical_fork_points: usize,
}

/// Verify a namespace's ledger: every entry's `prev_hash` must equal the
/// previous entry's `entry_hash` (first entry: NULL), and every `entry_hash`
/// must re-derive from the entry's own fields. Any edit, deletion, or
/// reordering of history breaks one of the two.
pub async fn verify_ledger_chain(db: &Database, namespace: &str) -> Result<ChainVerification> {
    let entries = db::memory_ledger_for_namespace(db, namespace).await?;
    let mut prev: Option<String> = None;
    for entry in &entries {
        let linked = entry.prev_hash == prev;
        let derived = ledger_entry_hash(
            entry.prev_hash.as_deref(),
            &entry.namespace,
            entry.memory_id,
            &entry.op_type,
            entry.actor.as_deref(),
            &entry.payload,
            entry.created_at,
        );
        if !linked || derived != entry.entry_hash {
            return Ok(ChainVerification {
                namespace: namespace.to_string(),
                entries: entries.len(),
                valid: false,
                first_broken_id: Some(entry.id),
                epoch: None,
                historical_entries: 0,
                historical_fork_points: 0,
            });
        }
        prev = Some(entry.entry_hash.clone());
    }
    Ok(ChainVerification {
        namespace: namespace.to_string(),
        entries: entries.len(),
        valid: true,
        first_broken_id: None,
        epoch: None,
        historical_entries: 0,
        historical_fork_points: 0,
    })
}

/// Full memory→action lineage for one memory: what it is, who wrote it under
/// which governance, every ledger operation that touched it, and every
/// injection into an agent context (session, rank, triggering query).
#[derive(Debug, Clone, Serialize)]
pub struct MemoryLineage {
    pub memory_id: i64,
    pub summary: Option<String>,
    pub project: Option<String>,
    pub namespace: Option<String>,
    pub kind: Option<String>,
    pub writer_identity: Option<String>,
    pub source_type: Option<String>,
    pub trust_tier: Option<String>,
    pub classification: Option<String>,
    pub consent_state: Option<String>,
    pub retention_policy_id: Option<String>,
    pub legal_hold: bool,
    pub tombstoned_at: Option<i64>,
    /// Derivation chain: parent memory ids walked to the root (nearest first).
    pub parent_chain: Vec<i64>,
    pub ledger: Vec<db::MemoryLedgerEntry>,
    pub injections: Vec<db::InjectionEventInfo>,
}

pub async fn memory_lineage(db: &Database, memory_id: i64) -> Result<MemoryLineage> {
    let memory = db::get_memory_by_id_any_namespace(db, memory_id).await?;
    // A forgotten memory is physically purged (tombstone → ledger → delete);
    // its ledger entries are the surviving record. Don't synthesize default
    // governance metadata for a purged row — absent fields tell the truth.
    let meta = if memory.is_some() {
        db::get_memory_meta_full(db, memory_id).await.ok()
    } else {
        None
    };

    let mut parent_chain = Vec::new();
    let mut cursor = meta.as_ref().and_then(|m| m.parent_memory_id);
    while let Some(parent) = cursor {
        if parent_chain.contains(&parent) || parent_chain.len() >= 32 {
            break; // cycle/depth guard: lineage is a report, not a proof of DAG-ness
        }
        parent_chain.push(parent);
        cursor = db::get_memory_meta_full(db, parent)
            .await
            .ok()
            .and_then(|m| m.parent_memory_id);
    }

    Ok(MemoryLineage {
        memory_id,
        summary: memory.as_ref().map(|m| m.summary.clone()),
        project: memory.as_ref().map(|m| m.project.clone()),
        namespace: meta.as_ref().map(|m| m.namespace.clone()),
        kind: meta.as_ref().map(|m| m.kind.clone()),
        writer_identity: meta.as_ref().and_then(|m| m.writer_identity.clone()),
        source_type: meta.as_ref().map(|m| m.source_type.clone()),
        trust_tier: meta.as_ref().map(|m| m.trust_tier.clone()),
        classification: meta.as_ref().map(|m| m.classification.clone()),
        consent_state: meta.as_ref().and_then(|m| m.consent_state.clone()),
        retention_policy_id: meta.as_ref().and_then(|m| m.retention_policy_id.clone()),
        legal_hold: meta.as_ref().map(|m| m.legal_hold).unwrap_or(false),
        tombstoned_at: meta.as_ref().and_then(|m| m.tombstoned_at),
        parent_chain,
        ledger: db::memory_ledger_for_memory(db, memory_id).await?,
        injections: db::injection_events_for_memory(db, memory_id, 200).await?,
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct SnapshotInfo {
    pub id: String,
    pub label: Option<String>,
    pub project: Option<String>,
    pub memory_count: i64,
    pub edge_count: i64,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComplianceReport {
    pub generated_at: String,
    pub chains: Vec<ChainVerification>,
    pub inventory: Vec<db::GovernanceInventoryRow>,
    pub snapshots: Vec<SnapshotInfo>,
}

impl ComplianceReport {
    pub fn all_chains_valid(&self) -> bool {
        self.chains.iter().all(|c| c.valid)
    }

    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("# IronMem Compliance Report\n\n");
        out.push_str(&format!("- generated_at: `{}`\n", self.generated_at));
        out.push_str(&format!(
            "- ledger integrity: `{}`\n\n",
            if self.all_chains_valid() {
                "ALL CHAINS VALID"
            } else {
                "CHAIN VERIFICATION FAILED"
            }
        ));

        out.push_str("## Record-keeping (EU AI Act Art. 12)\n\n");
        out.push_str(
            "Every governed memory operation (write, update, forget) appends a \
             SHA-256 hash-chained entry to the memory ledger with actor, operation, \
             and timestamp. Chain verification below re-derives every hash: any \
             edit, deletion, or reordering of history is detected.\n\n",
        );
        out.push_str("| Namespace | Current entries | Current chain valid | Epoch | Historical entries | Historical forks | First broken id |\n");
        out.push_str("| --- | --- | --- | --- | --- | --- | --- |\n");
        for c in &self.chains {
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} |\n",
                c.namespace,
                c.entries,
                if c.valid { "yes" } else { "NO" },
                c.epoch
                    .map(|epoch| epoch.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                c.historical_entries,
                c.historical_fork_points,
                c.first_broken_id
                    .map(|i| i.to_string())
                    .unwrap_or_else(|| "-".to_string())
            ));
        }

        out.push_str("\n## Data governance inventory (Art. 12 / Art. 13 transparency)\n\n");
        out.push_str(
            "Counts of governed memories by namespace, classification, and consent \
             state, with retention/erasure controls. Traceability from any memory to \
             its writer, source, and every agent context it influenced is available \
             per-memory via `ironmem lineage <id>` / `GET /memory/{id}/lineage`.\n\n",
        );
        out.push_str(
            "| Namespace | Classification | Consent | Total | Legal holds | Tombstoned | With expiry | With retention policy |\n",
        );
        out.push_str("| --- | --- | --- | --- | --- | --- | --- | --- |\n");
        for row in &self.inventory {
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} | {} |\n",
                row.namespace,
                row.classification,
                row.consent_state.as_deref().unwrap_or("-"),
                row.total,
                row.legal_holds,
                row.tombstoned,
                row.with_expiry,
                row.with_retention_policy
            ));
        }

        out.push_str("\n## Versioned memory state (snapshots)\n\n");
        if self.snapshots.is_empty() {
            out.push_str("No brain snapshots recorded.\n");
        } else {
            out.push_str("| Snapshot | Label | Project | Memories | Edges | Created |\n");
            out.push_str("| --- | --- | --- | --- | --- | --- |\n");
            for s in &self.snapshots {
                out.push_str(&format!(
                    "| {} | {} | {} | {} | {} | {} |\n",
                    s.id,
                    s.label.as_deref().unwrap_or("-"),
                    s.project.as_deref().unwrap_or("-"),
                    s.memory_count,
                    s.edge_count,
                    s.created_at
                ));
            }
        }

        out.push_str(
            "\n## Controls in force\n\n\
             - PII/PHI writes fail closed without granted consent (`MemoryGovernance::validate`).\n\
             - Deletion is governed: legal holds block it, tombstones preserve auditability, \
             the ledger records the actor and reason, and vectors/blobs are purged.\n\
             - Reads are namespace-scoped and exclude tombstoned/expired memories at the SQL layer.\n\
             - Every injection of a memory into an agent context is recorded \
             (`injection_events`) with session, rank, and triggering query.\n",
        );
        out
    }
}

pub async fn generate(db: &Database) -> Result<ComplianceReport> {
    let mut chains = Vec::new();
    for namespace in db::list_ledger_namespaces(db).await? {
        chains.push(verify_current_ledger_epoch(db, &namespace).await?);
    }
    let inventory = db::governance_inventory(db).await?;
    let snapshots = db::list_brain_snapshots(db, 100)
        .await?
        .into_iter()
        .map(|s| SnapshotInfo {
            id: s.id,
            label: s.label,
            project: s.project,
            memory_count: s.memory_count,
            edge_count: s.edge_count,
            created_at: s.created_at,
        })
        .collect();
    Ok(ComplianceReport {
        generated_at: Utc::now().to_rfc3339(),
        chains,
        inventory,
        snapshots,
    })
}

/// One predecessor with multiple children in the historical append-only log.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LedgerForkEvidence {
    pub prev_hash: Option<String>,
    pub child_ids: Vec<i64>,
    pub child_hashes: Vec<String>,
}

/// Complete deterministic export committed by a forward-only migration receipt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LedgerEvidence {
    pub format_version: u32,
    pub namespace: String,
    pub entries: Vec<db::MemoryLedgerEntry>,
    pub forks: Vec<LedgerForkEvidence>,
    pub tip_hashes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LedgerEvidenceBundle {
    pub evidence_sha256: String,
    pub evidence: LedgerEvidence,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LedgerMigrationResult {
    pub namespace: String,
    pub evidence_sha256: String,
    pub prior_entries: usize,
    pub fork_points: usize,
    pub start_index: usize,
    pub epoch: db::MemoryLedgerEpoch,
}

pub async fn build_ledger_evidence(db: &Database, namespace: &str) -> Result<LedgerEvidenceBundle> {
    let entries = db::memory_ledger_for_namespace(db, namespace).await?;
    let namespace = crate::governance::normalize_namespace(namespace);
    let mut children: BTreeMap<Option<String>, Vec<&db::MemoryLedgerEntry>> = BTreeMap::new();
    let mut referenced = BTreeSet::new();
    for entry in &entries {
        children
            .entry(entry.prev_hash.clone())
            .or_default()
            .push(entry);
        if let Some(prev_hash) = &entry.prev_hash {
            referenced.insert(prev_hash.clone());
        }
    }
    let forks = children
        .into_iter()
        .filter(|(_, children)| children.len() > 1)
        .map(|(prev_hash, children)| LedgerForkEvidence {
            prev_hash,
            child_ids: children.iter().map(|entry| entry.id).collect(),
            child_hashes: children
                .iter()
                .map(|entry| entry.entry_hash.clone())
                .collect(),
        })
        .collect();
    let tip_hashes = entries
        .iter()
        .filter(|entry| !referenced.contains(&entry.entry_hash))
        .map(|entry| entry.entry_hash.clone())
        .collect();
    let evidence = LedgerEvidence {
        format_version: 1,
        namespace,
        entries,
        forks,
        tip_hashes,
    };
    let canonical = serde_json::to_vec(&evidence)?;
    Ok(LedgerEvidenceBundle {
        evidence_sha256: sha256_hex(&canonical),
        evidence,
    })
}

pub async fn apply_ledger_migration(
    db: &Database,
    bundle: &LedgerEvidenceBundle,
    actor: &str,
) -> Result<LedgerMigrationResult> {
    anyhow::ensure!(
        db::latest_memory_ledger_epoch(db, &bundle.evidence.namespace)
            .await?
            .is_none(),
        "namespace '{}' already has a ledger migration epoch",
        bundle.evidence.namespace
    );
    let canonical = serde_json::to_vec(&bundle.evidence)?;
    anyhow::ensure!(
        sha256_hex(&canonical) == bundle.evidence_sha256,
        "ledger evidence hash does not match its contents"
    );
    let last = bundle
        .evidence
        .entries
        .last()
        .ok_or_else(|| anyhow::anyhow!("cannot migrate an empty ledger"))?;
    let payload = serde_json::json!({
        "evidence_format_version": bundle.evidence.format_version,
        "evidence_sha256": bundle.evidence_sha256,
        "fork_points": bundle.evidence.forks.len(),
        "namespace": bundle.evidence.namespace,
        "prior_entries": bundle.evidence.entries.len(),
        "prior_last_entry_hash": last.entry_hash,
        "repair_mode": "forward_only_epoch",
    })
    .to_string();
    let epoch = db::append_memory_ledger_migration(
        db,
        &bundle.evidence.namespace,
        actor,
        &payload,
        &last.entry_hash,
        &bundle.evidence_sha256,
        bundle.evidence.entries.len() as i64,
    )
    .await?;
    Ok(LedgerMigrationResult {
        namespace: bundle.evidence.namespace.clone(),
        evidence_sha256: bundle.evidence_sha256.clone(),
        prior_entries: bundle.evidence.entries.len(),
        fork_points: bundle.evidence.forks.len(),
        start_index: bundle.evidence.entries.len(),
        epoch,
    })
}

/// Verify the current post-migration epoch. Historical forks remain separately
/// committed by the evidence bundle and are never relabeled as a linear chain.
pub async fn verify_current_ledger_epoch(
    db: &Database,
    namespace: &str,
) -> Result<ChainVerification> {
    let Some(epoch) = db::latest_memory_ledger_epoch(db, namespace).await? else {
        return verify_ledger_chain(db, namespace).await;
    };
    let all_entries = db::memory_ledger_for_namespace(db, namespace).await?;
    let mut child_counts: BTreeMap<Option<String>, usize> = BTreeMap::new();
    for entry in all_entries.iter().take(epoch.prior_entry_count as usize) {
        *child_counts.entry(entry.prev_hash.clone()).or_default() += 1;
    }
    let historical_fork_points = child_counts.values().filter(|count| **count > 1).count();
    let entries: Vec<_> = all_entries
        .into_iter()
        .filter(|entry| entry.id >= epoch.start_entry_id)
        .collect();
    let mut prev = entries.first().and_then(|entry| entry.prev_hash.clone());
    for entry in &entries {
        let linked = entry.prev_hash == prev;
        let derived = ledger_entry_hash(
            entry.prev_hash.as_deref(),
            &entry.namespace,
            entry.memory_id,
            &entry.op_type,
            entry.actor.as_deref(),
            &entry.payload,
            entry.created_at,
        );
        if !linked || derived != entry.entry_hash {
            return Ok(ChainVerification {
                namespace: namespace.to_string(),
                entries: entries.len(),
                valid: false,
                first_broken_id: Some(entry.id),
                epoch: Some(epoch.epoch),
                historical_entries: epoch.prior_entry_count as usize,
                historical_fork_points,
            });
        }
        prev = Some(entry.entry_hash.clone());
    }
    Ok(ChainVerification {
        namespace: namespace.to_string(),
        entries: entries.len(),
        valid: !entries.is_empty(),
        first_broken_id: None,
        epoch: Some(epoch.epoch),
        historical_entries: epoch.prior_entry_count as usize,
        historical_fork_points,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::governance::ledger_entry_hash;

    async fn test_db() -> Result<(Database, String)> {
        let path = std::env::temp_dir().join(format!(
            "ironmem-ledger-migration-{}.db",
            uuid::Uuid::new_v4()
        ));
        let path = path.to_string_lossy().to_string();
        let db = Database::new(&path).await?;
        db.migrate().await?;
        Ok((db, path))
    }

    async fn insert_raw_entry(
        db: &Database,
        memory_id: i64,
        prev_hash: Option<&str>,
        created_at: i64,
    ) -> Result<String> {
        let payload = format!(r#"{{"memory_id":{memory_id}}}"#);
        let hash = ledger_entry_hash(
            prev_hash,
            "legacy",
            Some(memory_id),
            "derive",
            Some("test"),
            &payload,
            created_at,
        );
        sqlx::query(
            "INSERT INTO memory_ledger(namespace,memory_id,op_type,actor,prev_hash,entry_hash,payload,created_at)
             VALUES($1,$2,$3,$4,$5,$6,$7,$8)",
        )
        .bind("legacy")
        .bind(memory_id)
        .bind("derive")
        .bind("test")
        .bind(prev_hash)
        .bind(&hash)
        .bind(payload)
        .bind(created_at)
        .execute(&db.pool)
        .await?;
        Ok(hash)
    }

    #[tokio::test]
    async fn migration_evidence_is_deterministic_and_maps_forks() -> Result<()> {
        let (db, path) = test_db().await?;
        let root = insert_raw_entry(&db, 1, None, 10).await?;
        insert_raw_entry(&db, 2, Some(&root), 11).await?;
        insert_raw_entry(&db, 3, Some(&root), 11).await?;

        let first = build_ledger_evidence(&db, "legacy").await?;
        let second = build_ledger_evidence(&db, "legacy").await?;
        assert_eq!(first.evidence_sha256, second.evidence_sha256);
        assert_eq!(first.evidence.entries.len(), 3);
        assert_eq!(first.evidence.forks.len(), 1);
        assert_eq!(first.evidence.forks[0].child_ids, vec![2, 3]);

        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[tokio::test]
    async fn migration_preserves_history_and_starts_a_valid_epoch() -> Result<()> {
        let (db, path) = test_db().await?;
        let root = insert_raw_entry(&db, 1, None, 10).await?;
        insert_raw_entry(&db, 2, Some(&root), 11).await?;
        insert_raw_entry(&db, 3, Some(&root), 11).await?;
        let before = db::memory_ledger_for_namespace(&db, "legacy").await?;
        let evidence = build_ledger_evidence(&db, "legacy").await?;

        let migration = apply_ledger_migration(&db, &evidence, "test:migrator").await?;
        let after = db::memory_ledger_for_namespace(&db, "legacy").await?;
        assert_eq!(&after[..before.len()], before.as_slice());
        assert_eq!(after[migration.start_index].op_type, "migration_genesis");
        assert_eq!(migration.evidence_sha256, evidence.evidence_sha256);

        db::append_memory_ledger(
            &db,
            "legacy",
            Some(4),
            "remember",
            Some("test"),
            r#"{"memory_id":4}"#,
        )
        .await?;
        let verification = verify_current_ledger_epoch(&db, "legacy").await?;
        assert!(verification.valid);
        assert_eq!(verification.entries, 2);

        let second_evidence = build_ledger_evidence(&db, "legacy").await?;
        assert!(
            apply_ledger_migration(&db, &second_evidence, "test:migrator")
                .await
                .is_err(),
            "an already migrated namespace must not silently start another epoch"
        );

        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[tokio::test]
    async fn migration_rejects_evidence_if_ledger_head_changed() -> Result<()> {
        let (db, path) = test_db().await?;
        insert_raw_entry(&db, 1, None, 10).await?;
        let stale = build_ledger_evidence(&db, "legacy").await?;
        db::append_memory_ledger(
            &db,
            "legacy",
            Some(2),
            "remember",
            Some("test"),
            r#"{"memory_id":2}"#,
        )
        .await?;

        assert!(apply_ledger_migration(&db, &stale, "test:migrator")
            .await
            .is_err());
        assert!(db::latest_memory_ledger_epoch(&db, "legacy")
            .await?
            .is_none());

        let _ = std::fs::remove_file(path);
        Ok(())
    }
}
