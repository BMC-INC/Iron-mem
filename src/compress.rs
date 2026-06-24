//! Shared session-compression pipeline. One implementation drives every
//! surface (MCP, REST, CLI) so importance scoring and inline embedding can
//! never drift between them. The LLM call and the persistence path are split
//! so the persistence half is unit-testable without network access.

use anyhow::Result;
use std::collections::HashSet;

use crate::config::Config;
use crate::db::{self, Database};
use crate::embedder::Embedder;
use crate::governance::MemoryGovernance;
use crate::provider::{self, CompressionResult};
use crate::vectorstore::VectorStore;

/// Compress a session into a memory: summarize via the LLM, persist it, record
/// importance, and (best-effort) embed it for semantic recall.
pub async fn run(
    db: &Database,
    embedder: Option<&dyn Embedder>,
    store: &dyn VectorStore,
    cfg: &Config,
    session_id: &str,
) -> Result<i64> {
    let session = db::get_session(db, session_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Session not found: {}", session_id))?;

    let observations = db::get_observations_for_session(db, session_id).await?;
    let result = provider::compress(&observations, cfg).await?;
    let chunk_result = result.clone();

    let memory_id = persist(db, embedder, store, &session.project, session_id, result).await?;

    // CCR: preserve the verbatim pre-LLM transcript behind the lossy summary so
    // it can be retrieved later. Best-effort — never fail a successful
    // compression because the transcript blob could not be stored.
    match store_session_transcript(db, memory_id, &observations).await {
        Ok(hash) => {
            if let Err(e) = persist_memory_skim(
                db,
                memory_id,
                &session.project,
                session_id,
                &chunk_result,
                &observations,
                hash.as_deref(),
            )
            .await
            {
                tracing::warn!("memory skim store failed (memory {memory_id}): {e}");
            }
        }
        Err(e) => {
            tracing::warn!("CCR session transcript store failed (memory {memory_id}): {e}");
        }
    }

    // Feedback loop: mine error→fix corrections into error_solution memories.
    // Best-effort — never fail compression because mining hiccuped.
    if let Err(e) = crate::corrections::mine_and_store(
        db,
        embedder,
        store,
        &session.project,
        session_id,
        &observations,
    )
    .await
    {
        tracing::warn!("correction mining failed (session {session_id}): {e}");
    }
    Ok(memory_id)
}

#[derive(Debug, Clone)]
struct TranscriptSection {
    observation_id: i64,
    tool: String,
    start: usize,
    end: usize,
    text: String,
}

/// Render observations into a plain-text transcript (the pre-LLM session view)
/// and retain byte ranges for chunk-level exact expansion.
fn build_transcript_with_sections(
    observations: &[db::Observation],
) -> (String, Vec<TranscriptSection>) {
    let mut s = String::new();
    let mut sections = Vec::new();
    for o in observations {
        let start = s.len();
        s.push_str("## ");
        s.push_str(&o.tool);
        s.push('\n');
        if let Some(input) = &o.input {
            s.push_str("input: ");
            s.push_str(input);
            s.push('\n');
        }
        if let Some(output) = &o.output {
            s.push_str("output: ");
            s.push_str(output);
            s.push('\n');
        }
        s.push('\n');
        let end = s.len();
        sections.push(TranscriptSection {
            observation_id: o.id,
            tool: o.tool.clone(),
            start,
            end,
            text: s[start..end].to_string(),
        });
    }
    (s, sections)
}

/// Render observations into a plain-text transcript (the pre-LLM session view).
fn build_transcript(observations: &[db::Observation]) -> String {
    build_transcript_with_sections(observations).0
}

/// Store the verbatim session transcript as a CCR blob and link it to `memory_id`
/// via `memory_meta.session_blob`. Returns the blob hash, or `None` when there
/// were no observations to record.
pub async fn store_session_transcript(
    db: &Database,
    memory_id: i64,
    observations: &[db::Observation],
) -> Result<Option<String>> {
    let transcript = build_transcript(observations);
    if transcript.is_empty() {
        return Ok(None);
    }
    let hash = crate::ccr::store_blob(db, transcript.as_bytes(), None)
        .await?
        .hash;
    db::set_memory_session_blob(db, memory_id, &hash).await?;
    Ok(Some(hash))
}

fn word_count(s: &str) -> usize {
    s.split_whitespace().count()
}

fn token_estimate(s: &str) -> i64 {
    ((word_count(s) * 4).saturating_add(2) / 3).max(1) as i64
}

fn clip_words(s: &str, max_words: usize) -> String {
    let words: Vec<&str> = s.split_whitespace().collect();
    if words.len() <= max_words {
        return s.trim().to_string();
    }
    format!("{}...", words[..max_words].join(" "))
}

fn memory_density(result: &CompressionResult) -> &'static str {
    if result.kind == "fact"
        || result.kind == "procedural"
        || result.kind == "error_solution"
        || result.event_time.is_some()
        || !result.facts.is_empty()
        || !result.procedures.is_empty()
        || result.importance >= 8
    {
        "high"
    } else if matches!(
        result.kind.as_str(),
        "architecture" | "learned_pattern" | "project_config" | "preference" | "profile"
    ) || result.importance >= 6
    {
        "medium"
    } else {
        "low"
    }
}

fn section_density(section: &TranscriptSection, parent_density: &str) -> &'static str {
    let text = section.text.to_ascii_lowercase();
    if text.contains("error")
        || text.contains("failed")
        || text.contains("panic")
        || text.contains("fix")
        || text.contains("test result")
        || text.contains("commit")
        || text.contains("push")
    {
        "high"
    } else if parent_density == "high" || word_count(&section.text) > 160 {
        "medium"
    } else {
        "low"
    }
}

fn chunk_summary(text: &str, density: &str) -> String {
    match density {
        "high" => clip_words(text, 90),
        "medium" => clip_words(text, 48),
        _ => clip_words(text, 24),
    }
}

fn chunk_title(prefix: &str, text: &str) -> String {
    let first_line = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or(prefix)
        .trim()
        .trim_start_matches("## ")
        .trim();
    let clipped = clip_words(first_line, 10);
    if clipped.is_empty() {
        prefix.to_string()
    } else {
        clipped
    }
}

fn source_terms(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .map(|s| s.to_ascii_lowercase())
        .filter(|s| s.chars().count() >= 4)
        .filter(|s| {
            !matches!(
                s.as_str(),
                "that"
                    | "this"
                    | "with"
                    | "from"
                    | "have"
                    | "into"
                    | "will"
                    | "should"
                    | "memory"
                    | "session"
            )
        })
        .collect()
}

/// Best-effort evidence locator for LLM-derived fact/procedure chunks. Exact
/// substring matches get tight byte spans. When compression rewrites the fact,
/// fall back to the observation section with the highest content-word overlap.
fn best_source_span(
    text: &str,
    transcript: &str,
    sections: &[TranscriptSection],
) -> Option<(i64, i64)> {
    let needle = text.trim();
    if needle.chars().count() >= 8 {
        let transcript_lower = transcript.to_ascii_lowercase();
        let needle_lower = needle.to_ascii_lowercase();
        if let Some(start) = transcript_lower.find(&needle_lower) {
            return Some((start as i64, (start + needle.len()) as i64));
        }
    }

    let terms = source_terms(needle);
    if terms.is_empty() {
        return None;
    }
    let mut best: Option<(usize, usize, usize)> = None;
    for section in sections {
        let section_terms = source_terms(&section.text);
        let overlap = terms
            .iter()
            .filter(|term| section_terms.contains(*term))
            .count();
        if overlap >= 2 && best.is_none_or(|(score, _, _)| overlap > score) {
            best = Some((overlap, section.start, section.end));
        }
    }
    best.map(|(_, start, end)| (start as i64, end as i64))
}

async fn persist_memory_skim(
    db: &Database,
    memory_id: i64,
    project: &str,
    session_id: &str,
    result: &CompressionResult,
    observations: &[db::Observation],
    source_hash: Option<&str>,
) -> Result<()> {
    let parent_density = memory_density(result);
    let mut chunks = Vec::new();
    let mut ordinal = 0_i64;
    let (transcript, sections) = if source_hash.is_some() && !observations.is_empty() {
        let (transcript, sections) = build_transcript_with_sections(observations);
        (Some(transcript), sections)
    } else {
        (None, Vec::new())
    };

    chunks.push(db::NewMemoryChunk {
        chunk_id: format!("mem:{memory_id}:overview"),
        project: project.to_string(),
        memory_id,
        session_id: session_id.to_string(),
        ordinal,
        density: parent_density.to_string(),
        kind: result.kind.clone(),
        title: "Memory overview".to_string(),
        summary: chunk_summary(&result.summary, parent_density),
        source_hash: None,
        source_start: None,
        source_end: None,
        token_estimate: token_estimate(&result.summary),
    });
    ordinal += 1;

    for (idx, fact) in result.facts.iter().enumerate() {
        let source_span = transcript
            .as_deref()
            .and_then(|t| best_source_span(fact, t, &sections));
        chunks.push(db::NewMemoryChunk {
            chunk_id: format!("mem:{memory_id}:fact:{}", idx + 1),
            project: project.to_string(),
            memory_id,
            session_id: session_id.to_string(),
            ordinal,
            density: "high".to_string(),
            kind: "fact".to_string(),
            title: chunk_title("Fact", fact),
            summary: fact.trim().to_string(),
            source_hash: source_span.and(source_hash.map(str::to_string)),
            source_start: source_span.map(|(start, _)| start),
            source_end: source_span.map(|(_, end)| end),
            token_estimate: token_estimate(fact),
        });
        ordinal += 1;
    }

    for (idx, procedure) in result.procedures.iter().enumerate() {
        let source_span = transcript
            .as_deref()
            .and_then(|t| best_source_span(procedure, t, &sections));
        chunks.push(db::NewMemoryChunk {
            chunk_id: format!("mem:{memory_id}:procedure:{}", idx + 1),
            project: project.to_string(),
            memory_id,
            session_id: session_id.to_string(),
            ordinal,
            density: "high".to_string(),
            kind: "procedural".to_string(),
            title: chunk_title("Procedure", procedure),
            summary: procedure.trim().to_string(),
            source_hash: source_span.and(source_hash.map(str::to_string)),
            source_start: source_span.map(|(start, _)| start),
            source_end: source_span.map(|(_, end)| end),
            token_estimate: token_estimate(procedure),
        });
        ordinal += 1;
    }

    if source_hash.is_some() && !sections.is_empty() {
        for section in sections {
            let density = section_density(&section, parent_density);
            let title = format!(
                "{} observation {}",
                chunk_title(&section.tool, &section.text),
                section.observation_id
            );
            chunks.push(db::NewMemoryChunk {
                chunk_id: format!("mem:{memory_id}:obs:{}", section.observation_id),
                project: project.to_string(),
                memory_id,
                session_id: session_id.to_string(),
                ordinal,
                density: density.to_string(),
                kind: result.kind.clone(),
                title,
                summary: chunk_summary(&section.text, density),
                source_hash: source_hash.map(str::to_string),
                source_start: Some(section.start as i64),
                source_end: Some(section.end as i64),
                token_estimate: token_estimate(&section.text),
            });
            ordinal += 1;
        }
    }

    db::replace_memory_chunks(db, memory_id, &chunks).await
}

/// Persist an already-computed compression result. Inserts the memory, marks
/// the session compressed, records importance, then best-effort embeds the
/// memory (embedding failures are logged, never fatal — local-first posture).
pub async fn persist(
    db: &Database,
    embedder: Option<&dyn Embedder>,
    store: &dyn VectorStore,
    project: &str,
    session_id: &str,
    result: CompressionResult,
) -> Result<i64> {
    let memory_id =
        db::insert_memory(db, project, session_id, &result.summary, Some(&result.tags)).await?;
    db::mark_compressed(db, session_id).await?;
    db::upsert_memory_meta(db, memory_id, result.importance as f64 / 10.0).await?;
    // Compressed sessions are project-scoped; record the LLM-classified kind
    // (importance is preserved — set_memory_scope_kind only touches scope+kind).
    db::set_memory_scope_kind(db, memory_id, "project", &result.kind).await?;
    if let Err(e) = db::apply_memory_governance(
        db,
        memory_id,
        "project",
        &result.kind,
        &MemoryGovernance::compressed_session(),
        Some("ironmem:compress"),
        "remember",
    )
    .await
    {
        tracing::warn!("memory governance store failed (memory {memory_id}): {e}");
    }
    // Temporal tag: stamp the session's stated event date on the narrative so
    // the time-aware retrieval boost can surface it for date-anchored questions.
    if let Some(when) = &result.event_time {
        if let Err(e) = db::set_memory_event_time(db, memory_id, when).await {
            tracing::warn!("event_time store failed (memory {memory_id}): {e}");
        }
    }
    if let Err(e) =
        persist_memory_skim(db, memory_id, project, session_id, &result, &[], None).await
    {
        tracing::warn!("memory skim store failed (memory {memory_id}): {e}");
    }

    if let Some(emb) = embedder {
        let text = format!("{} {}", result.summary, result.tags);
        match emb.embed(&[text]).await {
            Ok(mut vecs) => {
                if let Some(vec) = vecs.drain(..).next() {
                    if let Err(e) = store.upsert(db, memory_id, emb.id(), emb.dim(), &vec).await {
                        tracing::warn!("inline embed upsert failed (memory {memory_id}): {e}");
                    }
                }
            }
            Err(e) => tracing::warn!("inline embed failed (memory {memory_id}): {e}"),
        }
    }

    // Entity inverted index: link every proper noun named in the session to the
    // narrative so name-anchored questions resolve by direct lookup even when the
    // memory ranks low on keyword/vector. Best-effort.
    for entity in &result.entities {
        if let Err(e) = db::insert_memory_entity(db, memory_id, entity).await {
            tracing::warn!("entity index failed (memory {memory_id}): {e}");
        }
    }

    // Temporal graph lite: persist structured subject→relation→object edges
    // with the narrative memory as provenance. This is additive to FTS/vector
    // recall and gives Operator OS a queryable relationship layer.
    for relation in &result.relations {
        let edge = db::NewMemoryEdge {
            project: project.to_string(),
            memory_id,
            source: relation.source.clone(),
            relation: relation.relation.clone(),
            target: relation.target.clone(),
            valid_from: relation
                .valid_from
                .clone()
                .or_else(|| result.event_time.clone()),
            valid_until: relation.valid_until.clone(),
            confidence: relation.confidence,
        };
        match db::insert_memory_edge(db, &edge).await {
            Ok(_) => {
                let _ = db::insert_memory_entity(db, memory_id, &relation.source).await;
                let _ = db::insert_memory_entity(db, memory_id, &relation.target).await;
            }
            Err(e) => tracing::warn!("memory edge store failed (memory {memory_id}): {e}"),
        }
    }

    // Procedural memory: durable "how work should be done" rules are stored as
    // first-class typed memories so agents can retrieve operating instructions
    // without mixing them into ordinary narrative/fact recall.
    for (idx, procedure) in result.procedures.iter().enumerate() {
        persist_procedure(
            db,
            embedder,
            store,
            project,
            session_id,
            memory_id,
            result.importance,
            procedure,
            &result.entities,
            idx + 1,
        )
        .await;
    }

    // Dual-output compression: persist each extracted atomic fact as its own
    // searchable kind=fact memory in the same project/session. This bakes the
    // benchmark's separate "explicit fact" extraction into the write path so
    // specifics (dates, names, quantities) survive compression and resolve on
    // direct lookup. Best-effort per fact — a single failure is logged, never
    // fatal (local-first posture, matching the inline-embed handling above).
    for (idx, fact) in result.facts.iter().enumerate() {
        persist_fact(
            db,
            embedder,
            store,
            project,
            session_id,
            memory_id,
            result.importance,
            fact,
            &result.entities,
            result.event_time.as_deref(),
            idx + 1,
        )
        .await;
    }

    tracing::info!(
        "Session {session_id} compressed → memory_id={memory_id} (+{} facts, +{} procedures)",
        result.facts.len(),
        result.procedures.len()
    );
    Ok(memory_id)
}

#[allow(clippy::too_many_arguments)]
async fn persist_procedure(
    db: &Database,
    embedder: Option<&dyn Embedder>,
    store: &dyn VectorStore,
    project: &str,
    session_id: &str,
    parent_id: i64,
    importance: u8,
    procedure: &str,
    entities: &[String],
    ordinal: usize,
) {
    let tags = format!("procedural session:{session_id}");
    let pid = match db::insert_memory(db, project, session_id, procedure, Some(&tags)).await {
        Ok(pid) => pid,
        Err(e) => {
            tracing::warn!("procedural store failed (parent memory {parent_id}): {e}");
            return;
        }
    };
    if let Err(e) = db::upsert_memory_meta(db, pid, (importance as f64 / 10.0).max(0.75)).await {
        tracing::warn!("procedural meta failed (memory {pid}): {e}");
    }
    if let Err(e) = db::set_memory_scope_kind(db, pid, "project", "procedural").await {
        tracing::warn!("procedural kind tag failed (memory {pid}): {e}");
    }
    let mut governance = MemoryGovernance::derived_from(parent_id);
    governance.source_ref = Some(format!("mem:{parent_id}:procedure:{ordinal}"));
    if let Err(e) = db::apply_memory_governance(
        db,
        pid,
        "project",
        "procedural",
        &governance,
        Some("ironmem:derive"),
        "derive",
    )
    .await
    {
        tracing::warn!("procedural governance failed (memory {pid}): {e}");
    }
    let proc_lower = procedure.to_lowercase();
    for entity in entities {
        if proc_lower.contains(&entity.to_lowercase()) {
            if let Err(e) = db::insert_memory_entity(db, pid, entity).await {
                tracing::warn!("procedural entity index failed (memory {pid}): {e}");
            }
        }
    }
    if let Some(emb) = embedder {
        match emb.embed(&[procedure.to_string()]).await {
            Ok(mut vecs) => {
                if let Some(vec) = vecs.drain(..).next() {
                    if let Err(e) = store.upsert(db, pid, emb.id(), emb.dim(), &vec).await {
                        tracing::warn!("procedural embed upsert failed (memory {pid}): {e}");
                    }
                }
            }
            Err(e) => tracing::warn!("procedural embed failed (memory {pid}): {e}"),
        }
    }
}

/// Persist one extracted fact as a `kind=fact`, project-scoped memory tied to the
/// originating session, inheriting the parent's importance and (best-effort)
/// carrying its own embedding. Errors are logged against `parent_id` and
/// swallowed so one bad fact never fails an otherwise-successful compression.
#[allow(clippy::too_many_arguments)] // each arg is an independent field of the fact memory
async fn persist_fact(
    db: &Database,
    embedder: Option<&dyn Embedder>,
    store: &dyn VectorStore,
    project: &str,
    session_id: &str,
    parent_id: i64,
    importance: u8,
    fact: &str,
    entities: &[String],
    event_time: Option<&str>,
    ordinal: usize,
) {
    // Date-stamp the fact so its date rides INSIDE the retrievable text and is
    // visible to the answerer — the single biggest lever for temporal questions
    // (mirrors the high-scoring "{fact} (as of {date})" pattern). Undated
    // sessions store the fact verbatim.
    let stored = match event_time {
        Some(when) => format!("{fact} (as of {when})"),
        None => fact.to_string(),
    };
    let tags = format!("fact session:{session_id}");
    let fid = match db::insert_memory(db, project, session_id, &stored, Some(&tags)).await {
        Ok(fid) => fid,
        Err(e) => {
            tracing::warn!("fact store failed (parent memory {parent_id}): {e}");
            return;
        }
    };
    if let Err(e) = db::upsert_memory_meta(db, fid, importance as f64 / 10.0).await {
        tracing::warn!("fact meta failed (fact memory {fid}): {e}");
    }
    if let Err(e) = db::set_memory_scope_kind(db, fid, "project", "fact").await {
        tracing::warn!("fact kind tag failed (fact memory {fid}): {e}");
    }
    let mut governance = MemoryGovernance::derived_from(parent_id);
    governance.source_ref = Some(format!("mem:{parent_id}:fact:{ordinal}"));
    if let Err(e) = db::apply_memory_governance(
        db,
        fid,
        "project",
        "fact",
        &governance,
        Some("ironmem:derive"),
        "derive",
    )
    .await
    {
        tracing::warn!("fact governance failed (memory {fid}): {e}");
    }
    // Also tag the fact with the session's event_time so the time-aware boost can
    // surface it directly (not just via the dated text).
    if let Some(when) = event_time {
        if let Err(e) = db::set_memory_event_time(db, fid, when).await {
            tracing::warn!("fact event_time failed (fact memory {fid}): {e}");
        }
    }
    // Index the fact under any session entity it actually mentions, so the fact
    // (which usually carries the answer) is directly reachable by that name.
    let fact_lower = fact.to_lowercase();
    for entity in entities {
        if fact_lower.contains(&entity.to_lowercase()) {
            if let Err(e) = db::insert_memory_entity(db, fid, entity).await {
                tracing::warn!("fact entity index failed (fact memory {fid}): {e}");
            }
        }
    }
    if let Some(emb) = embedder {
        match emb.embed(std::slice::from_ref(&stored)).await {
            Ok(mut vecs) => {
                if let Some(vec) = vecs.drain(..).next() {
                    if let Err(e) = store.upsert(db, fid, emb.id(), emb.dim(), &vec).await {
                        tracing::warn!("fact embed upsert failed (fact memory {fid}): {e}");
                    }
                }
            }
            Err(e) => tracing::warn!("fact embed failed (fact memory {fid}): {e}"),
        }
    }
}

/// Store an explicit, user-curated memory (the Supermemory "add memory" pattern):
/// insert the memory + meta, tag it with scope/kind (both clamped to the known
/// sets), and best-effort embed it for semantic recall. Unlike compression there
/// is no session to summarize — `text` is stored verbatim as the memory.
/// `scope="user"` makes it a cross-project fact; `kind` classifies it. Returns
/// the new memory id.
#[allow(clippy::too_many_arguments)] // each arg is an independent field of the memory
pub async fn remember(
    db: &Database,
    embedder: Option<&dyn Embedder>,
    store: &dyn VectorStore,
    project: &str,
    scope: &str,
    kind: &str,
    text: &str,
    tags: Option<&str>,
) -> Result<i64> {
    remember_with_governance(
        db,
        embedder,
        store,
        project,
        scope,
        kind,
        text,
        tags,
        MemoryGovernance::explicit(),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn remember_with_governance(
    db: &Database,
    embedder: Option<&dyn Embedder>,
    store: &dyn VectorStore,
    project: &str,
    scope: &str,
    kind: &str,
    text: &str,
    tags: Option<&str>,
    governance: MemoryGovernance,
) -> Result<i64> {
    governance.validate()?;
    // Explicit memories aren't tied to a compressed session; mark the origin so
    // they're distinguishable in session-history joins (no FK to sessions).
    let memory_id = db::insert_memory(db, project, "remember", text, tags).await?;
    // Deliberately curated → slightly above the neutral default importance.
    db::upsert_memory_meta(db, memory_id, 0.7).await?;
    db::set_memory_scope_kind(db, memory_id, scope, kind).await?;
    db::apply_memory_governance(
        db,
        memory_id,
        scope,
        kind,
        &governance,
        governance.writer_identity.as_deref(),
        "remember",
    )
    .await?;
    let explicit = CompressionResult {
        summary: text.to_string(),
        tags: tags.unwrap_or_default().to_string(),
        importance: 7,
        kind: db::clamp_kind(kind).to_string(),
        facts: if db::clamp_kind(kind) == "fact" {
            vec![text.to_string()]
        } else {
            Vec::new()
        },
        procedures: if db::clamp_kind(kind) == "procedural" {
            vec![text.to_string()]
        } else {
            Vec::new()
        },
        ..Default::default()
    };
    if let Err(e) =
        persist_memory_skim(db, memory_id, project, "remember", &explicit, &[], None).await
    {
        tracing::warn!("remember skim store failed (memory {memory_id}): {e}");
    }

    if let Some(emb) = embedder {
        let embed_text = match tags {
            Some(t) if !t.is_empty() => format!("{text} {t}"),
            _ => text.to_string(),
        };
        match emb.embed(&[embed_text]).await {
            Ok(mut vecs) => {
                if let Some(vec) = vecs.drain(..).next() {
                    if let Err(e) = store.upsert(db, memory_id, emb.id(), emb.dim(), &vec).await {
                        tracing::warn!("remember embed upsert failed (memory {memory_id}): {e}");
                    }
                }
            }
            Err(e) => tracing::warn!("remember embed failed (memory {memory_id}): {e}"),
        }
    }

    tracing::info!(
        "Remembered {}/{} memory → memory_id={memory_id} project={project}",
        db::clamp_scope(scope),
        db::clamp_kind(kind),
    );

    // Best-effort: keep the user profile fresh as cross-project memories grow.
    // Uses the deterministic local rollup (cfg=None → no network), so it never
    // blocks remember and never makes a surprise API call.
    if db::clamp_scope(scope) == "user" {
        let n = db::count_user_memories(db).await.unwrap_or(0);
        let no_profile = matches!(db::get_profile_memory(db).await, Ok(None));
        if no_profile || (n > 0 && n % crate::profile::PROFILE_REFRESH_EVERY == 0) {
            if let Err(e) = crate::profile::regenerate(db, embedder, store, None).await {
                tracing::warn!("profile auto-refresh failed: {e}");
            }
        }
    }

    Ok(memory_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::create_session;
    use crate::embedder::FakeEmbedder;
    use crate::vectorstore::SqliteVecStore;

    #[tokio::test]
    async fn persist_writes_memory_meta_and_embedding() {
        let path = std::env::temp_dir().join(format!("ironmem-cmp-{}.db", uuid::Uuid::new_v4()));
        let db = Database::new(&path.to_string_lossy()).await.unwrap();
        db.migrate().await.unwrap();
        db.ensure_ann(8).await.unwrap();
        let session = create_session(&db, "/tmp/p").await.unwrap();

        let emb = FakeEmbedder::new(8);
        let store = SqliteVecStore;
        let result = CompressionResult {
            summary: "implemented retrieval".into(),
            tags: "rust retrieval rrf".into(),
            importance: 8,
            kind: "architecture".into(),
            ..Default::default()
        };

        let id = persist(&db, Some(&emb), &store, "/tmp/p", &session, result)
            .await
            .unwrap();

        // Memory row exists.
        assert!(db::get_memory_by_id(&db, id).await.unwrap().is_some());
        // Importance persisted as 0.8 (8/10), and the classified kind landed on
        // the meta row at the default project scope.
        let info = db::get_memory_meta_full(&db, id).await.unwrap();
        assert!((info.importance - 0.8).abs() < 1e-9);
        assert_eq!(
            (info.scope.as_str(), info.kind.as_str()),
            ("project", "architecture")
        );
        // Embedding persisted under the embedder's model id.
        assert!(db::get_embedding(&db, "memory", id, emb.id())
            .await
            .unwrap()
            .is_some());
        let chunks = db::chunks_for_memories(&db, &[id]).await.unwrap();
        let memory_chunks = chunks.get(&id).expect("skim chunks written");
        assert!(memory_chunks
            .iter()
            .any(|c| c.chunk_id == format!("mem:{id}:overview")
                && c.density == "high"
                && c.kind == "architecture"));

        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn persist_stores_facts_as_searchable_memories() {
        let path = std::env::temp_dir().join(format!("ironmem-facts-{}.db", uuid::Uuid::new_v4()));
        let db = Database::new(&path.to_string_lossy()).await.unwrap();
        db.migrate().await.unwrap();
        db.ensure_ann(8).await.unwrap();
        let session = create_session(&db, "/tmp/p").await.unwrap();

        let emb = FakeEmbedder::new(8);
        let store = SqliteVecStore;
        let result = CompressionResult {
            summary: "Caroline attended community events".into(),
            tags: "caroline community".into(),
            importance: 6,
            kind: "session".into(),
            facts: vec![
                "Caroline joined the LGBTQ support group on 7 May 2023".into(),
                "Melanie painted a sunrise in 2022".into(),
            ],
            ..Default::default()
        };

        let id = persist(&db, Some(&emb), &store, "/tmp/p", &session, result)
            .await
            .unwrap();

        // Narrative memory exists.
        assert!(db::get_memory_by_id(&db, id).await.unwrap().is_some());

        // The date-bearing fact is retrievable by its date and tagged kind=fact,
        // as a memory distinct from the narrative.
        let hits = db::search_memories(&db, "/tmp/p", "7 May 2023", 10)
            .await
            .unwrap();
        let fact_hit = hits
            .iter()
            .find(|m| m.summary.contains("7 May 2023"))
            .expect("date fact must be retrievable by its date");
        assert_ne!(
            fact_hit.id, id,
            "fact is a memory distinct from the narrative"
        );
        let meta = db::get_memory_meta_full(&db, fact_hit.id).await.unwrap();
        assert_eq!(meta.kind, "fact", "fact memory must be tagged kind=fact");
        assert_eq!(meta.parent_memory_id, Some(id));
        let expected_source_ref = format!("mem:{id}:fact:1");
        assert_eq!(
            meta.source_ref.as_deref(),
            Some(expected_source_ref.as_str())
        );

        // The fact also carries an embedding (semantic recall path).
        assert!(db::get_embedding(&db, "memory", fact_hit.id, emb.id())
            .await
            .unwrap()
            .is_some());

        // The second fact is its own memory too.
        let melanie = db::search_memories(&db, "/tmp/p", "Melanie sunrise", 10)
            .await
            .unwrap();
        assert!(melanie.iter().any(|m| m.summary.contains("Melanie")));

        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn persist_date_stamps_facts_when_session_dated() {
        let path = std::env::temp_dir().join(format!("ironmem-df-{}.db", uuid::Uuid::new_v4()));
        let db = Database::new(&path.to_string_lossy()).await.unwrap();
        db.migrate().await.unwrap();
        let session = create_session(&db, "/tmp/p").await.unwrap();
        let store = crate::vectorstore::BruteForceStore;
        let result = CompressionResult {
            summary: "Caroline talked about adoption".into(),
            tags: "caroline".into(),
            importance: 6,
            kind: "session".into(),
            facts: vec!["Caroline researched adoption agencies".into()],
            event_time: Some("2023-05-08".into()),
            ..Default::default()
        };

        persist(&db, None, &store, "/tmp/p", &session, result)
            .await
            .unwrap();

        // The fact's stored text carries the date, and the fact memory is tagged
        // with event_time (both retrieval paths for temporal questions).
        let hits = db::search_memories(&db, "/tmp/p", "adoption 2023", 10)
            .await
            .unwrap();
        let fact = hits
            .iter()
            .find(|m| m.summary.contains("researched adoption agencies"))
            .expect("dated fact retrievable");
        assert!(
            fact.summary.contains("2023-05-08"),
            "fact text must carry the date"
        );
        let meta = db::get_memory_meta_full(&db, fact.id).await.unwrap();
        assert_eq!(meta.event_time.as_deref(), Some("2023-05-08"));
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn persist_stores_procedures_as_typed_memories() {
        let path = std::env::temp_dir().join(format!("ironmem-proc-{}.db", uuid::Uuid::new_v4()));
        let db = Database::new(&path.to_string_lossy()).await.unwrap();
        db.migrate().await.unwrap();
        let session = create_session(&db, "/tmp/p").await.unwrap();
        let store = crate::vectorstore::BruteForceStore;
        let result = CompressionResult {
            summary: "Operator OS memory planning".into(),
            tags: "operator-os memory".into(),
            importance: 7,
            kind: "session".into(),
            entities: vec!["Operator OS".into()],
            procedures: vec![
                "For Operator OS, keep tenant isolation explicit before shared memory.".into(),
            ],
            ..Default::default()
        };

        let parent = persist(&db, None, &store, "/tmp/p", &session, result)
            .await
            .unwrap();
        let hits = db::search_memories(&db, "/tmp/p", "tenant isolation", 10)
            .await
            .unwrap();
        let proc_mem = hits
            .iter()
            .find(|m| m.summary.contains("tenant isolation"))
            .expect("procedural memory should be searchable");
        assert_ne!(proc_mem.id, parent);
        let meta = db::get_memory_meta_full(&db, proc_mem.id).await.unwrap();
        assert_eq!(meta.kind, "procedural");
        assert_eq!(meta.parent_memory_id, Some(parent));
        let expected_source_ref = format!("mem:{parent}:procedure:1");
        assert_eq!(
            meta.source_ref.as_deref(),
            Some(expected_source_ref.as_str())
        );
        assert!(meta.importance >= 0.75);

        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn skim_fact_and_procedure_chunks_keep_source_spans() {
        let path = std::env::temp_dir().join(format!("ironmem-span-{}.db", uuid::Uuid::new_v4()));
        let db = Database::new(&path.to_string_lossy()).await.unwrap();
        db.migrate().await.unwrap();
        let session = create_session(&db, "/tmp/p").await.unwrap();
        let store = crate::vectorstore::BruteForceStore;
        let result = CompressionResult {
            summary: "Source-grounded memory upgrade".into(),
            tags: "source spans".into(),
            importance: 8,
            kind: "session".into(),
            facts: vec!["Caroline joined the LGBTQ support group on 7 May 2023".into()],
            procedures: vec!["For IronMem, preserve source spans for extracted facts.".into()],
            ..Default::default()
        };
        let observations = vec![db::Observation {
            id: 42,
            session_id: session.clone(),
            project: "/tmp/p".into(),
            tool: "note".into(),
            input: Some("Caroline joined the LGBTQ support group on 7 May 2023.".into()),
            output: Some("For IronMem, preserve source spans for extracted facts.".into()),
            created_at: 1,
        }];

        let memory_id = persist(&db, None, &store, "/tmp/p", &session, result.clone())
            .await
            .unwrap();
        let hash = store_session_transcript(&db, memory_id, &observations)
            .await
            .unwrap()
            .expect("transcript blob hash");
        persist_memory_skim(
            &db,
            memory_id,
            "/tmp/p",
            &session,
            &result,
            &observations,
            Some(&hash),
        )
        .await
        .unwrap();

        let chunks = db::chunks_for_memories(&db, &[memory_id]).await.unwrap();
        let chunks = chunks.get(&memory_id).unwrap();
        for kind in ["fact", "procedural"] {
            let chunk = chunks
                .iter()
                .find(|chunk| chunk.kind == kind)
                .expect("source-linked derived chunk");
            assert_eq!(chunk.source_hash.as_deref(), Some(hash.as_str()));
            assert!(chunk.source_start.is_some());
            assert!(chunk.source_end > chunk.source_start);
        }

        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn persist_stores_structured_relations_as_graph_edges() {
        let path = std::env::temp_dir().join(format!("ironmem-rel-{}.db", uuid::Uuid::new_v4()));
        let db = Database::new(&path.to_string_lossy()).await.unwrap();
        db.migrate().await.unwrap();
        let session = create_session(&db, "/tmp/p").await.unwrap();
        let store = crate::vectorstore::BruteForceStore;
        let result = CompressionResult {
            summary: "Caroline moved work forward".into(),
            tags: "caroline acme".into(),
            importance: 7,
            kind: "session".into(),
            event_time: Some("2026-06-05".into()),
            relations: vec![provider::MemoryRelation {
                source: "Caroline".into(),
                relation: "status".into(),
                target: "approved".into(),
                valid_from: None,
                valid_until: None,
                confidence: 0.88,
            }],
            ..Default::default()
        };

        let memory_id = persist(&db, None, &store, "/tmp/p", &session, result)
            .await
            .unwrap();

        let edges = db::memory_edges_for_entity(&db, Some("/tmp/p"), "Caroline", false, 10)
            .await
            .unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].memory_id, memory_id);
        assert_eq!(edges[0].source, "Caroline");
        assert_eq!(edges[0].relation, "status");
        assert_eq!(edges[0].target, "approved");
        assert_eq!(edges[0].valid_from.as_deref(), Some("2026-06-05"));
        assert!((edges[0].confidence - 0.88).abs() < 1e-9);

        // Relation endpoints are also indexed as entities on the narrative row.
        assert_eq!(
            db::memories_for_entity(&db, Some("/tmp/p"), "approved", 10)
                .await
                .unwrap(),
            vec![memory_id]
        );

        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn persist_without_embedder_still_writes_meta() {
        let path = std::env::temp_dir().join(format!("ironmem-cmp2-{}.db", uuid::Uuid::new_v4()));
        let db = Database::new(&path.to_string_lossy()).await.unwrap();
        db.migrate().await.unwrap();
        let session = create_session(&db, "/tmp/p").await.unwrap();
        let store = crate::vectorstore::BruteForceStore;
        let result = CompressionResult {
            summary: "no embedder path".into(),
            tags: "fts only".into(),
            importance: 3,
            kind: "session".into(),
            ..Default::default()
        };

        let id = persist(&db, None, &store, "/tmp/p", &session, result)
            .await
            .unwrap();
        assert!(db::get_memory_by_id(&db, id).await.unwrap().is_some());
        assert!((db::get_memory_meta(&db, id).await.unwrap() - 0.3).abs() < 1e-9);
        // No embedding written.
        assert!(db::get_embedding(&db, "memory", id, "fake")
            .await
            .unwrap()
            .is_none());
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn remember_writes_typed_memory_and_embeds() {
        let path = std::env::temp_dir().join(format!("ironmem-rem-{}.db", uuid::Uuid::new_v4()));
        let db = Database::new(&path.to_string_lossy()).await.unwrap();
        db.migrate().await.unwrap();
        db.ensure_ann(8).await.unwrap();
        let emb = FakeEmbedder::new(8);
        let store = SqliteVecStore;

        // A user-scope preference stored from project A.
        let id = remember(
            &db,
            Some(&emb),
            &store,
            "/tmp/projA",
            "user",
            "preference",
            "prefers tabs over spaces",
            Some("style editor"),
        )
        .await
        .unwrap();

        // scope/kind landed; importance bumped above the neutral default.
        let info = db::get_memory_meta_full(&db, id).await.unwrap();
        assert_eq!(
            (info.scope.as_str(), info.kind.as_str()),
            ("user", "preference")
        );
        assert!((info.importance - 0.7).abs() < 1e-9);
        // Embedding written under the embedder's model id.
        assert!(db::get_embedding(&db, "memory", id, emb.id())
            .await
            .unwrap()
            .is_some());
        // Retrievable via the global user scope, irrespective of which project
        // it was created in (the cross-project guarantee).
        let users = db::get_recent_memories_scoped(&db, "user", None, 10)
            .await
            .unwrap();
        assert!(
            users.iter().any(|m| m.id == id),
            "user memory must be globally visible"
        );
        // It must NOT appear under another project's project-scope view.
        let proj_b = db::get_recent_memories_scoped(&db, "project", Some("/tmp/projB"), 10)
            .await
            .unwrap();
        assert!(!proj_b.iter().any(|m| m.id == id));

        // Unknown scope/kind clamp to the safe defaults; no embedder is fine.
        let id2 = remember(&db, None, &store, "/tmp/projB", "bogus", "bogus", "x", None)
            .await
            .unwrap();
        let info2 = db::get_memory_meta_full(&db, id2).await.unwrap();
        assert_eq!(
            (info2.scope.as_str(), info2.kind.as_str()),
            ("project", "session")
        );

        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn store_session_transcript_round_trips() {
        let path = std::env::temp_dir().join(format!("ironmem-cmp3-{}.db", uuid::Uuid::new_v4()));
        let db = Database::new(&path.to_string_lossy()).await.unwrap();
        db.migrate().await.unwrap();
        let session = create_session(&db, "/tmp/p").await.unwrap();
        let store = crate::vectorstore::BruteForceStore;

        db::insert_observation(
            &db,
            &session,
            "/tmp/p",
            "Read",
            Some("src/main.rs"),
            Some("fn main(){}"),
            2048,
        )
        .await
        .unwrap();
        db::insert_observation(
            &db,
            &session,
            "/tmp/p",
            "Bash",
            Some("cargo test"),
            Some("ok"),
            2048,
        )
        .await
        .unwrap();
        let observations = db::get_observations_for_session(&db, &session)
            .await
            .unwrap();

        let result = CompressionResult {
            summary: "s".into(),
            tags: "t".into(),
            importance: 5,
            kind: "session".into(),
            ..Default::default()
        };
        let memory_id = persist(&db, None, &store, "/tmp/p", &session, result)
            .await
            .unwrap();

        let hash = store_session_transcript(&db, memory_id, &observations)
            .await
            .unwrap()
            .expect("transcript stored");

        // Linked on the memory and retrievable byte-exact.
        assert_eq!(
            db::get_memory_session_blob(&db, memory_id).await.unwrap(),
            Some(hash.clone())
        );
        let restored = crate::ccr::load_blob(&db, &hash).await.unwrap();
        let expected = build_transcript(&observations);
        assert_eq!(String::from_utf8(restored).unwrap(), expected);
        assert!(expected.contains("## Read"));

        // No observations → nothing stored.
        assert!(store_session_transcript(&db, memory_id, &[])
            .await
            .unwrap()
            .is_none());

        let _ = std::fs::remove_file(path);
    }
}
