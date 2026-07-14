//! Observer pass (memory-leadership roadmap Phase 2): observation-log
//! extraction alongside narrative compression.
//!
//! Narrative summarization is lossy — "attended an LGBTQ support group on
//! 7 May 2023" becomes "attended social events" and the date is gone forever.
//! The Observer takes the opposite contract, modeled on the observation-log
//! systems leading LongMemEval: an **append-only, timestamped,
//! priority-tagged log** of what specifically happened, was decided, or
//! changed. Each line is persisted as its own `kind='observation'` memory
//! through the governed write path (embeddings, chunking, FTS, ledger all
//! apply), linked to the session's narrative memory via `parent_memory_id`.
//!
//! Division of labor with the rest of the pipeline:
//! - narrative + atomic facts (compress.rs) remain the primary artifacts;
//!   the Observer log is additive and never destructive.
//! - the dream sweep's maturity promotion and the graph reconcile pass act as
//!   the Reflector: recurring observations graduate draft→stable→core, and
//!   durable relations are extracted into `memory_edges`.
//! - CCR blobs keep the verbatim transcript as the lossless floor
//!   (`retrieve_original`).

use anyhow::Result;

use crate::config::Config;
use crate::db::{self, Database};
use crate::embedder::Embedder;
use crate::governance::{MemoryGovernance, MemorySourceType, TrustTier};
use crate::provider;
use crate::vectorstore::VectorStore;

/// One parsed line of the Observer's log.
#[derive(Debug, Clone, PartialEq)]
pub struct ObserverLine {
    /// Resolved absolute date (YYYY-MM-DD) when the line states one.
    pub when: Option<String>,
    /// 1 = critical (decisions, identity facts), 2 = important, 3 = context.
    pub priority: u8,
    /// decision | fact | preference | error_fix | event
    pub category: String,
    pub text: String,
}

impl ObserverLine {
    /// Priority-scaled importance for `memory_meta`: critical observations
    /// outrank context lines at recall time.
    pub fn importance(&self) -> f64 {
        match self.priority {
            1 => 0.85,
            2 => 0.7,
            _ => 0.55,
        }
    }
}

pub fn build_observer_prompt(transcript: &str, session_date: &str) -> String {
    format!(
        "You are the Observer: the memory system's subconscious. You watch a session \
         and keep an append-only log of what specifically happened — you never \
         summarize it away.\n\
         Session date: {session_date}\n\n\
         TRANSCRIPT:\n{transcript}\n\n\
         Emit the observation log now. One observation per line, format exactly:\n\
         - [YYYY-MM-DD] P1 category: text\n\
         Rules:\n\
         - The bracketed date is the date the observed event happened (resolve \
           relative dates like 'last week' against the session date); write [-] \
           if no date applies.\n\
         - Priority: P1 = critical (decisions made, identity facts, corrections), \
           P2 = important (durable facts, preferences, configurations), \
           P3 = context (routine events worth keeping).\n\
         - category is one of: decision, fact, preference, error_fix, event.\n\
         - PRESERVE every specific: exact dates, proper nouns, quantities, file \
           names, error messages, and who did what. Never generalize.\n\
         - Each line must be self-contained (carry its own subject).\n\
         - Log what happened; do not narrate, interpret, or pad. Skip chit-chat.\n\
         - Aim for one line per distinct event/decision/fact."
    )
}

/// Parse the Observer's response. Tolerant: malformed lines are skipped, never
/// fatal — a partial log is still worth persisting.
pub fn parse_observer_response(raw: &str) -> Vec<ObserverLine> {
    let mut out = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("- ") else {
            continue;
        };
        // "[date] P1 category: text"
        let Some(rest) = rest.strip_prefix('[') else {
            continue;
        };
        let Some((date_part, rest)) = rest.split_once(']') else {
            continue;
        };
        let when = {
            let d = date_part.trim();
            if provider::is_valid_memory_date(d) {
                Some(d.to_string())
            } else {
                None
            }
        };
        let rest = rest.trim_start();
        let Some(rest) = rest.strip_prefix('P') else {
            continue;
        };
        let (priority, rest) = match rest.split_once(' ') {
            Some((p, rest)) => match p.trim().parse::<u8>() {
                Ok(p @ 1..=3) => (p, rest),
                _ => continue,
            },
            None => continue,
        };
        let Some((category, text)) = rest.split_once(':') else {
            continue;
        };
        let category = match category.trim().to_ascii_lowercase().as_str() {
            c @ ("decision" | "fact" | "preference" | "error_fix" | "event") => c.to_string(),
            _ => "event".to_string(),
        };
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        out.push(ObserverLine {
            when,
            priority,
            category,
            text: text.to_string(),
        });
    }
    out
}

/// Run the Observer over a session's observations and persist the log.
/// Returns the number of log lines stored. Best-effort by design: callers
/// treat an error as a warning, the narrative memory is already safe.
#[allow(clippy::too_many_arguments)]
pub async fn run_observer(
    db: &Database,
    embedder: Option<&dyn Embedder>,
    store: &dyn VectorStore,
    cfg: &Config,
    project: &str,
    session_id: &str,
    observations: &[db::Observation],
    narrative_memory_id: i64,
) -> Result<usize> {
    if observations.is_empty() {
        return Ok(0);
    }
    let transcript = crate::compress::build_transcript(observations);
    if transcript.trim().is_empty() {
        return Ok(0);
    }
    let session_date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let prompt = build_observer_prompt(&transcript, &session_date);
    let model = if cfg.observer.model.trim().is_empty() {
        cfg.model.clone()
    } else {
        cfg.observer.model.clone()
    };
    let raw = provider::complete_with(&prompt, &model, cfg).await?;
    let mut lines = parse_observer_response(&raw);
    lines.truncate(cfg.observer.max_lines.max(1));

    let stored = persist_lines(db, embedder, store, project, &lines, narrative_memory_id).await;
    tracing::info!(
        "observer: {} of {} parsed line(s) stored for session {} (project {})",
        stored,
        lines.len(),
        session_id,
        project
    );
    Ok(stored)
}

/// Persist parsed observer lines as governed `kind='observation'` memories.
/// Split from `run_observer` so the persistence contract is testable without
/// an LLM call. Returns how many lines stored (per-line failures are logged,
/// never fatal).
pub async fn persist_lines(
    db: &Database,
    embedder: Option<&dyn Embedder>,
    store: &dyn VectorStore,
    project: &str,
    lines: &[ObserverLine],
    narrative_memory_id: i64,
) -> usize {
    let mut stored = 0usize;
    for line in lines {
        let governance = MemoryGovernance {
            source_type: MemorySourceType::Derived,
            trust_tier: TrustTier::Medium,
            writer_identity: Some("ironmem:observer".to_string()),
            parent_memory_id: Some(narrative_memory_id),
            ..MemoryGovernance::default()
        };
        let tags = format!("observer p{} {}", line.priority, line.category);
        match crate::compress::remember_with_governance(
            db,
            embedder,
            store,
            project,
            "project",
            "observation",
            &line.text,
            Some(&tags),
            governance,
        )
        .await
        {
            Ok(memory_id) => {
                // Priority-scaled importance + the observed event's own date
                // (valid time), distinct from the write time.
                if let Err(e) = db::upsert_memory_meta(db, memory_id, line.importance()).await {
                    tracing::warn!("observer importance update failed (memory {memory_id}): {e}");
                }
                if let Some(when) = &line.when {
                    let _ = db::set_memory_event_time(db, memory_id, when).await;
                }
                stored += 1;
            }
            Err(e) => {
                tracing::warn!("observer line store failed: {e}");
            }
        }
    }
    stored
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dated_priority_tagged_lines() {
        let raw = "\
Some preamble the model added.
- [2023-05-07] P1 decision: Caroline joined the LGBTQ support group
- [-] P2 preference: Dave prefers espresso over filter coffee
- [2024-01-15] P3 event: routine dependency bump to tokio 1.50
- malformed line without brackets
- [not-a-date] P2 fact: Melanie painted a sunrise on canvas
- [2023-05-07] P9 fact: bad priority is skipped
";
        let lines = parse_observer_response(raw);
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0].when.as_deref(), Some("2023-05-07"));
        assert_eq!(lines[0].priority, 1);
        assert_eq!(lines[0].category, "decision");
        assert!(lines[0].text.contains("LGBTQ support group"));
        assert_eq!(lines[1].when, None);
        assert_eq!(lines[1].priority, 2);
        // Invalid date degrades to undated, the observation itself survives.
        assert_eq!(lines[3].when, None);
        assert!(lines[3].text.contains("sunrise"));
    }

    #[test]
    fn unknown_category_degrades_to_event_and_importance_scales() {
        let lines = parse_observer_response("- [-] P1 remark: something notable");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].category, "event");
        assert!(
            lines[0].importance()
                > ObserverLine {
                    when: None,
                    priority: 3,
                    category: "event".to_string(),
                    text: String::new(),
                }
                .importance()
        );
    }

    #[test]
    fn prompt_demands_specific_preservation_and_format() {
        let p = build_observer_prompt("## Edit\ninput: x\n", "2026-07-14");
        assert!(p.contains("append-only"));
        assert!(p.contains("PRESERVE every specific"));
        assert!(p.contains("[YYYY-MM-DD] P1 category: text"));
        assert!(p.contains("2026-07-14"));
    }
}
