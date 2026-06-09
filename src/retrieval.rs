//! Hybrid retrieval: Reciprocal Rank Fusion over FTS + vector results, plus
//! the blended relevance/recency/importance ranking used for session-start
//! injection. Pure scoring helpers are unit-tested; the search/rank entry
//! points compose them with the db + vector store.

use anyhow::Result;
use chrono::Utc;
use std::collections::{HashMap, HashSet};

use crate::config::{Config, Weights};
use crate::context;
use crate::db::{self, Database, Memory};
use crate::embedder::Embedder;
use crate::vectorstore::VectorStore;

/// Standard RRF damping constant. Larger ⇒ rank position matters less.
pub const RRF_K: i64 = 60;

/// Whether to fuse the entity-index signal into hybrid retrieval. Disabled: in
/// person-centric corpora the index matches nearly every memory and returns them
/// most-recent-first, which demotes older-but-exact facts below newer vaguer ones
/// and measurably hurt LoCoMo precision. FTS + vector already match the named
/// person; the index is retained for future relevance-ranked entity retrieval.
const FUSE_ENTITY_SIGNAL: bool = false;

/// Fuse several ranked id-lists into one ordering by Σ 1/(k + rank).
/// `rank` is 0-indexed (top of a list contributes the most). Ties keep
/// first-appearance order so the result is deterministic.
pub fn rrf_fuse(lists: &[Vec<i64>], k: i64) -> Vec<i64> {
    let mut score: HashMap<i64, f64> = HashMap::new();
    let mut order: Vec<i64> = Vec::new();
    for list in lists {
        for (rank, &id) in list.iter().enumerate() {
            let entry = score.entry(id).or_insert_with(|| {
                order.push(id);
                0.0
            });
            *entry += 1.0 / (k as f64 + rank as f64);
        }
    }
    // Stable sort by score desc; equal scores keep first-appearance order.
    order.sort_by(|a, b| {
        score[b]
            .partial_cmp(&score[a])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    order
}

/// 4-digit years (1900–2099) named in a query. A temporal question ("what did X
/// do in May 2023?") implies a year, which anchors the time-aware retrieval
/// boost: memories whose `event_time` contains a named year get a rank lift.
fn query_years(query: &str) -> Vec<String> {
    query
        .split(|c: char| !c.is_ascii_digit())
        .filter(|t| t.len() == 4)
        .filter(|t| matches!(t.parse::<u16>(), Ok(y) if (1900..=2099).contains(&y)))
        .map(|s| s.to_string())
        .collect()
}

/// Capitalized word tokens (≥3 chars) in a query — candidate proper nouns for
/// the entity-index lookup. No stoplist is needed: a non-entity capitalized word
/// ("What") simply resolves to no memories, since the index only matches names
/// actually stored at write time.
fn query_entities(query: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().count() >= 3)
        .filter(|t| t.chars().next().is_some_and(|c| c.is_uppercase()))
        .filter(|t| seen.insert(t.to_lowercase()))
        .map(|s| s.to_string())
        .collect()
}

/// Hybrid search: fuse keyword (FTS), semantic (vector), temporal (event_time),
/// and entity (proper-noun index) signals via RRF. With none of the auxiliary
/// signals present this returns the exact FTS ordering, reproducing legacy
/// behavior.
pub async fn hybrid_search(
    db: &Database,
    embedder: Option<&dyn Embedder>,
    store: &dyn VectorStore,
    project: Option<&str>,
    query: &str,
    limit: usize,
) -> Result<Vec<Memory>> {
    // Candidate pool: pull more than `limit` per signal so the narrative-reserve
    // quota below has narratives to choose from even when facts dominate ranking.
    let pool = (limit * 3).max(30);

    // Keyword side (always run).
    let fts = match project {
        Some(p) => db::search_memories(db, p, query, pool as i64).await?,
        None => db::search_all_memories(db, query, pool as i64).await?,
    };

    // Semantic side (best-effort; only when an embedder is configured).
    let vec_ids: Vec<i64> = if let Some(emb) = embedder {
        match embed_one(emb, query).await {
            Some(qvec) => store
                .knn(db, project, &qvec, emb.id(), pool)
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|(id, _sim)| id)
                .collect(),
            None => Vec::new(),
        }
    } else {
        Vec::new()
    };

    // Temporal side: when the query names a year, memories tagged with a matching
    // event_time get a rank boost. Additive — undated memories never match, so
    // this only ever lifts dated memories, never suppresses anything.
    let time_ids: Vec<i64> = {
        let years = query_years(query);
        if years.is_empty() {
            Vec::new()
        } else {
            let mut seen = HashSet::new();
            let mut ids = Vec::new();
            for y in &years {
                for id in db::memories_by_event_time(db, project, y, pool)
                    .await
                    .unwrap_or_default()
                {
                    if seen.insert(id) {
                        ids.push(id);
                    }
                }
            }
            ids
        }
    };

    // Entity signal (see FUSE_ENTITY_SIGNAL): off by default — its recency-ordered
    // matches demote older-but-exact facts in person-centric data. FTS + vector
    // already cover the named person.
    let entity_ids: Vec<i64> = if FUSE_ENTITY_SIGNAL {
        let ents = query_entities(query);
        let mut seen = HashSet::new();
        let mut ids = Vec::new();
        for e in &ents {
            for id in db::memories_for_entity(db, project, e, pool)
                .await
                .unwrap_or_default()
            {
                if seen.insert(id) {
                    ids.push(id);
                }
            }
        }
        ids
    } else {
        Vec::new()
    };

    // Candidate ordering: RRF over keyword + any auxiliary signals (semantic,
    // temporal, entity). With no auxiliary signal this is the FTS order.
    let fts_ids: Vec<i64> = fts.iter().map(|m| m.id).collect();
    let by_id: HashMap<i64, Memory> = fts.into_iter().map(|m| (m.id, m)).collect();

    let mut aux: Vec<Vec<i64>> = Vec::new();
    if !vec_ids.is_empty() {
        aux.push(vec_ids);
    }
    if !time_ids.is_empty() {
        aux.push(time_ids);
    }
    if !entity_ids.is_empty() {
        aux.push(entity_ids);
    }
    let candidates: Vec<i64> = if aux.is_empty() {
        fts_ids
    } else {
        let mut lists: Vec<Vec<i64>> = Vec::with_capacity(aux.len() + 1);
        lists.push(fts_ids);
        lists.append(&mut aux);
        rrf_fuse(&lists, RRF_K)
    };

    // Narrative-reserve quota, then materialize in rank order (reusing FTS rows).
    let chosen = reserve_narrative_slots(db, &candidates, limit).await?;
    let mut out = Vec::with_capacity(chosen.len());
    for id in chosen {
        if let Some(m) = by_id.get(&id) {
            out.push(m.clone());
        } else if let Some(m) = db::get_memory_by_id(db, id).await? {
            out.push(m);
        }
    }
    Ok(out)
}

/// Apply the narrative-reserve quota over a ranked candidate id list, returning at
/// most `limit` ids. Atomic facts (`kind="fact"`) dominate ranking for specific
/// queries and would otherwise crowd the few narrative memories that carry
/// cross-turn (multi-hop) links out of the top-`limit`. Guarantee up to ~40% of
/// the slots to narratives (in rank order) before filling the rest by rank, so
/// facts AUGMENT rather than REPLACE narratives. Final order follows rank.
async fn reserve_narrative_slots(db: &Database, candidates: &[i64], limit: usize) -> Result<Vec<i64>> {
    if candidates.len() <= limit {
        return Ok(candidates.to_vec());
    }
    let kinds = db::kinds_for_memories(db, candidates).await?;
    // A missing kind (legacy/no-meta row) counts as a narrative, never a fact.
    let is_fact = |id: &i64| kinds.get(id).map(|k| k == "fact").unwrap_or(false);

    let narr_slots = ((limit * 2) / 5).max(1); // ~40% reserved for narratives
    let mut chosen: Vec<i64> = Vec::with_capacity(limit);
    let mut taken: HashSet<i64> = HashSet::new();

    // First guarantee the narrative quota (in rank order)…
    for &id in candidates {
        if chosen.len() >= narr_slots {
            break;
        }
        if !is_fact(&id) {
            chosen.push(id);
            taken.insert(id);
        }
    }
    // …then fill the remaining slots by rank (facts + any extra narratives).
    for &id in candidates {
        if chosen.len() >= limit {
            break;
        }
        if taken.insert(id) {
            chosen.push(id);
        }
    }
    // Restore rank order for a coherent final ordering.
    let rank: HashMap<i64, usize> = candidates.iter().enumerate().map(|(i, &id)| (id, i)).collect();
    chosen.sort_by_key(|id| rank[id]);
    Ok(chosen)
}

// ── LLM reranking ───────────────────────────────────────────────────

/// Per-candidate character cap in the rerank prompt — keeps a wide pool within a
/// small token budget. Short fact memories pass through whole; long narratives
/// are trimmed to their leading content (enough to judge relevance).
const RERANK_SNIPPET_CHARS: usize = 400;

/// Build the rerank prompt: the question, then the numbered candidate snippets,
/// then an instruction to return the useful candidate numbers most-useful-first.
fn build_rerank_prompt(query: &str, candidates: &[Memory]) -> String {
    let mut s = String::with_capacity(256 + candidates.len() * RERANK_SNIPPET_CHARS);
    s.push_str(
        "You are selecting which memory snippets best help answer a question.\n\
         Read the QUESTION, then the numbered CANDIDATES.\n\n",
    );
    s.push_str("QUESTION: ");
    s.push_str(query);
    s.push_str("\n\nCANDIDATES:\n");
    for (i, m) in candidates.iter().enumerate() {
        let snippet: String = m
            .summary
            .chars()
            .take(RERANK_SNIPPET_CHARS)
            .collect::<String>()
            .replace('\n', " ");
        s.push_str(&format!("{}. {}\n", i + 1, snippet));
    }
    s.push_str(
        "\nReturn the candidate numbers ordered from MOST to LEAST useful for \
         answering the question, as a comma-separated list (e.g. \"4,1,9\"). Rank a \
         snippet that contains the SPECIFIC answer the question asks for — the exact \
         date, name, number, or event — above one that is merely on the same topic. \
         Include only genuinely relevant numbers; omit the rest. Output ONLY the list.",
    );
    s
}

/// Parse a rerank reply into 0-based candidate positions. Pulls integer runs in
/// order, maps 1-based → 0-based, and drops out-of-range and duplicate indices.
/// Tolerant of stray prose/years: only valid candidate numbers survive.
fn parse_rerank_order(text: &str, n: usize) -> Vec<usize> {
    let mut seen = HashSet::new();
    let mut order = Vec::new();
    for tok in text.split(|c: char| !c.is_ascii_digit()) {
        if let Ok(k) = tok.parse::<usize>() {
            if (1..=n).contains(&k) && seen.insert(k) {
                order.push(k - 1);
            }
        }
    }
    order
}

/// Fuse the base retrieval order with the LLM's preference order via RRF, then
/// take the top `limit`. Fusing (rather than letting the LLM order replace the
/// base) is the safety property: a candidate the base ranked high keeps its base
/// RRF contribution even when the model omits or deprioritizes it, so reranking
/// can PROMOTE a buried answer the model favors but can never push a strong
/// base hit out of the top `limit`. An empty `order` (parse/LLM failure)
/// collapses to the base order, truncated — never worse than no rerank.
fn fuse_rerank(base: &[Memory], order: &[usize], limit: usize) -> Vec<Memory> {
    if base.is_empty() {
        return Vec::new();
    }
    let base_ids: Vec<i64> = base.iter().map(|m| m.id).collect();
    let llm_ids: Vec<i64> = order.iter().filter_map(|&i| base.get(i).map(|m| m.id)).collect();
    let fused = if llm_ids.is_empty() {
        base_ids
    } else {
        rrf_fuse(&[base_ids, llm_ids], RRF_K)
    };
    let by_id: HashMap<i64, &Memory> = base.iter().map(|m| (m.id, m)).collect();
    fused
        .into_iter()
        .take(limit)
        .filter_map(|id| by_id.get(&id).map(|m| (*m).clone()))
        .collect()
}

/// Rerank retrieved `candidates` against `query` with a fast model, returning the
/// best `limit` in ranked order. On any provider/parse failure it falls back to
/// the base retrieval order (truncated), so reranking can only improve precision,
/// never reduce recall below what `hybrid_search` already produced.
pub async fn llm_rerank(
    config: &Config,
    query: &str,
    candidates: Vec<Memory>,
    limit: usize,
) -> Vec<Memory> {
    if candidates.len() <= 1 || limit == 0 {
        return candidates.into_iter().take(limit).collect();
    }
    // Empty rerank model ⇒ use the compression model (always available, capable).
    let model = if config.rerank.model.is_empty() {
        config.model.as_str()
    } else {
        config.rerank.model.as_str()
    };
    let prompt = build_rerank_prompt(query, &candidates);
    match crate::provider::complete_with(&prompt, model, config).await {
        Ok(reply) => {
            let order = parse_rerank_order(&reply, candidates.len());
            fuse_rerank(&candidates, &order, limit)
        }
        Err(e) => {
            tracing::warn!("llm rerank failed ({e}); using base retrieval order");
            candidates.into_iter().take(limit).collect()
        }
    }
}

/// Merge the narrow (`limit`-sized) retrieval with the wider pool: narrow items
/// first — preserving their stronger ordering, which keeps an FTS-dominant answer
/// (e.g. a specific dated fact) from being demoted by the pool-widening artifact
/// where a larger candidate fusion sinks single-signal-strong items — then the
/// wide-pool newcomers the reranker may promote. Deduped by id.
fn reanchor(narrow: Vec<Memory>, wide: Vec<Memory>) -> Vec<Memory> {
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(wide.len().max(narrow.len()));
    for m in narrow.into_iter().chain(wide) {
        if seen.insert(m.id) {
            out.push(m);
        }
    }
    out
}

/// Reranked retrieval. Retrieves the well-ordered `base@limit` set AND a wider
/// pool, re-anchors them (narrow order on top, wide-pool newcomers appended), and
/// LLM-reranks. The re-anchoring is what protects FTS-dominant temporal answers:
/// they keep their strong narrow-order floor, so reranking can only PROMOTE a
/// buried wide-pool answer (precision/recall for reasoning questions), never
/// demote a dated fact the base retrieval already had in the top `limit`.
#[allow(clippy::too_many_arguments)]
pub async fn rerank_search(
    db: &Database,
    embedder: Option<&dyn Embedder>,
    store: &dyn VectorStore,
    config: &Config,
    project: Option<&str>,
    query: &str,
    limit: usize,
) -> Result<Vec<Memory>> {
    let pool = limit.saturating_mul(2).max(config.rerank.pool);
    let narrow = hybrid_search(db, embedder, store, project, query, limit).await?;
    let wide = hybrid_search(db, embedder, store, project, query, pool).await?;
    let candidates = reanchor(narrow, wide);
    Ok(llm_rerank(config, query, candidates, limit).await)
}

// ── Blended injection ranking ───────────────────────────────────────

/// Recency weight via true half-life decay: 1.0 at age 0, 0.5 at one
/// half-life, approaching 0 for very old memories.
pub fn recency_weight(age_secs: f64, half_life_days: f64) -> f64 {
    if half_life_days <= 0.0 {
        return 0.0;
    }
    0.5_f64.powf(age_secs / (half_life_days * 86_400.0))
}

/// Linear blend of relevance, recency, and importance (each in 0..1).
pub fn blended_score(rel: f64, rec: f64, imp: f64, w: &Weights) -> f64 {
    w.relevance * rel + w.recency * rec + w.importance * imp
}

/// Rank recent memories for session-start injection by blended score.
/// With no `query_vec`/embedder the relevance term is 0 for every candidate,
/// so the ordering collapses to recency + importance (legacy-compatible).
// The knobs (db/embedder/store/project/query/weights/half-life/limit) are all
// independent inputs; bundling them into a struct would obscure more than it
// clarifies for a single internal ranking function.
#[allow(clippy::too_many_arguments)]
pub async fn injection_rank(
    db: &Database,
    embedder: Option<&dyn Embedder>,
    store: &dyn VectorStore,
    project: &str,
    query_vec: Option<&[f32]>,
    weights: &Weights,
    half_life_days: f64,
    limit: usize,
) -> Result<Vec<Memory>> {
    // Pull a generous recent window, then re-rank it by the blend. Candidates =
    // this project's memories ∪ the user's global (cross-project) memories, so a
    // user-scope preference surfaces even in a brand-new project. The two scopes
    // are disjoint by construction; dedup defensively all the same.
    let window = ((limit as i64) * 10).max(50);
    let mut candidates = db::get_recent_memories_scoped(db, "project", Some(project), window).await?;
    let mut seen: HashSet<i64> = candidates.iter().map(|m| m.id).collect();
    for m in db::get_recent_memories_scoped(db, "user", None, window).await? {
        if seen.insert(m.id) {
            candidates.push(m);
        }
    }
    if candidates.is_empty() {
        return Ok(candidates);
    }

    // Relevance only when we have both an embedder (for its model id) and a
    // query vector. Map memory id → cosine similarity (0..1).
    let relevance: HashMap<i64, f64> = match (embedder, query_vec) {
        (Some(emb), Some(qv)) => store
            .knn(db, Some(project), qv, emb.id(), window as usize)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|(id, sim)| (id, sim as f64))
            .collect(),
        _ => HashMap::new(),
    };

    let now = Utc::now().timestamp();
    let mut scored: Vec<(f64, Memory)> = Vec::with_capacity(candidates.len());
    for m in candidates {
        let rel = relevance.get(&m.id).copied().unwrap_or(0.0);
        let rec = recency_weight((now - m.created_at).max(0) as f64, half_life_days);
        // Importance + kind in one query; kind applies a typed prior on top of
        // the relevance/recency/importance blend.
        let info = db::get_memory_meta_full(db, m.id).await?;
        let base = blended_score(rel, rec, info.importance, weights);
        scored.push((base * weights.kind_multiplier(&info.kind), m));
    }
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    Ok(scored.into_iter().take(limit).map(|(_, m)| m).collect())
}

/// High-level session-start ranking: derive a query signal from the project's
/// git state, embed it (when an embedder is available), and rank recent
/// memories by blended score. With no embedder/git signal this collapses to
/// recency + importance — the legacy injection order.
pub async fn rank_for_injection(
    db: &Database,
    embedder: Option<&dyn Embedder>,
    store: &dyn VectorStore,
    project: &str,
    weights: &Weights,
    half_life_days: f64,
    limit: usize,
) -> Result<Vec<Memory>> {
    let query_vec: Option<Vec<f32>> = match (embedder, context::git_query(project)) {
        (Some(emb), Some(signal)) => embed_one(emb, &signal).await,
        _ => None,
    };
    let mut ranked = injection_rank(
        db,
        embedder,
        store,
        project,
        query_vec.as_deref(),
        weights,
        half_life_days,
        limit,
    )
    .await?;

    // The user profile is always injected first (a single high-signal row),
    // ahead of the blended ranking — it's the durable "who is this user" context.
    if let Some(profile) = db::get_profile_memory(db).await? {
        ranked.retain(|m| m.id != profile.id);
        ranked.insert(0, profile);
        if limit > 0 {
            ranked.truncate(limit);
        }
    }
    Ok(ranked)
}

/// Embed a single string, returning the vector or `None` on failure/empty.
async fn embed_one(embedder: &dyn Embedder, text: &str) -> Option<Vec<f32>> {
    embedder
        .embed(&[text.to_string()])
        .await
        .ok()
        .and_then(|mut v| v.drain(..).next())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{create_session, insert_memory};
    use crate::embedder::FakeEmbedder;
    use crate::embedding_codec::normalize;
    use crate::vectorstore::SqliteVecStore;

    #[test]
    fn rrf_fuses_by_reciprocal_rank() {
        let fts = vec![1_i64, 2, 3];
        let vec = vec![3_i64, 1, 4];
        let fused = rrf_fuse(&[fts, vec], 60);
        assert_eq!(fused[0], 1); // appears high in both
        assert!(fused.contains(&4));
    }

    #[test]
    fn rrf_single_list_preserves_order() {
        let fused = rrf_fuse(&[vec![5_i64, 6, 7]], 60);
        assert_eq!(fused, vec![5, 6, 7]);
    }

    #[test]
    fn query_years_extracts_valid_years_only() {
        assert_eq!(query_years("what did Caroline do in May 2023?"), vec!["2023"]);
        assert_eq!(query_years("between 2021 and 2099"), vec!["2021", "2099"]);
        assert!(query_years("no years, just 42 and 12345").is_empty());
        assert!(query_years("1899 is out of range").is_empty());
    }

    #[test]
    fn query_entities_extracts_capitalized_tokens() {
        assert_eq!(
            query_entities("What did Caroline tell Melanie?"),
            vec!["What", "Caroline", "Melanie"]
        );
        // Lowercased duplicates collapse; sub-3-char tokens drop.
        assert_eq!(query_entities("Al and Bo met Caroline and Caroline"), vec!["Caroline"]);
    }

    #[tokio::test]
    async fn reserve_narrative_slots_keeps_narrative_against_fact_flood() {
        let (db, path) = seeded_db().await; // narratives 1,2,3 (irrelevant here)
        let s = create_session(&db, "/tmp/p").await.unwrap();
        // Six fact memories ranked ABOVE a single narrative (narrative ranked last
        // in the candidate order) — without a quota it is dropped at limit=3.
        let mut facts = Vec::new();
        for i in 0..6 {
            let f = insert_memory(&db, "/tmp/p", &s, &format!("fact {i}"), Some("fact"))
                .await
                .unwrap();
            db::set_memory_scope_kind(&db, f, "project", "fact").await.unwrap();
            facts.push(f);
        }
        let narr = insert_memory(&db, "/tmp/p", &s, "the connecting narrative", Some("x"))
            .await
            .unwrap();
        db::set_memory_scope_kind(&db, narr, "project", "session").await.unwrap();

        let mut candidates = facts.clone();
        candidates.push(narr); // narrative is the lowest-ranked candidate

        let chosen = reserve_narrative_slots(&db, &candidates, 3).await.unwrap();
        assert_eq!(chosen.len(), 3, "respects limit");
        assert!(
            chosen.contains(&narr),
            "narrative must be reserved despite ranking last: {chosen:?}"
        );
        // Quota is ~40% of 3 ⇒ 1 narrative slot; the other 2 are top facts by rank.
        assert!(chosen.contains(&facts[0]) && chosen.contains(&facts[1]));
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn hybrid_search_time_boost_surfaces_dated_memory() {
        let (db, path) = seeded_db().await; // ids 1,2,3 under /tmp/p, all undated
        // A 4th memory whose wording shares no keyword with the query and which
        // has no embedding, but is tagged with an event_time in 2023.
        let s = create_session(&db, "/tmp/p").await.unwrap();
        let dated = insert_memory(&db, "/tmp/p", &s, "zzz unrelated wording", Some("x"))
            .await
            .unwrap();
        db::set_memory_event_time(&db, dated, "2023-05-07").await.unwrap();

        // No embedder ⇒ vector side empty; query keyword misses every summary in
        // FTS, but names the year 2023 ⇒ the temporal signal surfaces the memory.
        let res = hybrid_search(
            &db,
            None,
            &crate::vectorstore::BruteForceStore,
            Some("/tmp/p"),
            "what happened in 2023",
            5,
        )
        .await
        .unwrap();
        assert!(
            res.iter().any(|m| m.id == dated),
            "time-boost must surface the dated memory FTS+vector miss: {res:?}"
        );
        let _ = std::fs::remove_file(path);
    }

    fn mk(id: i64, summary: &str) -> Memory {
        Memory {
            id,
            project: "/p".into(),
            session_id: "s".into(),
            summary: summary.into(),
            tags: None,
            created_at: 0,
        }
    }

    #[test]
    fn parse_rerank_order_maps_filters_and_dedupes() {
        // 1-based → 0-based, order preserved.
        assert_eq!(parse_rerank_order("3,1,2", 3), vec![2, 0, 1]);
        // Out-of-range (99) and duplicate (second 1) dropped.
        assert_eq!(parse_rerank_order("4, 1, 1, 99", 4), vec![3, 0]);
        // No valid numbers ⇒ empty (caller falls back to base order).
        assert!(parse_rerank_order("none are relevant", 3).is_empty());
    }

    #[test]
    fn parse_rerank_order_tolerates_prose_and_years() {
        // A stray year (2023) is out of range and dropped; real picks survive.
        assert_eq!(
            parse_rerank_order("Candidates 2 and 5 (from 2023) help most.", 6),
            vec![1, 4]
        );
    }

    #[test]
    fn fuse_rerank_promotes_llm_favored_buried_candidate() {
        let cands = vec![mk(10, "a"), mk(11, "b"), mk(12, "c"), mk(13, "d"), mk(14, "e")];
        // The model's single favorite is the LAST base candidate (id 14, base rank
        // 4) — a buried answer. Fusion promotes it to #1…
        let out = fuse_rerank(&cands, &[4], 5);
        assert_eq!(
            out.first().map(|m| m.id),
            Some(14),
            "buried answer the model favors is promoted: {:?}",
            out.iter().map(|m| m.id).collect::<Vec<_>>()
        );
        // …while the strong base hit (id 10) stays right behind it, never dropped.
        assert_eq!(out.get(1).map(|m| m.id), Some(10));
    }

    #[test]
    fn fuse_rerank_keeps_strong_base_hit_the_model_omits() {
        // The safety property: id 10 is the top base hit (the answer). The model
        // omits it entirely and only names a lower candidate. Fusion must STILL
        // keep id 10 in the truncated result — reranking can't drop a strong base
        // hit. (This is the regression the limit=20 sanity check exposed.)
        let cands = vec![mk(10, "the answer"), mk(11, "b"), mk(12, "c"), mk(13, "d")];
        let out = fuse_rerank(&cands, &[3], 2); // model only likes candidate 4 (id 13)
        let ids: Vec<i64> = out.iter().map(|m| m.id).collect();
        assert!(ids.contains(&13), "promoted candidate is present: {ids:?}");
        assert!(ids.contains(&10), "strong base hit is NOT dropped: {ids:?}");
    }

    #[test]
    fn reanchor_keeps_narrow_order_then_appends_newcomers() {
        let narrow = vec![mk(10, "a"), mk(11, "b"), mk(12, "c")];
        // Wide pool: a different (degraded) order, same ids plus newcomers 9 and 8.
        let wide = vec![mk(12, "c"), mk(9, "x"), mk(10, "a"), mk(8, "y"), mk(11, "b")];
        let out = reanchor(narrow, wide);
        // Narrow order preserved on top; only the wide-pool newcomers (9, 8) trail.
        assert_eq!(out.iter().map(|m| m.id).collect::<Vec<_>>(), vec![10, 11, 12, 9, 8]);
    }

    #[test]
    fn fuse_rerank_empty_order_is_base_order_truncated() {
        let cands = vec![mk(10, "a"), mk(11, "b"), mk(12, "c")];
        // Parse/LLM failure ⇒ exact base order, truncated — never worse than off.
        let out = fuse_rerank(&cands, &[], 2);
        assert_eq!(out.iter().map(|m| m.id).collect::<Vec<_>>(), vec![10, 11]);
    }

    #[test]
    fn build_rerank_prompt_numbers_and_caps_snippets() {
        let long = "x".repeat(900);
        let cands = vec![mk(1, "short fact"), mk(2, &long)];
        let p = build_rerank_prompt("when did it happen?", &cands);
        assert!(p.contains("QUESTION: when did it happen?"));
        assert!(p.contains("1. short fact"));
        // The 900-char candidate is capped — its raw text never appears in full.
        assert!(!p.contains(&"x".repeat(RERANK_SNIPPET_CHARS + 1)));
    }

    #[test]
    fn recency_weight_halves_at_half_life() {
        assert!((recency_weight(0.0, 30.0) - 1.0).abs() < 1e-9);
        let one_hl = 30.0 * 86_400.0;
        assert!((recency_weight(one_hl, 30.0) - 0.5).abs() < 1e-9);
        assert!(recency_weight(one_hl * 20.0, 30.0) < 0.001); // →0 for old
    }

    #[test]
    fn blended_score_is_linear_combo() {
        let w = Weights {
            relevance: 0.5,
            recency: 0.3,
            importance: 0.2,
            ..Weights::default()
        };
        let s = blended_score(1.0, 1.0, 1.0, &w);
        assert!((s - 1.0).abs() < 1e-9);
        let s2 = blended_score(1.0, 0.0, 0.0, &w);
        assert!((s2 - 0.5).abs() < 1e-9);
    }

    async fn seeded_db() -> (Database, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!("ironmem-ret-{}.db", uuid::Uuid::new_v4()));
        let db = Database::new(&path.to_string_lossy()).await.unwrap();
        db.migrate().await.unwrap();
        db.ensure_ann(8).await.unwrap();
        let s = create_session(&db, "/tmp/p").await.unwrap();
        // rowids 1,2,3
        insert_memory(&db, "/tmp/p", &s, "alpha auth login", Some("auth"))
            .await
            .unwrap();
        insert_memory(&db, "/tmp/p", &s, "beta search index", Some("search"))
            .await
            .unwrap();
        insert_memory(&db, "/tmp/p", &s, "gamma database schema", Some("db"))
            .await
            .unwrap();
        (db, path)
    }

    async fn embed_all(db: &Database, emb: &FakeEmbedder, store: &SqliteVecStore) {
        for (id, text) in [
            (1_i64, "alpha auth login"),
            (2, "beta search index"),
            (3, "gamma database schema"),
        ] {
            let v = emb.embed(&[text.to_string()]).await.unwrap();
            store.upsert(db, id, emb.id(), emb.dim(), &v[0]).await.unwrap();
        }
    }

    #[tokio::test]
    async fn hybrid_search_surfaces_semantic_hit_fts_misses() {
        let (db, path) = seeded_db().await;
        let emb = FakeEmbedder::new(8);
        let store = SqliteVecStore;
        embed_all(&db, &emb, &store).await;

        // Query whose embedding matches memory 3, but whose keyword has no FTS
        // overlap with any summary — pure FTS returns nothing.
        let qvec = emb.embed(&["gamma database schema".to_string()]).await.unwrap();
        let knn = store.knn(&db, Some("/tmp/p"), &qvec[0], emb.id(), 3).await.unwrap();
        assert_eq!(knn[0].0, 3);

        let res = hybrid_search(&db, Some(&emb), &store, Some("/tmp/p"), "zzznomatch", 5)
            .await
            .unwrap();
        assert!(res.iter().any(|m| m.id == 3), "semantic hit should appear");
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn hybrid_search_without_embedder_is_pure_fts() {
        let (db, path) = seeded_db().await;
        let store = SqliteVecStore;
        let res = hybrid_search(&db, None, &store, Some("/tmp/p"), "alpha", 5)
            .await
            .unwrap();
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].id, 1);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn injection_rank_prefers_semantic_match() {
        let (db, path) = seeded_db().await;
        let emb = FakeEmbedder::new(8);
        let store = SqliteVecStore;
        embed_all(&db, &emb, &store).await;
        let weights = Weights::default();

        let qvec = normalize(&emb.embed(&["gamma database schema".to_string()]).await.unwrap()[0]);
        let ranked = injection_rank(
            &db,
            Some(&emb),
            &store,
            "/tmp/p",
            Some(&qvec),
            &weights,
            30.0,
            3,
        )
        .await
        .unwrap();
        assert_eq!(ranked[0].id, 3, "semantically nearest ranks first");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn kind_multiplier_boosts_durable_kinds_and_honors_overrides() {
        let w = Weights::default();
        assert!((w.kind_multiplier("session") - 1.0).abs() < 1e-9);
        assert!((w.kind_multiplier("unknown-kind") - 1.0).abs() < 1e-9);
        assert!(w.kind_multiplier("preference") > 1.0);
        assert!(w.kind_multiplier("error_solution") > 1.0);
        assert!(w.kind_multiplier("profile") >= w.kind_multiplier("preference"));

        // A configured override wins over the built-in prior.
        let mut w2 = Weights::default();
        w2.kind_boosts.insert("session".to_string(), 3.0);
        assert!((w2.kind_multiplier("session") - 3.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn injection_includes_cross_project_user_memory() {
        let (db, path) = seeded_db().await; // 3 project memories under /tmp/p
        // A user-scope preference created from a DIFFERENT project.
        let s2 = create_session(&db, "/tmp/other").await.unwrap();
        let uid = insert_memory(&db, "/tmp/other", &s2, "user prefers tabs over spaces", Some("pref"))
            .await
            .unwrap();
        db::set_memory_scope_kind(&db, uid, "user", "preference").await.unwrap();

        let store = crate::vectorstore::BruteForceStore;
        let weights = Weights::default();

        // Inject into a FRESH project with zero project-scoped memories.
        let ranked = rank_for_injection(&db, None, &store, "/tmp/fresh", &weights, 30.0, 5)
            .await
            .unwrap();
        assert!(
            ranked.iter().any(|m| m.id == uid),
            "user-scope memory must inject into a fresh project: {ranked:?}"
        );
        // Project isolation: /tmp/p's project-scoped memories must NOT leak in.
        assert!(
            !ranked.iter().any(|m| [1, 2, 3].contains(&m.id)),
            "another project's memories must not inject: {ranked:?}"
        );
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn profile_is_always_injected_first() {
        let (db, path) = seeded_db().await; // /tmp/p with 3 project memories
        let s = create_session(&db, "/tmp/u").await.unwrap();
        let pid = insert_memory(
            &db,
            "ironmem:profile",
            &s,
            "# User Profile\n- prefers rust",
            Some("profile user"),
        )
        .await
        .unwrap();
        db::set_memory_scope_kind(&db, pid, "user", "profile").await.unwrap();

        let store = crate::vectorstore::BruteForceStore;
        let weights = Weights::default();
        let ranked = rank_for_injection(&db, None, &store, "/tmp/p", &weights, 30.0, 3)
            .await
            .unwrap();
        assert_eq!(ranked[0].id, pid, "profile must inject first: {ranked:?}");
        assert!(ranked.len() <= 3, "limit is respected even with the profile");
        // Exactly one copy (deduped, not duplicated by the user-scope candidate).
        assert_eq!(ranked.iter().filter(|m| m.id == pid).count(), 1);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn kind_boost_lifts_preference_over_newer_session() {
        let path = std::env::temp_dir().join(format!("ironmem-kb-{}.db", uuid::Uuid::new_v4()));
        let db = Database::new(&path.to_string_lossy()).await.unwrap();
        db.migrate().await.unwrap();
        let s = create_session(&db, "/tmp/p").await.unwrap();

        // Preference inserted FIRST (older), session SECOND (newer). Equal
        // importance. Without the kind prior the newer session would win on
        // recency; the preference boost must flip it.
        let pref = insert_memory(&db, "/tmp/p", &s, "a durable preference", Some("x"))
            .await
            .unwrap();
        db::set_memory_scope_kind(&db, pref, "project", "preference").await.unwrap();
        let sess = insert_memory(&db, "/tmp/p", &s, "a plain session note", Some("x"))
            .await
            .unwrap();
        db::set_memory_scope_kind(&db, sess, "project", "session").await.unwrap();

        let store = crate::vectorstore::BruteForceStore;
        let weights = Weights::default();
        let ranked = injection_rank(&db, None, &store, "/tmp/p", None, &weights, 30.0, 2)
            .await
            .unwrap();
        assert_eq!(ranked[0].id, pref, "kind-boosted preference must rank first: {ranked:?}");
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn injection_rank_without_query_is_recency_order() {
        let (db, path) = seeded_db().await;
        let store = crate::vectorstore::BruteForceStore;
        let weights = Weights::default();
        let ranked = injection_rank(&db, None, &store, "/tmp/p", None, &weights, 30.0, 3)
            .await
            .unwrap();
        // All created ~now with equal importance ⇒ stable recency (DESC) order.
        assert_eq!(ranked.len(), 3);
        let _ = std::fs::remove_file(path);
    }
}
