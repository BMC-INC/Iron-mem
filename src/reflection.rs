//! Background reflection and consolidation.
//!
//! This is designed as a safe "sleep cycle": dry-run proposals are generated
//! first, and apply mode stores a new dense memory plus negative feedback on the
//! fragments it replaced. Existing memories are not deleted, so provenance stays
//! recoverable through normal search and CCR expansion.

use anyhow::Result;
use std::collections::{BTreeMap, HashMap, HashSet};

use crate::config::Config;
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

// ── Transitive synthesis (Track B) ──────────────────────────────────────────
//
// Distinct from `run` (which textually CONSOLIDATES near-duplicates and records
// −0.5 on the fragments it supersedes). Synthesis COMBINES complementary facts
// that share an entity into NEW derived facts the store never held explicitly,
// so a multi-hop question collapses to a single-hop retrieval. Derived memories
// are ADDITIVE (originals untouched), and every source fact gets a POSITIVE
// feedback bump — which populates the #5 temporal-trust signal
// (`trust_ref_count` / `trust_last_validated_at`), so corroborated facts that
// fed a derivation also rank higher in retrieval. Paper: M3 transitive reasoning
// over stored memory + Finding 4 (trust earned over time, not a static scalar).

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct SynthesisReport {
    pub scanned: usize,
    pub groups: usize,
    pub derived: usize,
    pub sources_reinforced: usize,
    pub dry_run: bool,
    pub derived_ids: Vec<i64>,
}

/// Group facts that share a salient (entity-like) term but aren't pure
/// duplicates, so the model has complementary material to chain. Each group is a
/// small set of distinct memories co-mentioning one term. Ubiquitous terms
/// (linking too many memories to be a specific entity) are skipped.
fn find_synthesis_groups(
    memories: &[Memory],
    max_group: usize,
    max_groups: usize,
) -> Vec<Vec<Memory>> {
    let normalized: HashMap<i64, Vec<String>> = memories
        .iter()
        .map(|m| (m.id, normalize(&m.summary)))
        .collect();
    let by_id: HashMap<i64, &Memory> = memories.iter().map(|m| (m.id, m)).collect();
    let mut by_term: BTreeMap<String, Vec<i64>> = BTreeMap::new();
    for m in memories {
        if let Some(terms) = normalized.get(&m.id) {
            for t in terms.iter().filter(|t| t.len() >= 4) {
                by_term.entry(t.clone()).or_default().push(m.id);
            }
        }
    }
    let mut groups: Vec<Vec<Memory>> = Vec::new();
    let mut seen: HashSet<Vec<i64>> = HashSet::new();
    for (_term, mut ids) in by_term {
        ids.sort_unstable();
        ids.dedup();
        if ids.len() < 2 || ids.len() > max_group.saturating_mul(3) {
            continue;
        }
        ids.truncate(max_group);
        if !seen.insert(ids.clone()) {
            continue;
        }
        let group: Vec<Memory> = ids
            .iter()
            .filter_map(|id| by_id.get(id).map(|m| (*m).clone()))
            .collect();
        if group.len() >= 2 {
            groups.push(group);
            if groups.len() >= max_groups {
                break;
            }
        }
    }
    groups
}

fn build_synthesis_prompt(group: &[Memory]) -> String {
    let mut p = String::from(
        "You combine existing memory facts via multi-hop reasoning.\n\
         Below are related facts about overlapping entities. Output ONLY NEW facts \
         that REQUIRE combining two or more lines — facts not already stated by any \
         single line. Resolve pronouns and relative references to concrete entities \
         so each derived fact stands alone. One fact per line, terse. If nothing can \
         be soundly derived, output exactly: NONE\n\nFacts:\n",
    );
    let mut seen = HashSet::new();
    for m in group {
        let s = m.summary.trim();
        if seen.insert(s.to_ascii_lowercase()) {
            p.push_str("- ");
            p.push_str(s);
            p.push('\n');
        }
    }
    p.push_str("\nDerived facts:\n");
    p
}

fn parse_derived_facts(reply: &str, inputs: &[Memory], max_per_group: usize) -> Vec<String> {
    let input_set: HashSet<String> = inputs
        .iter()
        .map(|m| m.summary.trim().to_ascii_lowercase())
        .collect();
    let mut out = Vec::new();
    for raw in reply.lines() {
        let line = raw.trim().trim_start_matches(['-', '*', '•']).trim();
        // strip a leading enumerator like "1." / "2)"
        let line = line
            .trim_start_matches(|c: char| c.is_ascii_digit() || c == '.' || c == ')' || c == ' ')
            .trim();
        if line.len() < 8 {
            continue;
        }
        let lc = line.to_ascii_lowercase();
        if lc == "none" || lc == "derived facts:" || input_set.contains(&lc) {
            continue;
        }
        out.push(line.to_string());
        if out.len() >= max_per_group {
            break;
        }
    }
    out
}

/// Derive new multi-hop facts from existing facts and store them additively,
/// reinforcing each source (which feeds the #5 trust signal). `apply=false` is a
/// dry run (counts only, no writes, no LLM-free — it still queries the model to
/// report how many facts WOULD be derived).
#[allow(clippy::too_many_arguments)]
pub async fn synthesize(
    db: &Database,
    embedder: Option<&dyn Embedder>,
    store: &dyn VectorStore,
    config: &Config,
    project: Option<&str>,
    apply: bool,
    limit: i64,
    max_groups: usize,
) -> Result<SynthesisReport> {
    let mut report = SynthesisReport {
        dry_run: !apply,
        ..Default::default()
    };
    // Facts are the multi-hop substrate.
    let memories = db::get_memories_by_kind(db, project, "fact", limit).await?;
    report.scanned = memories.len();
    let groups = find_synthesis_groups(&memories, 6, max_groups);
    report.groups = groups.len();

    for group in groups {
        let prompt = build_synthesis_prompt(&group);
        // A single group's LLM failure shouldn't abort the whole pass.
        let reply = match crate::provider::complete_with(&prompt, &config.model, config).await {
            Ok(r) => r,
            Err(_) => continue,
        };
        let derived = parse_derived_facts(&reply, &group, 4);
        if derived.is_empty() {
            continue;
        }
        let project_for_write = group
            .first()
            .map(|m| m.project.as_str())
            .or(project)
            .unwrap_or("global");
        if !apply {
            report.derived += derived.len();
            continue;
        }
        for fact in &derived {
            let id = crate::compress::remember(
                db,
                embedder,
                store,
                project_for_write,
                "project",
                "fact",
                fact,
                Some("synthesized,derived"),
            )
            .await?;
            report.derived_ids.push(id);
            report.derived += 1;
        }
        // Reinforce the sources: positive feedback bumps trust_ref_count +
        // trust_last_validated_at (the #5 signal), so facts that corroborated a
        // derivation rank higher when temporal_trust.weight > 0.
        for m in &group {
            if db::record_memory_feedback(
                db,
                m.id,
                project_for_write,
                "synthesized",
                0.5,
                Some("source of a synthesized fact"),
            )
            .await
            .is_ok()
            {
                report.sources_reinforced += 1;
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
