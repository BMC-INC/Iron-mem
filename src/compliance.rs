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
use serde::Serialize;

use crate::db::{self, Database};
use crate::governance::ledger_entry_hash;

/// Result of walking one namespace's ledger and re-deriving every hash.
#[derive(Debug, Clone, Serialize)]
pub struct ChainVerification {
    pub namespace: String,
    pub entries: usize,
    pub valid: bool,
    /// First ledger id whose linkage or recomputed hash failed, when invalid.
    pub first_broken_id: Option<i64>,
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
            });
        }
        prev = Some(entry.entry_hash.clone());
    }
    Ok(ChainVerification {
        namespace: namespace.to_string(),
        entries: entries.len(),
        valid: true,
        first_broken_id: None,
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
        out.push_str("| Namespace | Ledger entries | Chain valid | First broken id |\n");
        out.push_str("| --- | --- | --- | --- |\n");
        for c in &self.chains {
            out.push_str(&format!(
                "| {} | {} | {} | {} |\n",
                c.namespace,
                c.entries,
                if c.valid { "yes" } else { "NO" },
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
        chains.push(verify_ledger_chain(db, &namespace).await?);
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
