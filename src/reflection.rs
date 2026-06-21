//! Background reflection and consolidation.
//!
//! This is designed as a safe "sleep cycle": dry-run proposals are generated
//! first, and apply mode stores a new dense memory plus negative feedback on the
//! fragments it replaced. Existing memories are not deleted, so provenance stays
//! recoverable through normal search and CCR expansion.

use anyhow::Result;
use std::collections::{BTreeMap, HashMap, HashSet};

use crate::db::{self, Database, Memory, ReflectionProposal};
use crate::embedder::Embedder;
use crate::vectorstore::VectorStore;

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ReflectionReport {
    pub scanned: usize,
    pub proposals: usize,
    pub applied: usize,
    pub dry_run: bool,
    pub proposals_written: Vec<i64>,
}

const REFLECT_KINDS: &[&str] = &[
    "fact",
    "procedural",
    "preference",
    "architecture",
    "learned_pattern",
    "project_config",
    "error_solution",
];

fn normalize(text: &str) -> Vec<String> {
    let stop: HashSet<&str> = [
        "the", "and", "for", "with", "that", "this", "from", "into", "when", "then", "than",
        "should", "must", "will", "memory", "session",
    ]
    .into_iter()
    .collect();
    let mut terms: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .map(|s| s.to_ascii_lowercase())
        .filter(|s| s.len() >= 3 && !stop.contains(s.as_str()))
        .collect();
    terms.sort();
    terms.dedup();
    terms
}

fn jaccard(a: &[String], b: &[String]) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let aa: HashSet<&String> = a.iter().collect();
    let bb: HashSet<&String> = b.iter().collect();
    let inter = aa.intersection(&bb).count() as f64;
    let union = aa.union(&bb).count() as f64;
    inter / union
}

fn proposal_summary(kind: &str, group: &[Memory]) -> String {
    let mut lines = vec![format!(
        "Consolidated {kind} memory from {} overlapping fragments:",
        group.len()
    )];
    let mut seen = HashSet::new();
    for m in group {
        let s = m.summary.trim();
        if seen.insert(s.to_ascii_lowercase()) {
            lines.push(format!("- {s}"));
        }
    }
    lines.join("\n")
}

fn find_groups(memories: &[Memory]) -> Vec<Vec<Memory>> {
    let mut by_signature: BTreeMap<String, Vec<Memory>> = BTreeMap::new();
    let normalized: HashMap<i64, Vec<String>> = memories
        .iter()
        .map(|m| (m.id, normalize(&m.summary)))
        .collect();

    for m in memories {
        let terms = normalized.get(&m.id).cloned().unwrap_or_default();
        let signature = terms.iter().take(8).cloned().collect::<Vec<_>>().join(" ");
        if !signature.is_empty() {
            by_signature.entry(signature).or_default().push(m.clone());
        }
    }

    let mut groups: Vec<Vec<Memory>> = by_signature.into_values().filter(|g| g.len() > 1).collect();

    let mut used = HashSet::new();
    for i in 0..memories.len() {
        if used.contains(&memories[i].id) {
            continue;
        }
        let mut group = vec![memories[i].clone()];
        for j in (i + 1)..memories.len() {
            if used.contains(&memories[j].id) {
                continue;
            }
            let a = normalized
                .get(&memories[i].id)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let b = normalized
                .get(&memories[j].id)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            if jaccard(a, b) >= 0.50 {
                group.push(memories[j].clone());
            }
        }
        if group.len() > 1 {
            for m in &group {
                used.insert(m.id);
            }
            groups.push(group);
        }
    }

    groups
}

pub async fn run(
    db: &Database,
    embedder: Option<&dyn Embedder>,
    store: &dyn VectorStore,
    project: Option<&str>,
    dry_run: bool,
    apply: bool,
    limit: i64,
) -> Result<ReflectionReport> {
    let mut report = ReflectionReport {
        dry_run,
        ..Default::default()
    };
    let mut proposed_keys = HashSet::new();

    for kind in REFLECT_KINDS {
        let memories = db::get_memories_by_kind(db, project, kind, limit).await?;
        report.scanned += memories.len();
        for group in find_groups(&memories) {
            let ids: Vec<i64> = group.iter().map(|m| m.id).collect();
            let key = format!("{kind}:{ids:?}");
            if !proposed_keys.insert(key) {
                continue;
            }
            let summary = proposal_summary(kind, &group);
            report.proposals += 1;
            if !dry_run {
                let project_for_write = group
                    .first()
                    .map(|m| m.project.as_str())
                    .or(project)
                    .unwrap_or("global");
                let proposal_id =
                    db::insert_reflection_proposal(db, project_for_write, kind, &ids, &summary)
                        .await?;
                report.proposals_written.push(proposal_id);
                if apply {
                    let memory_id = crate::compress::remember(
                        db,
                        embedder,
                        store,
                        project_for_write,
                        "project",
                        kind,
                        &summary,
                        Some("reflection consolidated"),
                    )
                    .await?;
                    db::mark_reflection_proposal_applied(db, proposal_id).await?;
                    for id in ids {
                        let _ = db::record_memory_feedback(
                            db,
                            id,
                            project_for_write,
                            "consolidated",
                            -0.5,
                            Some(&format!("superseded by consolidated memory {memory_id}")),
                        )
                        .await;
                    }
                    report.applied += 1;
                }
            }
        }
    }
    Ok(report)
}

pub async fn list(
    db: &Database,
    project: Option<&str>,
    status: Option<&str>,
    limit: i64,
) -> Result<Vec<ReflectionProposal>> {
    db::reflection_proposals(db, project, status, limit).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_similar_fragments() {
        let a = Memory {
            id: 1,
            project: "p".into(),
            session_id: "s".into(),
            summary: "Always run cargo test before release".into(),
            tags: None,
            created_at: 1,
        };
        let b = Memory {
            id: 2,
            project: "p".into(),
            session_id: "s".into(),
            summary: "Run cargo test before releasing changes".into(),
            tags: None,
            created_at: 2,
        };
        assert_eq!(find_groups(&[a, b]).len(), 1);
    }
}
