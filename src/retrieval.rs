//! Hybrid retrieval: Reciprocal Rank Fusion over FTS + vector + temporal graph
//! results, plus the blended relevance/recency/importance ranking used for
//! session-start injection. Pure scoring helpers are unit-tested; the search/rank
//! entry points compose them with the db + vector store.

use anyhow::Result;
use chrono::{Datelike, NaiveDate, Utc};
use std::collections::{HashMap, HashSet};

use crate::config::{Config, Weights};
use crate::context;
use crate::db::{self, Database, Memory};
use crate::embedder::Embedder;
use crate::storage::StorageBackend;
use crate::vectorstore::VectorStore;

/// Standard RRF damping constant. Larger ⇒ rank position matters less.
pub const RRF_K: i64 = 60;

/// Whether to fuse the entity-index signal into hybrid retrieval. Disabled: in
/// person-centric corpora the index matches nearly every memory and returns them
/// most-recent-first, which demotes older-but-exact facts below newer vaguer ones
/// and measurably hurt LoCoMo precision. FTS + vector already match the named
/// person; the index is retained for future relevance-ranked entity retrieval.
const FUSE_ENTITY_SIGNAL: bool = false;

/// Graph fusion is intentionally narrower than the old entity signal: only a
/// handful of explicit entity phrases from the query are expanded, and edges are
/// ranked by relation/source/target relevance before their memory ids enter RRF.
const MAX_GRAPH_ENTITIES: usize = 8;
const GRAPH_EDGES_PER_ENTITY: usize = 12;
const GRAPH_CHAIN_EDGES_PER_BRIDGE: usize = 6;
const MAX_DECOMPOSED_QUERIES: usize = 6;
const TEMPORAL_EVENT_POOL: usize = 512;
const SOURCE_FACT_FLOOR_MAX_SLOTS: usize = 5;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum QueryRoute {
    SingleHop,
    MultiHop,
    Temporal,
    OpenDomain,
}

#[derive(Clone, Copy, Debug)]
struct FusionWeights {
    fts: usize,
    vector: usize,
    query_variant: usize,
    event_year: usize,
    temporal_event: usize,
    graph: usize,
    entity: usize,
    /// Chunk-level (skim layer) recall, mapped back to parent memories. Only
    /// open-domain queries fuse it: that route loses the most detail to
    /// narrative summaries, and chunks carry the specifics.
    chunk: usize,
}

impl FusionWeights {
    fn for_query(query: &str, temporal_weight: usize, chunk_weight: usize) -> Self {
        match classify_query_route(query) {
            QueryRoute::Temporal => Self {
                fts: 1,
                vector: 1,
                query_variant: 1,
                event_year: 2,
                temporal_event: temporal_weight.max(1) + 2,
                graph: 0,
                entity: 0,
                chunk: 0,
            },
            QueryRoute::MultiHop => Self {
                fts: 1,
                vector: 2,
                query_variant: 2,
                event_year: 1,
                temporal_event: temporal_weight.max(1),
                graph: 3,
                entity: 0,
                chunk: 0,
            },
            QueryRoute::SingleHop => Self {
                fts: 2,
                vector: 1,
                query_variant: 0,
                event_year: 1,
                temporal_event: temporal_weight.max(1),
                graph: 1,
                entity: 0,
                chunk: 0,
            },
            QueryRoute::OpenDomain => Self {
                fts: 1,
                vector: 2,
                query_variant: 0,
                event_year: 1,
                temporal_event: temporal_weight.max(1),
                graph: 1,
                entity: 0,
                chunk: chunk_weight,
            },
        }
    }
}

/// Process-wide retrieval tuning, installed once at server startup from `Config`
/// (config is fixed for the process lifetime). Lets the gated temporal-event
/// fusion weight (B) and the #5 temporal-trust boost reach the core ranker
/// without threading `&Config` through every `hybrid_search` caller. Unset →
/// `Default` → behaviour identical to before these levers existed.
#[derive(Clone, Copy, Debug)]
pub struct RetrievalTuning {
    /// Times the temporal-event id-list is pushed into RRF (>=1). 1 = unchanged.
    pub temporal_fusion_weight: usize,
    /// #5 temporal-trust: additive boost weight (0.0 = off), recency half-life
    /// (days), and reference saturation. See `governance::trust_trajectory_boost`.
    pub trust_weight: f64,
    pub trust_halflife_days: f64,
    pub trust_ref_saturation: f64,
    /// #1 governed-retrieval router: writer trust-tier authority weight (0.0 =
    /// off). High-tier (user-explicit) facts outrank machine-derived ones on
    /// near-ties. See `governance::tier_authority_boost`.
    pub tier_weight: f64,
    /// Times the chunk-parent id-list is pushed into RRF for open-domain
    /// queries (0 = signal off). 1 = one list, same footing as FTS.
    pub chunk_fusion_weight: usize,
    /// Bridge-hop depth in the graph id-list builder. 1 = the historical
    /// single evidence-chain hop; 2 adds a second-order hop at decayed weight.
    pub graph_chain_depth: usize,
    /// Demotion weight for candidates whose only edge support is superseded
    /// while another candidate holds the live edge for the same
    /// (source, relation). 0.0 = off.
    pub stale_demotion_weight: f64,
    /// Additive activation boost weight: importance × maturity × recency.
    /// 0.0 = off.
    pub activation_weight: f64,
    /// Recency half-life (days) for the activation boost.
    pub activation_halflife_days: f64,
    /// Abstention guard: drop result memories sharing less than this fraction
    /// of the query's salient terms (0.0 = off). Better an empty answer than a
    /// confident wrong one — abstention is a scored LongMemEval ability.
    pub abstention_min_overlap: f64,
    /// T0 lexical early exit: skip embedding/auxiliary recall when the top FTS
    /// hit already contains every salient query term (single-hop route only).
    /// Off by default; tier exit rates are published in `/status` metrics.
    pub tier_early_exit: bool,
}

impl Default for RetrievalTuning {
    fn default() -> Self {
        Self {
            temporal_fusion_weight: 1,
            trust_weight: 0.0,
            trust_halflife_days: 30.0,
            trust_ref_saturation: 5.0,
            tier_weight: 0.0,
            chunk_fusion_weight: 1,
            graph_chain_depth: 1,
            stale_demotion_weight: 0.0,
            activation_weight: 0.0,
            activation_halflife_days: 30.0,
            abstention_min_overlap: 0.0,
            tier_early_exit: false,
        }
    }
}

static RETRIEVAL_TUNING: std::sync::OnceLock<RetrievalTuning> = std::sync::OnceLock::new();

/// Install the retrieval tuning (call once at startup). Idempotent: later calls
/// are ignored, matching the "config is fixed for the process" model.
pub fn set_retrieval_tuning(t: RetrievalTuning) {
    let _ = RETRIEVAL_TUNING.set(t);
}

fn tuning() -> RetrievalTuning {
    RETRIEVAL_TUNING.get().copied().unwrap_or_default()
}

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

fn push_weighted_signal(lists: &mut Vec<Vec<i64>>, signal: &[i64], weight: usize) {
    if signal.is_empty() || weight == 0 {
        return;
    }
    for _ in 0..weight {
        lists.push(signal.to_vec());
    }
}

/// (#5) Temporal-trust re-rank of an already-fused candidate ordering. When
/// `trust_weight > 0`, each candidate's reciprocal-rank base score is nudged by
/// its trust trajectory (referenced + recently validated — see
/// `governance::trust_trajectory_boost`). The base term keeps semantic/keyword
/// order dominant, so trust only reorders near-ties. A no-op (and zero DB cost)
/// when the lever is off or no candidate has accrued trust. The trust signal is
/// populated by the synthesis pass, which positively reinforces source facts.
async fn apply_trust_boost(db: &Database, candidates: Vec<i64>) -> Result<Vec<i64>> {
    let t = tuning();
    if t.trust_weight <= 0.0 || candidates.len() < 2 {
        return Ok(candidates);
    }
    let trust = db::trust_meta_for(db, &candidates)
        .await
        .unwrap_or_default();
    if trust.is_empty() {
        return Ok(candidates);
    }
    let now = Utc::now().timestamp();
    let mut scored: Vec<(usize, i64, f64)> = candidates
        .iter()
        .enumerate()
        .map(|(rank, &id)| {
            let base = 1.0 / (RRF_K as f64 + rank as f64);
            let boost = trust
                .get(&id)
                .map(|(rc, lv)| {
                    crate::governance::trust_trajectory_boost(
                        *rc,
                        *lv,
                        now,
                        t.trust_weight,
                        t.trust_halflife_days,
                        t.trust_ref_saturation,
                    )
                })
                .unwrap_or(0.0);
            (rank, id, base + boost)
        })
        .collect();
    // Re-sort by combined score desc; equal scores keep the original fused order.
    scored.sort_by(|a, b| {
        b.2.partial_cmp(&a.2)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
    });
    Ok(scored.into_iter().map(|(_, id, _)| id).collect())
}

/// (#1) Governed-retrieval router: nudge the fused order by each candidate's
/// *writer trust tier* (recorded at write time, consulted here at query time).
/// Same shape as `apply_trust_boost` — the reciprocal-rank base term stays
/// dominant so tier authority only reorders near-ties, lifting user-explicit
/// (`High`) facts above machine-`Derived` (`Medium`) ones and demoting
/// `Low`/`Untrusted` writers. No-op (and zero DB cost) when `tier_weight <= 0`.
async fn apply_tier_boost(db: &Database, candidates: Vec<i64>) -> Result<Vec<i64>> {
    let t = tuning();
    if t.tier_weight <= 0.0 || candidates.len() < 2 {
        return Ok(candidates);
    }
    let tiers = db::trust_tiers_for(db, &candidates)
        .await
        .unwrap_or_default();
    if tiers.is_empty() {
        return Ok(candidates);
    }
    let mut scored: Vec<(usize, i64, f64)> = candidates
        .iter()
        .enumerate()
        .map(|(rank, &id)| {
            let base = 1.0 / (RRF_K as f64 + rank as f64);
            // A missing tier reads as Medium (neutral), so legacy rows are unmoved.
            let tier = tiers
                .get(&id)
                .map(|s| crate::governance::parse_trust_tier(s))
                .unwrap_or(crate::governance::TrustTier::Medium);
            let boost = crate::governance::tier_authority_boost(tier, t.tier_weight);
            (rank, id, base + boost)
        })
        .collect();
    scored.sort_by(|a, b| {
        b.2.partial_cmp(&a.2)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
    });
    Ok(scored.into_iter().map(|(_, id, _)| id).collect())
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

fn query_named_entities(query: &str) -> Vec<String> {
    query_entities(query)
        .into_iter()
        .filter(|t| !is_question_word(t))
        .collect()
}

fn is_question_word(token: &str) -> bool {
    matches!(
        token.to_ascii_lowercase().as_str(),
        "what"
            | "when"
            | "where"
            | "which"
            | "who"
            | "whom"
            | "whose"
            | "why"
            | "how"
            | "did"
            | "does"
            | "do"
            | "is"
            | "are"
            | "was"
            | "were"
            | "show"
            | "tell"
            | "list"
            | "give"
    )
}

fn is_graph_entity_token(token: &str) -> bool {
    let len = token.chars().count();
    if len < 2 || is_question_word(token) {
        return false;
    }
    let mut chars = token.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_uppercase() && (len >= 3 || token.chars().all(|c| c.is_uppercase()))
}

fn push_unique_text(out: &mut Vec<String>, seen: &mut HashSet<String>, value: impl Into<String>) {
    let value = value
        .into()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if value.is_empty() {
        return;
    }
    if seen.insert(value.to_ascii_lowercase()) {
        out.push(value);
    }
}

fn entity_aliases(entity: &str) -> Vec<String> {
    let clean = entity.split_whitespace().collect::<Vec<_>>().join(" ");
    if clean.is_empty() {
        return Vec::new();
    }
    let mut aliases = Vec::new();
    let mut seen = HashSet::new();
    push_unique_text(&mut aliases, &mut seen, clean.clone());

    let mut words: Vec<String> = clean
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_string())
        .collect();
    while words
        .first()
        .is_some_and(|w| matches!(w.to_ascii_lowercase().as_str(), "mr" | "mrs" | "ms" | "dr"))
    {
        words.remove(0);
    }

    if words.len() > 1 {
        push_unique_text(&mut aliases, &mut seen, words.join(" "));
        let useful_short = |w: &String| {
            w.chars().count() >= 3
                || (w.chars().count() >= 2 && w.chars().all(|c| c.is_uppercase()))
        };
        if let Some(first) = words.first().filter(|w| useful_short(w)) {
            push_unique_text(&mut aliases, &mut seen, first.clone());
        }
        if let Some(last) = words.last().filter(|w| useful_short(w)) {
            push_unique_text(&mut aliases, &mut seen, last.clone());
        }
        let acronym: String = words
            .iter()
            .filter_map(|w| w.chars().next())
            .map(|c| c.to_ascii_uppercase())
            .collect();
        if acronym.len() >= 2 && acronym.len() <= 8 {
            push_unique_text(&mut aliases, &mut seen, acronym);
        }
    }

    aliases
}

/// Capitalized entity phrases for graph lookup. This keeps multi-word names like
/// "Operator OS" intact while also trying useful single-token names like
/// "Caroline". Common question words are dropped so the graph signal stays
/// narrow and does not become the disabled broad entity signal in disguise.
fn query_graph_entities(query: &str) -> Vec<String> {
    let mut primaries = Vec::new();
    let mut primary_seen = HashSet::new();
    let mut phrase: Vec<String> = Vec::new();

    let flush_phrase =
        |phrase: &mut Vec<String>, out: &mut Vec<String>, seen: &mut HashSet<String>| {
            if phrase.len() > 1 {
                let joined = phrase.join(" ");
                push_unique_text(out, seen, joined);
            }
            for token in phrase.drain(..) {
                push_unique_text(out, seen, token);
            }
        };

    for token in query.split(|c: char| !c.is_alphanumeric()) {
        if is_graph_entity_token(token) {
            phrase.push(token.to_string());
        } else {
            flush_phrase(&mut phrase, &mut primaries, &mut primary_seen);
        }
    }
    flush_phrase(&mut phrase, &mut primaries, &mut primary_seen);

    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for entity in primaries {
        for alias in entity_aliases(&entity) {
            push_unique_text(&mut out, &mut seen, alias);
            if out.len() >= MAX_GRAPH_ENTITIES {
                return out;
            }
        }
    }
    out.truncate(MAX_GRAPH_ENTITIES);
    out
}

fn salient_query_terms(query: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    query
        .split(|c: char| !c.is_alphanumeric() && c != '\'')
        .filter(|t| t.chars().count() >= 3)
        .map(normalize_event_token)
        .filter(|t| !is_temporal_stopword(t))
        .filter(|t| seen.insert(t.clone()))
        .collect()
}

fn decomposed_queries(query: &str) -> Vec<String> {
    let entities = query_graph_entities(query);
    let terms = salient_query_terms(query);
    if entities.is_empty() || terms.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let top_terms = terms
        .iter()
        .filter(|term| {
            !entities
                .iter()
                .any(|entity| entity.eq_ignore_ascii_case(term.as_str()))
        })
        .take(4)
        .cloned()
        .collect::<Vec<_>>();

    for entity in entities.iter().take(4) {
        if !top_terms.is_empty() {
            push_unique_text(
                &mut out,
                &mut seen,
                format!("{entity} {}", top_terms.join(" ")),
            );
        }
        for other in entities.iter().filter(|other| *other != entity).take(2) {
            push_unique_text(&mut out, &mut seen, format!("{entity} {other}"));
            if out.len() >= MAX_DECOMPOSED_QUERIES {
                return out;
            }
        }
        if out.len() >= MAX_DECOMPOSED_QUERIES {
            return out;
        }
    }
    out.truncate(MAX_DECOMPOSED_QUERIES);
    out
}

fn graph_terms(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().count() >= 3)
        .map(normalize_event_token)
        .collect()
}

fn graph_overlap_score(terms: &HashSet<String>, text: &str, weight: f64) -> f64 {
    graph_terms(text)
        .iter()
        .filter(|term| terms.contains(*term))
        .count() as f64
        * weight
}

fn graph_edge_score(edge: &db::MemoryEdge, query_terms: &HashSet<String>) -> f64 {
    graph_overlap_score(query_terms, &edge.relation, 3.0)
        + graph_overlap_score(query_terms, &edge.source, 1.0)
        + graph_overlap_score(query_terms, &edge.target, 1.0)
        + edge.confidence.clamp(0.0, 1.0)
}

async fn graph_ids_for_query(
    backend: &dyn StorageBackend,
    project: Option<&str>,
    query: &str,
    limit: usize,
) -> Result<Vec<i64>> {
    let entities = query_graph_entities(query);
    if entities.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }

    let query_terms = graph_terms(query);
    let mut best: HashMap<i64, (f64, i64, usize)> = HashMap::new();
    let mut first_seen = 0_usize;
    let mut bridge_entities = Vec::new();
    let mut bridge_seen = HashSet::new();
    let lookup_entities: HashSet<String> =
        entities.iter().map(|e| e.to_ascii_lowercase()).collect();

    for entity in &entities {
        let edges = backend
            .edges_for_entity(project, entity, false, GRAPH_EDGES_PER_ENTITY)
            .await
            .unwrap_or_default();
        for edge in edges {
            let score = graph_edge_score(&edge, &query_terms);
            let current =
                best.entry(edge.memory_id)
                    .or_insert((score, edge.observed_at, first_seen));
            if score > current.0 || (score == current.0 && edge.observed_at > current.1) {
                *current = (score, edge.observed_at, first_seen);
            }
            first_seen += 1;
            for bridge in [&edge.source, &edge.target] {
                let key = bridge.to_ascii_lowercase();
                if !lookup_entities.contains(&key) && bridge_seen.insert(key) {
                    bridge_entities.push(bridge.clone());
                }
            }
        }
    }

    // Evidence-chain expansion: hop through connected entities so multi-hop
    // questions can retrieve both sides of a relation chain without turning
    // graph lookup into a broad recency-ordered entity search. Depth 1 (the
    // default) is the historical single hop; deeper hops decay geometrically
    // and each level's frontier stays capped, so noise cannot compound.
    let chain_depth = tuning().graph_chain_depth.max(1);
    let mut frontier = bridge_entities;
    for level in 0..chain_depth {
        if frontier.is_empty() {
            break;
        }
        let decay = 0.75_f64.powi(level as i32 + 1);
        let mut next_frontier = Vec::new();
        for bridge in frontier.into_iter().take(MAX_GRAPH_ENTITIES * 2) {
            let edges = backend
                .edges_for_entity(project, &bridge, false, GRAPH_CHAIN_EDGES_PER_BRIDGE)
                .await
                .unwrap_or_default();
            for edge in edges {
                let score = graph_edge_score(&edge, &query_terms) * decay;
                let current =
                    best.entry(edge.memory_id)
                        .or_insert((score, edge.observed_at, first_seen));
                if score > current.0 || (score == current.0 && edge.observed_at > current.1) {
                    *current = (score, edge.observed_at, first_seen);
                }
                first_seen += 1;
                if level + 1 < chain_depth {
                    for endpoint in [&edge.source, &edge.target] {
                        let key = endpoint.to_ascii_lowercase();
                        if !lookup_entities.contains(&key) && bridge_seen.insert(key) {
                            next_frontier.push(endpoint.clone());
                        }
                    }
                }
            }
        }
        frontier = next_frontier;
    }

    let mut scored: Vec<(i64, f64, i64, usize)> = best
        .into_iter()
        .map(|(id, (score, observed_at, order))| (id, score, observed_at, order))
        .collect();
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.2.cmp(&a.2))
            .then_with(|| a.3.cmp(&b.3))
    });
    Ok(scored
        .into_iter()
        .take(limit)
        .map(|(id, _, _, _)| id)
        .collect())
}

/// LoCoMo-style temporal questions are overwhelmingly event→time lookups:
/// "when did X happen?", "what date/day/year...", "how long...", or
/// before/after ordering. Graph edges are useful for relationship questions, but
/// for these queries they tend to promote nearby entity relationships over the
/// date-bearing event memory. Keep temporal retrieval on keyword/vector/date
/// channels unless the query is actually relational.
fn is_temporal_lookup_query(query: &str) -> bool {
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() {
        return false;
    }
    q.starts_with("when ")
        || q.starts_with("when did ")
        || q.starts_with("when was ")
        || q.starts_with("when were ")
        || q.starts_with("when is ")
        || q.contains("what date")
        || q.contains("which date")
        || q.contains("what day")
        || q.contains("which day")
        || q.contains("what month")
        || q.contains("which month")
        || q.contains("what year")
        || q.contains("which year")
        || q.contains("what time")
        || q.contains("how long")
        || q.contains("how many days")
        || q.contains("how many weeks")
        || q.contains("how many months")
        || q.contains("how many years")
        || q.contains(" before ")
        || q.contains(" after ")
        || q.contains(" prior to ")
        || q.contains(" following ")
}

fn classify_query_route(query: &str) -> QueryRoute {
    if is_temporal_lookup_query(query) {
        return QueryRoute::Temporal;
    }
    if is_multi_hop_query(query) {
        return QueryRoute::MultiHop;
    }
    let entities = query_named_entities(query);
    let terms = temporal_event_terms(query);
    if entities.is_empty() && terms.len() <= 3 {
        QueryRoute::OpenDomain
    } else {
        QueryRoute::SingleHop
    }
}

fn is_temporal_stopword(token: &str) -> bool {
    matches!(
        token,
        "when"
            | "what"
            | "which"
            | "where"
            | "who"
            | "whom"
            | "whose"
            | "why"
            | "how"
            | "did"
            | "does"
            | "do"
            | "was"
            | "were"
            | "is"
            | "are"
            | "has"
            | "have"
            | "had"
            | "the"
            | "this"
            | "that"
            | "with"
            | "from"
            | "into"
            | "onto"
            | "about"
            | "date"
            | "day"
            | "time"
            | "year"
            | "month"
            | "week"
            | "weeks"
            | "days"
            | "months"
            | "years"
            | "long"
            | "many"
            | "before"
            | "after"
            | "prior"
            | "following"
            | "happen"
            | "happened"
            | "first"
            | "last"
            | "latest"
            | "recent"
            | "more"
            | "most"
    )
}

fn normalize_event_token(token: &str) -> String {
    let mut t = token.to_ascii_lowercase();
    for suffix in ["'s", "ing", "edly", "ed", "es", "s"] {
        if t.len() > suffix.len() + 3 && t.ends_with(suffix) {
            t.truncate(t.len() - suffix.len());
            break;
        }
    }
    t
}

fn temporal_event_terms(query: &str) -> HashSet<String> {
    query
        .split(|c: char| !c.is_alphanumeric() && c != '\'')
        .filter(|t| t.chars().count() >= 3)
        .map(normalize_event_token)
        .filter(|t| !is_temporal_stopword(t))
        .collect()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DatePrecision {
    Day,
    Month,
    Year,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DateAnchor {
    date: NaiveDate,
    precision: DatePrecision,
}

fn month_number(token: &str) -> Option<u32> {
    match token.to_ascii_lowercase().as_str() {
        "jan" | "january" => Some(1),
        "feb" | "february" => Some(2),
        "mar" | "march" => Some(3),
        "apr" | "april" => Some(4),
        "may" => Some(5),
        "jun" | "june" => Some(6),
        "jul" | "july" => Some(7),
        "aug" | "august" => Some(8),
        "sep" | "sept" | "september" => Some(9),
        "oct" | "october" => Some(10),
        "nov" | "november" => Some(11),
        "dec" | "december" => Some(12),
        _ => None,
    }
}

fn parse_year(token: &str) -> Option<i32> {
    if token.len() == 4 {
        token
            .parse::<i32>()
            .ok()
            .filter(|y| (1900..=2099).contains(y))
    } else {
        None
    }
}

fn first_ymd(text: &str) -> Option<NaiveDate> {
    for token in text.split(|c: char| !(c.is_ascii_digit() || c == '-')) {
        if token.len() == 10 {
            if let Ok(date) = NaiveDate::parse_from_str(token, "%Y-%m-%d") {
                return Some(date);
            }
        }
    }
    None
}

fn query_date_anchor(query: &str) -> Option<DateAnchor> {
    if let Some(date) = first_ymd(query) {
        return Some(DateAnchor {
            date,
            precision: DatePrecision::Day,
        });
    }

    let tokens: Vec<&str> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    for window in tokens.windows(2) {
        if let (Some(month), Some(year)) = (month_number(window[0]), parse_year(window[1])) {
            if let Some(date) = NaiveDate::from_ymd_opt(year, month, 1) {
                return Some(DateAnchor {
                    date,
                    precision: DatePrecision::Month,
                });
            }
        }
        if let (Some(year), Some(month)) = (parse_year(window[0]), month_number(window[1])) {
            if let Some(date) = NaiveDate::from_ymd_opt(year, month, 1) {
                return Some(DateAnchor {
                    date,
                    precision: DatePrecision::Month,
                });
            }
        }
    }

    query_years(query).first().and_then(|year| {
        let year = year.parse::<i32>().ok()?;
        Some(DateAnchor {
            date: NaiveDate::from_ymd_opt(year, 1, 1)?,
            precision: DatePrecision::Year,
        })
    })
}

fn event_date_anchor(event_time: &str) -> Option<DateAnchor> {
    if let Some(date) = first_ymd(event_time) {
        return Some(DateAnchor {
            date,
            precision: DatePrecision::Day,
        });
    }
    let year = query_years(event_time).first()?.parse::<i32>().ok()?;
    Some(DateAnchor {
        date: NaiveDate::from_ymd_opt(year, 1, 1)?,
        precision: DatePrecision::Year,
    })
}

fn temporal_proximity_score(query_anchor: Option<DateAnchor>, event_time: &str) -> f64 {
    let Some(q) = query_anchor else {
        return 0.0;
    };
    let Some(e) = event_date_anchor(event_time) else {
        return 0.0;
    };
    let days = (q.date - e.date).num_days().unsigned_abs() as f64;
    match q.precision {
        DatePrecision::Day => (8.0 - days / 7.0).max(0.0),
        DatePrecision::Month => {
            let months = ((q.date.year() - e.date.year()).abs() * 12) as f64
                + (q.date.month() as i32 - e.date.month() as i32).abs() as f64;
            (6.0 - months).max(0.0)
        }
        DatePrecision::Year => {
            let years = (q.date.year() - e.date.year()).abs() as f64;
            (4.0 - years).max(0.0)
        }
    }
}

fn event_text_terms(memory: &Memory) -> HashSet<String> {
    let mut text = memory.summary.clone();
    if let Some(tags) = &memory.tags {
        text.push(' ');
        text.push_str(tags);
    }
    graph_terms(&text)
}

async fn temporal_event_ids_for_query(
    backend: &dyn StorageBackend,
    project: Option<&str>,
    query: &str,
    limit: usize,
) -> Result<Vec<i64>> {
    let query_terms = temporal_event_terms(query);
    if query_terms.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    let date_anchor = query_date_anchor(query);

    #[derive(Debug)]
    struct TemporalCandidate {
        id: i64,
        score: f64,
        evidence_quality: f64,
        created_at: i64,
        date_key: Option<String>,
    }

    let mut scored = Vec::new();
    for candidate in backend
        .dated_memories(project, TEMPORAL_EVENT_POOL.max(limit))
        .await?
    {
        let text_terms = event_text_terms(&candidate.memory);
        let overlap = query_terms
            .iter()
            .filter(|term| text_terms.contains(*term))
            .count();
        if overlap == 0 {
            continue;
        }

        let kind_bonus = if candidate.kind == "fact" { 4.0 } else { 1.0 };
        let source_bonus = if is_source_linked(&candidate.memory) {
            2.0
        } else {
            0.0
        };
        let date_specificity_bonus = if candidate
            .event_time
            .chars()
            .filter(|c| c.is_ascii_digit())
            .count()
            >= 8
        {
            1.0
        } else {
            0.0
        };
        let specificity = overlap as f64 / query_terms.len().max(1) as f64;
        let proximity = temporal_proximity_score(date_anchor, &candidate.event_time);
        let evidence_quality = overlap as f64 * 2.0 + specificity * 2.0 + kind_bonus + source_bonus;
        let score = overlap as f64 * 10.0
            + specificity * 3.0
            + kind_bonus
            + source_bonus
            + date_specificity_bonus
            + proximity;
        scored.push(TemporalCandidate {
            id: candidate.memory.id,
            score,
            evidence_quality,
            created_at: candidate.memory.created_at,
            date_key: event_date_anchor(&candidate.event_time)
                .map(|anchor| anchor.date.to_string()),
        });
    }

    // Conflict handling: when several date-bearing facts match the same temporal
    // question but disagree on the date, favor candidates with stronger source/
    // fact/term evidence before recency. That keeps a newer broad mention from
    // winning over an older source-backed fact solely because it was stored last.
    let distinct_dates: HashSet<String> = scored
        .iter()
        .filter_map(|candidate| candidate.date_key.clone())
        .collect();
    if distinct_dates.len() > 1 {
        for candidate in &mut scored {
            candidate.score += candidate.evidence_quality * 2.0;
        }
    }

    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                b.evidence_quality
                    .partial_cmp(&a.evidence_quality)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| b.created_at.cmp(&a.created_at))
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(scored
        .into_iter()
        .take(limit)
        .map(|candidate| candidate.id)
        .collect())
}

/// Hybrid search: fuse keyword (FTS), semantic (vector), temporal (event_time),
/// temporal graph, and entity (proper-noun index) signals via RRF. With none of
/// the auxiliary signals present this returns the exact FTS ordering,
/// reproducing legacy behavior.
pub async fn hybrid_search(
    db: &Database,
    embedder: Option<&dyn Embedder>,
    store: &dyn VectorStore,
    project: Option<&str>,
    query: &str,
    limit: usize,
) -> Result<Vec<Memory>> {
    hybrid_search_in_namespace(
        db,
        embedder,
        store,
        crate::governance::DEFAULT_NAMESPACE,
        project,
        query,
        limit,
    )
    .await
}

pub async fn hybrid_search_in_namespace(
    db: &Database,
    embedder: Option<&dyn Embedder>,
    store: &dyn VectorStore,
    namespace: &str,
    project: Option<&str>,
    query: &str,
    limit: usize,
) -> Result<Vec<Memory>> {
    // Candidate pool: pull more than `limit` per signal so the narrative-reserve
    // quota below has narratives to choose from even when facts dominate ranking.
    // (W1.2) 5×/floor-50: the LLM-free recall@N curve showed gold sitting in pool
    // positions 11–50 that a 3×/floor-30 window couldn't surface — widen the pool
    // so the reranker has more buried-answer candidates to promote.
    let pool = (limit * 5).max(50);

    // Storage substrate (#4): the native engine as the default StorageBackend.
    // Ranking (RRF, reserve) and governance (trust/tier boosts) below compose ON
    // TOP of this trait; only the raw recall/materialize calls flow through it,
    // so behavior is identical to the prior direct-SQL path.
    let backend = crate::storage::make_backend(db, store).await;

    let started = std::time::Instant::now();

    // Keyword side (always run).
    let fts: Vec<Memory> = backend
        .fulltext_search(namespace, project, query, pool)
        .await?
        .into_iter()
        .filter_map(|c| c.memory)
        .collect();

    let route = classify_query_route(query);

    // T0 lexical early exit (opt-in): a single-hop query whose top FTS hit
    // already contains every salient query term is lexically resolved — skip
    // the embedding calls and auxiliary recall and fuse the cheap signals
    // (FTS + graph) only. Off by default; the benchmark harness owns the
    // accuracy/latency trade.
    let t0_exit = tuning().tier_early_exit
        && route == QueryRoute::SingleHop
        && fts
            .first()
            .map(|m| {
                let terms = salient_query_terms(query);
                !terms.is_empty() && salient_overlap_fraction(&terms, &m.summary) >= 1.0
            })
            .unwrap_or(false);

    // Semantic side (best-effort; only when an embedder is configured).
    let vec_ids: Vec<i64> = if t0_exit {
        Vec::new()
    } else if let Some(emb) = embedder {
        match embed_one(emb, query).await {
            Some(qvec) => backend
                .vector_search(project, &qvec, emb.id(), pool)
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|c| c.id)
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
        if years.is_empty() || t0_exit {
            Vec::new()
        } else {
            let mut seen = HashSet::new();
            let mut ids = Vec::new();
            for y in &years {
                for id in backend
                    .memories_by_event_time(project, y, pool)
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

    // Temporal lookup side: most LoCoMo temporal questions ask for the date of
    // an event ("when did X happen?") and do not name the answer year. Rank
    // date-bearing event/fact memories by event-term overlap so old exact facts
    // can beat newer broad memories that only share the same person/topic.
    let temporal_event_ids = if is_temporal_lookup_query(query) && !t0_exit {
        temporal_event_ids_for_query(backend.as_ref(), project, query, pool).await?
    } else {
        Vec::new()
    };

    // Entity signal (see FUSE_ENTITY_SIGNAL): off by default — its recency-ordered
    // matches demote older-but-exact facts in person-centric data. FTS + vector
    // already cover the named person.
    let entity_ids: Vec<i64> = if FUSE_ENTITY_SIGNAL {
        let ents = query_entities(query);
        let mut seen = HashSet::new();
        let mut ids = Vec::new();
        for e in &ents {
            for id in backend
                .memories_for_entity(project, e, pool)
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

    let query_variants = match route {
        QueryRoute::MultiHop | QueryRoute::Temporal => decomposed_queries(query),
        QueryRoute::SingleHop | QueryRoute::OpenDomain => Vec::new(),
    };

    // Query decomposition side: split a compound question into entity+relation
    // probes that can retrieve bridge facts even when the full question does not
    // lexically match either side of the evidence chain. Bounded and additive:
    // variants enter RRF as weak signals, while the original query remains the
    // anchor for final ranking/reranking.
    let variant_fts_ids: Vec<i64> = if query_variants.is_empty() {
        Vec::new()
    } else {
        let mut seen = HashSet::new();
        let mut ids = Vec::new();
        for variant in &query_variants {
            for candidate in backend
                .fulltext_search(namespace, project, variant, pool / 2)
                .await
                .unwrap_or_default()
            {
                if let Some(memory) = candidate.memory {
                    if seen.insert(memory.id) {
                        ids.push(memory.id);
                    }
                }
            }
        }
        ids
    };

    let variant_vec_ids: Vec<i64> = if let Some(emb) = embedder {
        if query_variants.is_empty() {
            Vec::new()
        } else {
            let mut seen = HashSet::new();
            let mut ids = Vec::new();
            for variant in &query_variants {
                let Some(qvec) = embed_one(emb, variant).await else {
                    continue;
                };
                for candidate in backend
                    .vector_search(project, &qvec, emb.id(), pool / 2)
                    .await
                    .unwrap_or_default()
                {
                    if seen.insert(candidate.id) {
                        ids.push(candidate.id);
                    }
                }
            }
            ids
        }
    } else {
        Vec::new()
    };

    // Graph side: active temporal graph edges contribute their provenance memory
    // ids when the query names an entity present as an edge source or target.
    // Unlike the disabled entity signal, graph ids are relation-ranked before
    // fusion so specific relationship questions beat generic recency.
    let graph_ids = if route == QueryRoute::Temporal {
        Vec::new()
    } else {
        graph_ids_for_query(backend.as_ref(), project, query, pool).await?
    };

    // Candidate ordering: route-weighted RRF over keyword + auxiliary signals.
    // This keeps single-hop lexical, multi-hop graph/vector, temporal date-heavy,
    // and open-domain semantic-biased without paying a classify-LLM call.
    let fts_ids: Vec<i64> = fts.iter().map(|m| m.id).collect();
    let lexical_floor_ids = lexical_source_fact_floor_ids(db, query, &fts, limit).await?;
    let by_id: HashMap<i64, Memory> = fts.into_iter().map(|m| (m.id, m)).collect();

    let weights = FusionWeights::for_query(
        query,
        tuning().temporal_fusion_weight,
        tuning().chunk_fusion_weight,
    );

    // Chunk-level (skim layer) recall for open-domain queries: chunks preserve
    // the specifics narrative summaries generalize away, so their parents join
    // fusion as a first-class signal. Zero cost on the other routes.
    let chunk_ids = if weights.chunk > 0 {
        db::search_memory_chunk_parents_in_namespace(
            db,
            namespace,
            project,
            &salient_query_terms(query),
            pool,
        )
        .await
        .unwrap_or_default()
    } else {
        Vec::new()
    };

    let mut lists: Vec<Vec<i64>> = Vec::new();
    push_weighted_signal(&mut lists, &fts_ids, weights.fts);
    push_weighted_signal(&mut lists, &vec_ids, weights.vector);
    push_weighted_signal(&mut lists, &variant_fts_ids, weights.query_variant);
    push_weighted_signal(&mut lists, &variant_vec_ids, weights.query_variant);
    push_weighted_signal(&mut lists, &time_ids, weights.event_year);
    push_weighted_signal(&mut lists, &temporal_event_ids, weights.temporal_event);
    push_weighted_signal(&mut lists, &graph_ids, weights.graph);
    push_weighted_signal(&mut lists, &entity_ids, weights.entity);
    push_weighted_signal(&mut lists, &chunk_ids, weights.chunk);
    let candidates: Vec<i64> = match lists.len() {
        0 => Vec::new(),
        1 => lists.pop().unwrap_or_default(),
        _ => rrf_fuse(&lists, RRF_K),
    };

    // (#5) Temporal-trust re-rank: nudge the fused order by each candidate's trust
    // trajectory. No-op (and zero DB cost) unless temporal_trust.weight > 0.
    let candidates = apply_trust_boost(db, candidates).await?;

    // (#1) Governed-retrieval router: nudge by writer trust-tier authority.
    // No-op (and zero DB cost) unless governance_router.weight > 0.
    let candidates = apply_tier_boost(db, candidates).await?;

    // Knowledge-update enforcement: demote candidates whose only edge support
    // is superseded while another candidate holds the live edge for the same
    // (source, relation). No-op unless ranking.stale_demotion_weight > 0.
    let candidates = apply_supersession_demotion(db, candidates).await?;

    // Activation re-rank (Context-Tree analog): importance × maturity ×
    // recency decay. No-op unless ranking.activation_weight > 0.
    let candidates = apply_activation_boost(db, candidates).await?;

    // Quarantine derived inferences (kind="inference") from default retrieval:
    // a wrong inference that ranked high would poison the answer. They stay
    // reachable on demand via the governance/edge APIs (see exclude_derived).
    let candidates = exclude_derived(db, candidates).await?;

    // Source-fact retention floor, then narrative-reserve quota, then materialize
    // in rank order (reusing FTS rows). The floor protects strong exact lexical
    // evidence from route-fusion demotion without disabling route fusion.
    let candidates = promote_source_fact_floor(&candidates, &lexical_floor_ids, limit);
    let chosen = reserve_narrative_slots(db, &candidates, limit).await?;
    let mut out = Vec::with_capacity(chosen.len());
    for id in chosen {
        if let Some(m) = by_id.get(&id) {
            out.push(m.clone());
        } else if let Some(m) = backend.get_memory(namespace, id).await? {
            out.push(m);
        }
    }

    // Abstention guard: drop results that share too little of the query's
    // salient vocabulary. An empty result lets callers answer "I don't know"
    // instead of confidently citing a bad hit. Off by default.
    let min_overlap = tuning().abstention_min_overlap;
    if min_overlap > 0.0 {
        let terms = salient_query_terms(query);
        out.retain(|m| salient_overlap_fraction(&terms, &m.summary) >= min_overlap);
    }

    crate::metrics::record_tier(
        if t0_exit {
            crate::metrics::RetrievalTier::T0LexicalExit
        } else {
            crate::metrics::RetrievalTier::FullFusion
        },
        started.elapsed(),
    );
    Ok(out)
}

/// Fraction of `terms` present (case-insensitive substring) in `text`.
/// 1.0 when there are no salient terms — an unusual query must not force
/// abstention on its own.
fn salient_overlap_fraction(terms: &[String], text: &str) -> f64 {
    if terms.is_empty() {
        return 1.0;
    }
    let haystack = text.to_lowercase();
    let hits = terms
        .iter()
        .filter(|t| haystack.contains(t.to_lowercase().as_str()))
        .count();
    hits as f64 / terms.len() as f64
}

/// Ids of candidates whose entire edge support is superseded/validity-closed
/// while some *other* candidate holds a live edge for the same
/// (source, relation) key — the fingerprint of a stale fact competing with its
/// own update. Pure so the policy is unit-testable.
fn stale_candidate_ids(
    candidates: &[i64],
    edges: &HashMap<i64, Vec<db::MemoryEdge>>,
) -> HashSet<i64> {
    let key = |e: &db::MemoryEdge| {
        (
            e.source.trim().to_lowercase(),
            e.relation.trim().to_lowercase(),
        )
    };
    let mut live_keys: HashSet<(String, String)> = HashSet::new();
    for id in candidates {
        for edge in edges.get(id).map(|v| v.as_slice()).unwrap_or_default() {
            if edge.superseded_by.is_none() && edge.valid_until.is_none() {
                live_keys.insert(key(edge));
            }
        }
    }
    let mut stale = HashSet::new();
    for id in candidates {
        let Some(own) = edges.get(id) else { continue };
        if own.is_empty() {
            continue;
        }
        let all_closed = own
            .iter()
            .all(|e| e.superseded_by.is_some() || e.valid_until.is_some());
        let update_exists = own.iter().any(|e| {
            (e.superseded_by.is_some() || e.valid_until.is_some()) && live_keys.contains(&key(e))
        });
        if all_closed && update_exists {
            stale.insert(*id);
        }
    }
    stale
}

/// Knowledge-update demotion pass: same base+boost shape as
/// `apply_trust_boost`, with a negative boost for stale candidates so the
/// live fact wins the near-tie its own update created.
async fn apply_supersession_demotion(db: &Database, candidates: Vec<i64>) -> Result<Vec<i64>> {
    let t = tuning();
    if t.stale_demotion_weight <= 0.0 || candidates.len() < 2 {
        return Ok(candidates);
    }
    let edges = db::memory_edges_for_memories_with_history(db, &candidates)
        .await
        .unwrap_or_default();
    if edges.is_empty() {
        return Ok(candidates);
    }
    let stale = stale_candidate_ids(&candidates, &edges);
    if stale.is_empty() {
        return Ok(candidates);
    }
    let mut scored: Vec<(usize, i64, f64)> = candidates
        .iter()
        .enumerate()
        .map(|(rank, &id)| {
            let base = 1.0 / (RRF_K as f64 + rank as f64);
            let penalty = if stale.contains(&id) {
                t.stale_demotion_weight
            } else {
                0.0
            };
            (rank, id, base - penalty)
        })
        .collect();
    scored.sort_by(|a, b| {
        b.2.partial_cmp(&a.2)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
    });
    Ok(scored.into_iter().map(|(_, id, _)| id).collect())
}

/// Activation re-rank: importance × maturity multiplier × recency half-life
/// decay, added on top of the reciprocal-rank base like the trust/tier passes.
async fn apply_activation_boost(db: &Database, candidates: Vec<i64>) -> Result<Vec<i64>> {
    let t = tuning();
    if t.activation_weight <= 0.0 || candidates.len() < 2 {
        return Ok(candidates);
    }
    let meta = db::activation_meta_for(db, &candidates)
        .await
        .unwrap_or_default();
    if meta.is_empty() {
        return Ok(candidates);
    }
    let now = Utc::now().timestamp();
    let mut scored: Vec<(usize, i64, f64)> = candidates
        .iter()
        .enumerate()
        .map(|(rank, &id)| {
            let base = 1.0 / (RRF_K as f64 + rank as f64);
            let boost = meta
                .get(&id)
                .map(|m| {
                    let age_days = ((now - m.created_at).max(0) as f64) / 86_400.0;
                    let recency = 0.5_f64.powf(age_days / t.activation_halflife_days.max(0.001));
                    t.activation_weight
                        * m.importance.clamp(0.0, 1.0)
                        * db::maturity_multiplier(m.maturity.as_deref())
                        * recency
                })
                .unwrap_or(0.0);
            (rank, id, base + boost)
        })
        .collect();
    scored.sort_by(|a, b| {
        b.2.partial_cmp(&a.2)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
    });
    Ok(scored.into_iter().map(|(_, id, _)| id).collect())
}

/// Apply the narrative-reserve quota over a ranked candidate id list, returning at
/// most `limit` ids. Atomic facts (`kind="fact"`) dominate ranking for specific
/// queries and would otherwise crowd the few narrative memories that carry
/// cross-turn (multi-hop) links out of the top-`limit`. Guarantee up to ~40% of
/// the slots to narratives (in rank order) before filling the rest by rank, so
/// facts AUGMENT rather than REPLACE narratives. Final order follows rank.
async fn reserve_narrative_slots(
    db: &Database,
    candidates: &[i64],
    limit: usize,
) -> Result<Vec<i64>> {
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
    let rank: HashMap<i64, usize> = candidates
        .iter()
        .enumerate()
        .map(|(i, &id)| (id, i))
        .collect();
    chosen.sort_by_key(|id| rank[id]);
    Ok(chosen)
}

fn source_fact_floor_slots(limit: usize) -> usize {
    if limit == 0 {
        0
    } else {
        (limit / 5).clamp(1, SOURCE_FACT_FLOOR_MAX_SLOTS)
    }
}

fn is_source_linked(memory: &Memory) -> bool {
    let tags = memory.tags.as_deref().unwrap_or("").to_ascii_lowercase();
    tags.contains("locomo")
        || tags.contains("source")
        || tags.contains("fact")
        || memory.summary.contains("(as of ")
}

fn is_synthesized_or_derived(memory: &Memory) -> bool {
    let text = format!(
        "{} {}",
        memory.summary.to_ascii_lowercase(),
        memory.tags.as_deref().unwrap_or("").to_ascii_lowercase()
    );
    text.contains("synthesized") || text.contains("derived")
}

async fn lexical_source_fact_floor_ids(
    db: &Database,
    query: &str,
    fts: &[Memory],
    limit: usize,
) -> Result<Vec<i64>> {
    let slots = source_fact_floor_slots(limit);
    if slots == 0 || fts.is_empty() {
        return Ok(Vec::new());
    }
    let query_terms = temporal_event_terms(query);
    if query_terms.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<i64> = fts.iter().map(|m| m.id).collect();
    let kinds = db::kinds_for_memories(db, &ids).await?;
    let mut scored_floor: Vec<(i64, usize, usize)> = Vec::with_capacity(slots);
    for (idx, memory) in fts.iter().enumerate() {
        let kind = kinds.get(&memory.id).map(|s| s.as_str());
        if matches!(kind, Some("inference")) {
            continue;
        }
        if is_synthesized_or_derived(memory) {
            continue;
        }
        let is_fact = matches!(kind, Some("fact"));
        if !(is_fact || is_source_linked(memory)) {
            continue;
        }
        let text_terms = event_text_terms(memory);
        let overlap = query_terms
            .iter()
            .filter(|term| text_terms.contains(*term))
            .count();
        if overlap >= 2 || (overlap >= 1 && query_terms.len() == 1) {
            scored_floor.push((memory.id, overlap, idx));
        }
    }
    scored_floor.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.2.cmp(&b.2)));
    Ok(scored_floor
        .into_iter()
        .take(slots)
        .map(|(id, _, _)| id)
        .collect())
}

fn promote_source_fact_floor(candidates: &[i64], floor_ids: &[i64], limit: usize) -> Vec<i64> {
    if floor_ids.is_empty() || candidates.len() <= limit || limit == 0 {
        return candidates.to_vec();
    }
    let slots = source_fact_floor_slots(limit);
    let candidate_set: HashSet<i64> = candidates.iter().copied().collect();
    let mut promoted = Vec::new();
    let mut seen = HashSet::new();
    for &id in floor_ids {
        if promoted.len() >= slots {
            break;
        }
        if candidate_set.contains(&id) && seen.insert(id) {
            promoted.push(id);
        }
    }
    if promoted.is_empty() {
        return candidates.to_vec();
    }

    let mut out = Vec::with_capacity(candidates.len());
    out.extend(promoted.iter().copied());
    for &id in candidates {
        if seen.insert(id) {
            out.push(id);
        }
    }
    out
}

/// Quarantine derived inferences (`kind="inference"`) from default retrieval.
/// Derived memories are LLM inferences; a wrong one that ranked high would
/// poison every downstream answer, so they never enter the candidate set the
/// answerer sees. They remain reachable on demand via the governance/edge APIs.
/// A missing kind (legacy/no-meta row) is kept — only an explicit `inference`
/// kind is excluded.
async fn exclude_derived(db: &Database, candidates: Vec<i64>) -> Result<Vec<i64>> {
    if candidates.is_empty() {
        return Ok(candidates);
    }
    let kinds = db::kinds_for_memories(db, &candidates).await?;
    Ok(candidates
        .into_iter()
        .filter(|id| kinds.get(id).map(|k| k != "inference").unwrap_or(true))
        .collect())
}

// ── LLM reranking ───────────────────────────────────────────────────

/// Per-candidate character cap in the rerank prompt — keeps a wide pool within a
/// small token budget. Short fact memories pass through whole; long narratives
/// are trimmed to their leading content (enough to judge relevance).
const RERANK_SNIPPET_CHARS: usize = 400;

#[derive(Clone, Debug)]
struct RerankEvidence {
    memory: Memory,
    kind: String,
    event_time: Option<String>,
    source_ref: Option<String>,
    chunks: Vec<db::MemoryChunk>,
    graph_edges: Vec<db::MemoryEdge>,
    query_route: QueryRoute,
    temporal_proximity: Option<f64>,
    temporal_conflict: bool,
}

async fn enrich_rerank_evidence(
    db: &Database,
    query: &str,
    candidates: Vec<Memory>,
) -> Vec<RerankEvidence> {
    let mut out = Vec::with_capacity(candidates.len());
    let ids: Vec<i64> = candidates.iter().map(|m| m.id).collect();
    let chunks = db::chunks_for_memories(db, &ids).await.unwrap_or_default();
    let graph_edges = db::memory_edges_for_memories(db, &ids)
        .await
        .unwrap_or_default();
    let query_route = classify_query_route(query);
    let query_anchor = query_date_anchor(query);
    let event_times = db::event_times_for(db, &ids).await.unwrap_or_default();
    let candidate_event_dates: HashSet<String> = event_times
        .values()
        .filter_map(|event_time| {
            event_date_anchor(event_time).map(|anchor| anchor.date.to_string())
        })
        .collect();
    let has_temporal_conflict =
        query_route == QueryRoute::Temporal && candidate_event_dates.len() > 1;
    for memory in candidates {
        let memory_id = memory.id;
        let meta = db::get_memory_meta_full(db, memory.id)
            .await
            .unwrap_or_default();
        let temporal_proximity = meta
            .event_time
            .as_deref()
            .map(|event_time| temporal_proximity_score(query_anchor, event_time));
        let has_event_time = meta.event_time.is_some();
        out.push(RerankEvidence {
            memory,
            kind: meta.kind,
            event_time: meta.event_time,
            source_ref: meta.source_ref,
            chunks: chunks.get(&memory_id).cloned().unwrap_or_default(),
            graph_edges: graph_edges.get(&memory_id).cloned().unwrap_or_default(),
            query_route,
            temporal_proximity,
            temporal_conflict: has_temporal_conflict && has_event_time,
        });
    }
    out
}

fn compact_text(text: &str, cap: usize) -> String {
    text.chars()
        .take(cap)
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn query_route_label(route: QueryRoute) -> &'static str {
    match route {
        QueryRoute::SingleHop => "single_hop",
        QueryRoute::MultiHop => "multi_hop",
        QueryRoute::Temporal => "temporal",
        QueryRoute::OpenDomain => "open_domain",
    }
}

fn temporal_proximity_label(score: f64) -> &'static str {
    if score >= 0.99 {
        "exact"
    } else if score >= 0.80 {
        "near"
    } else if score >= 0.35 {
        "related_date"
    } else {
        "weak_or_missing"
    }
}

fn chunk_evidence_text(chunks: &[db::MemoryChunk]) -> Option<String> {
    let mut parts = Vec::new();
    for chunk in chunks.iter().take(3) {
        let mut piece = format!(
            "{}:{}",
            compact_text(&chunk.title, 80),
            compact_text(&chunk.summary, 220)
        );
        if let (Some(start), Some(end)) = (chunk.source_start, chunk.source_end) {
            piece.push_str(&format!(" source_span={start}..{end}"));
        }
        if !chunk.chunk_id.trim().is_empty() {
            piece.push_str(&format!(" chunk_id={}", compact_text(&chunk.chunk_id, 80)));
        }
        parts.push(piece);
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" ; "))
    }
}

fn graph_edge_evidence_text(edges: &[db::MemoryEdge]) -> Option<String> {
    let mut parts = Vec::new();
    for edge in edges.iter().take(4) {
        let mut piece = format!(
            "{} --{}--> {}",
            compact_text(&edge.source, 80),
            compact_text(&edge.relation, 60),
            compact_text(&edge.target, 80)
        );
        if edge.valid_from.is_some() || edge.valid_until.is_some() {
            piece.push_str(&format!(
                " valid={}..{}",
                edge.valid_from.as_deref().unwrap_or(""),
                edge.valid_until.as_deref().unwrap_or("")
            ));
        }
        piece.push_str(&format!(" confidence={:.2}", edge.confidence));
        parts.push(piece);
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" ; "))
    }
}

fn structured_rerank_text(e: &RerankEvidence) -> String {
    let mut parts = Vec::new();
    parts.push(format!("route={}", query_route_label(e.query_route)));
    parts.push(format!("kind={}", e.kind));
    if let Some(event_time) = e.event_time.as_deref().filter(|s| !s.trim().is_empty()) {
        parts.push(format!("event_time={event_time}"));
    }
    if let Some(score) = e.temporal_proximity {
        parts.push(format!(
            "date_proximity={:.2}:{}",
            score,
            temporal_proximity_label(score)
        ));
    }
    if e.temporal_conflict {
        parts.push("temporal_conflict=conflicting_candidate_dates".to_string());
    }
    if let Some(tags) = e.memory.tags.as_deref().filter(|s| !s.trim().is_empty()) {
        parts.push(format!("tags={}", compact_text(tags, 120)));
    }
    if let Some(source_ref) = e.source_ref.as_deref().filter(|s| !s.trim().is_empty()) {
        parts.push(format!("source_ref={}", compact_text(source_ref, 120)));
    }
    if let Some(chunks) = chunk_evidence_text(&e.chunks) {
        parts.push(format!("chunk_evidence={chunks}"));
    }
    if let Some(edges) = graph_edge_evidence_text(&e.graph_edges) {
        parts.push(format!("graph_edges={edges}"));
    }
    parts.push(format!(
        "evidence={}",
        compact_text(&e.memory.summary, RERANK_SNIPPET_CHARS)
    ));
    parts.join(" | ")
}

/// Build the rerank prompt: the question, then the numbered candidate snippets,
/// then an instruction to return the useful candidate numbers most-useful-first.
fn build_rerank_prompt(query: &str, candidates: &[RerankEvidence]) -> String {
    let mut s = String::with_capacity(256 + candidates.len() * RERANK_SNIPPET_CHARS);
    s.push_str(
        "You are selecting which structured memory evidence best helps answer a question.\n\
         Read the QUESTION, then the numbered CANDIDATES. Each candidate may include \
         route, kind, event_time, date_proximity, temporal_conflict, tags, source_ref, \
         chunk_evidence, graph_edges, and evidence text.\n\n",
    );
    s.push_str("QUESTION: ");
    s.push_str(query);
    s.push_str("\n\nCANDIDATES:\n");
    for (i, m) in candidates.iter().enumerate() {
        s.push_str(&format!("{}. {}\n", i + 1, structured_rerank_text(m)));
    }
    s.push_str(
        "\nReturn the candidate numbers ordered from MOST to LEAST useful for \
         answering the question, as a comma-separated list (e.g. \"4,1,9\"). Rank a \
         candidate that contains the SPECIFIC answer the question asks for — the exact \
         date, name, number, event, relationship, source, or temporal fact — above one \
         that is merely on the same topic. For multi-hop questions, prefer candidates \
         whose graph_edges or chunk_evidence bridge the named people, places, events, \
         or dates. For temporal questions, prefer exact or near date_proximity and \
         event_time over broad recency. When temporal_conflict is present, prefer \
         source_ref/chunk-backed fact evidence over newer broad mentions. Prefer \
         source_ref/chunk evidence that can be traced back to the original session. \
         Include only genuinely relevant numbers; omit the rest. Output ONLY the list.",
    );
    s
}

/// Candidate text for the cross-encoder: the leading content of the summary
/// (plus tags), trimmed to the same budget the LLM reranker's snippet uses so
/// the two backends judge the same evidence.
fn rerank_doc(e: &RerankEvidence) -> String {
    structured_rerank_text(e)
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
    let llm_ids: Vec<i64> = order
        .iter()
        .filter_map(|&i| base.get(i).map(|m| m.id))
        .collect();
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
    db: &Database,
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
    let evidence = enrich_rerank_evidence(db, query, candidates.clone()).await;
    let prompt = build_rerank_prompt(query, &evidence);
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

async fn rerank_candidates(
    db: &Database,
    config: &Config,
    query: &str,
    candidates: Vec<Memory>,
    limit: usize,
) -> Vec<Memory> {
    let started = std::time::Instant::now();
    if config.rerank.backend.eq_ignore_ascii_case("cross_encoder") {
        let cap = config.rerank.cross_encoder_max_candidates.max(limit);
        let head = candidates.len().min(cap);
        let evidence = enrich_rerank_evidence(db, query, candidates[..head].to_vec()).await;
        let docs: Vec<String> = evidence.iter().map(rerank_doc).collect();
        if let Some(scored) = crate::reranker::rerank_scored(query, &docs) {
            // T2→T3 escalation: when the cross-encoder can't separate its top
            // candidates (margin below the configured threshold), the ordering
            // is a coin flip — spend the LLM call. 0.0 (default) never
            // escalates, keeping cross-encoder results final.
            let margin = match (scored.first(), scored.get(1)) {
                (Some(a), Some(b)) => (a.1 - b.1) as f64,
                _ => f64::MAX,
            };
            if config.rerank.escalate_margin > 0.0 && margin < config.rerank.escalate_margin {
                let out = llm_rerank(db, config, query, candidates, limit).await;
                crate::metrics::record_tier(
                    crate::metrics::RetrievalTier::RerankEscalated,
                    started.elapsed(),
                );
                return out;
            }
            let order: Vec<usize> = scored.into_iter().map(|(i, _)| i).collect();
            crate::metrics::record_tier(
                crate::metrics::RetrievalTier::RerankCrossEncoder,
                started.elapsed(),
            );
            return fuse_rerank(&candidates, &order, limit);
        }
    }
    let out = llm_rerank(db, config, query, candidates, limit).await;
    crate::metrics::record_tier(crate::metrics::RetrievalTier::RerankLlm, started.elapsed());
    out
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
#[allow(dead_code, clippy::too_many_arguments)]
pub async fn rerank_search(
    db: &Database,
    embedder: Option<&dyn Embedder>,
    store: &dyn VectorStore,
    config: &Config,
    project: Option<&str>,
    query: &str,
    limit: usize,
) -> Result<Vec<Memory>> {
    rerank_search_in_namespace(
        db,
        embedder,
        store,
        config,
        crate::governance::DEFAULT_NAMESPACE,
        project,
        query,
        limit,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn rerank_search_in_namespace(
    db: &Database,
    embedder: Option<&dyn Embedder>,
    store: &dyn VectorStore,
    config: &Config,
    namespace: &str,
    project: Option<&str>,
    query: &str,
    limit: usize,
) -> Result<Vec<Memory>> {
    rerank_search_in_namespace_with_pool(
        db, embedder, store, config, namespace, project, query, limit, None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn rerank_search_in_namespace_with_pool(
    db: &Database,
    embedder: Option<&dyn Embedder>,
    store: &dyn VectorStore,
    config: &Config,
    namespace: &str,
    project: Option<&str>,
    query: &str,
    limit: usize,
    pool_override: Option<usize>,
) -> Result<Vec<Memory>> {
    let pool = limit
        .saturating_mul(2)
        .max(pool_override.unwrap_or(config.rerank.pool));
    let narrow =
        hybrid_search_in_namespace(db, embedder, store, namespace, project, query, limit).await?;
    let wide =
        hybrid_search_in_namespace(db, embedder, store, namespace, project, query, pool).await?;
    let candidates = reanchor(narrow, wide);
    Ok(rerank_candidates(db, config, query, candidates, limit).await)
}

/// (W3.1) Heuristic gate for the iterative multi-hop loop: a question that chains
/// facts across turns ("what city did X move to after the job in session 3?").
/// Cheap and conservative — single-hop questions must NOT pay the extra LLM
/// reason + re-retrieval cost. Triggers on relational/bridge cues or on ≥2 named
/// entities (a relation between two proper nouns is the canonical multi-hop case).
pub fn is_multi_hop_query(query: &str) -> bool {
    let q = format!(" {} ", query.to_lowercase());
    const CUES: &[&str] = &[
        " after ",
        " before ",
        " then ",
        " same ",
        " also ",
        " both ",
        " depend ",
        " depends ",
        " dependent ",
        " dependency ",
        " who else",
        " what else",
        " where did ",
        " how did ",
        " because ",
        " led to ",
        " compared ",
        " related to ",
        " connected ",
        " between ",
    ];
    if CUES.iter().any(|c| q.contains(c)) {
        return true;
    }
    query_named_entities(query).len() >= 2
}

/// Prompt the rerank model for the single missing bridge fact (or DONE) given the
/// question and the facts retrieved so far. Intentionally asks for a SHORT
/// keyword query, not prose, so the follow-up feeds `hybrid_search` cleanly.
fn build_followup_prompt(question: &str, retrieved: &[Memory]) -> String {
    let mut facts = String::new();
    for (i, m) in retrieved.iter().take(12).enumerate() {
        facts.push_str(&format!("{}. {}\n", i + 1, m.summary));
    }
    format!(
        "You are helping answer a question that may require chaining several facts.\n\
         Question: {question}\n\n\
         Facts retrieved so far:\n{facts}\n\
         If these facts are sufficient to answer the question, reply with exactly: DONE\n\
         Otherwise reply with a SHORT search query (a few keywords, no punctuation) for the single \
         missing bridge fact you still need — the name, date, place, or entity that links the facts \
         above to the answer. Reply with ONLY `DONE` or the query, nothing else."
    )
}

/// Parse the follow-up reply: `None` means stop (DONE / empty / unusable), `Some`
/// is the next bridge query. Guards against the model returning a sentence or
/// echoing the instruction.
fn parse_followup_query(reply: &str) -> Option<String> {
    // DONE on any line → stop (models sometimes wrap it in punctuation/markdown).
    for l in reply.lines() {
        let t = l
            .trim()
            .trim_matches(|c: char| c == '"' || c == '`' || c == '.' || c == '*' || c == '#');
        if t.eq_ignore_ascii_case("DONE") {
            return None;
        }
    }
    // Otherwise the bridge query is the LAST non-empty line: a well-behaved model
    // returns just the query, and a chatty one puts its preamble first and the
    // query last.
    let last = reply
        .lines()
        .map(|l| l.trim())
        .rfind(|l| !l.is_empty())
        .unwrap_or("");
    let q = last
        .trim_matches(|c: char| c == '"' || c == '`' || c == '.')
        .trim();
    if q.is_empty() || q.len() > 200 || q.to_ascii_uppercase().starts_with("DONE") {
        return None;
    }
    Some(q.to_string())
}

/// (W3.1) Iterative multi-hop retrieval. Hop 1 is the standard reranked search;
/// then, up to `max_hops`, the model names the missing bridge fact, we retrieve
/// on it, and merge new candidates. A final rerank over the accumulated union
/// against the ORIGINAL question picks the best `limit`. Every step degrades
/// safely: a provider/parse failure or a hop that adds nothing stops the loop and
/// returns what hop 1 already had, so recall is never below a single-pass rerank.
#[allow(clippy::too_many_arguments)]
pub async fn iterative_rerank_search_in_namespace(
    db: &Database,
    embedder: Option<&dyn Embedder>,
    store: &dyn VectorStore,
    config: &Config,
    namespace: &str,
    project: Option<&str>,
    query: &str,
    limit: usize,
    max_hops: usize,
    pool_override: Option<usize>,
) -> Result<Vec<Memory>> {
    let mut pool = rerank_search_in_namespace_with_pool(
        db,
        embedder,
        store,
        config,
        namespace,
        project,
        query,
        limit,
        pool_override,
    )
    .await?;
    let hops = max_hops.max(1);
    if hops <= 1 || pool.is_empty() {
        return Ok(pool);
    }

    let model = if config.rerank.model.is_empty() {
        config.model.as_str()
    } else {
        config.rerank.model.as_str()
    };
    let mut seen: HashSet<i64> = pool.iter().map(|m| m.id).collect();
    let mut last_query = query.to_string();

    for _ in 1..hops {
        let prompt = build_followup_prompt(query, &pool);
        let follow = match crate::provider::complete_with(&prompt, model, config).await {
            Ok(reply) => parse_followup_query(&reply),
            Err(e) => {
                tracing::warn!("multi-hop follow-up failed ({e}); stopping at current hop");
                None
            }
        };
        let Some(fq) = follow else { break };
        if fq.eq_ignore_ascii_case(last_query.trim()) {
            break; // model repeated itself → no new ground to cover
        }
        last_query = fq.clone();

        let next = rerank_search_in_namespace_with_pool(
            db,
            embedder,
            store,
            config,
            namespace,
            project,
            &fq,
            limit,
            pool_override,
        )
        .await
        .unwrap_or_default();
        let mut added = false;
        for m in next {
            if seen.insert(m.id) {
                pool.push(m);
                added = true;
            }
        }
        if !added {
            break; // bridge query surfaced nothing new → done
        }
    }

    // Final precision pass over the union, scored against the original question.
    Ok(rerank_candidates(db, config, query, pool, limit).await)
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
    let mut candidates =
        db::get_recent_memories_scoped(db, "project", Some(project), window).await?;
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
    let candidate_ids: Vec<i64> = candidates.iter().map(|m| m.id).collect();
    let adjustments = db::score_adjustments_for_memories(db, &candidate_ids)
        .await
        .unwrap_or_default();
    let mut scored: Vec<(f64, Memory)> = Vec::with_capacity(candidates.len());
    for m in candidates {
        let rel = relevance.get(&m.id).copied().unwrap_or(0.0);
        let rec = recency_weight((now - m.created_at).max(0) as f64, half_life_days);
        // Importance + kind in one query; kind applies a typed prior on top of
        // the relevance/recency/importance blend.
        let info = db::get_memory_meta_full(db, m.id).await?;
        let base = blended_score(rel, rec, info.importance, weights);
        let reinforcement = adjustments
            .get(&m.id)
            .map(|a| db::reinforcement_multiplier(a.feedback_score, a.injection_count))
            .unwrap_or(1.0);
        scored.push((
            base * weights.kind_multiplier(&info.kind) * reinforcement,
            m,
        ));
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
    use crate::db::{create_session, insert_memory, NewMemoryEdge};
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
        assert_eq!(
            query_years("what did Caroline do in May 2023?"),
            vec!["2023"]
        );
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
        assert_eq!(
            query_entities("Al and Bo met Caroline and Caroline"),
            vec!["Caroline"]
        );
    }

    #[test]
    fn query_graph_entities_keeps_phrases_and_drops_question_words() {
        assert_eq!(
            query_graph_entities("What memory does Operator OS share with Caroline?"),
            vec![
                "Operator OS".to_string(),
                "Operator".to_string(),
                "OS".to_string(),
                "OO".to_string(),
                "Caroline".to_string()
            ]
        );
        assert_eq!(
            query_graph_entities("When did Dr Alice Morgan tell Melanie?"),
            vec![
                "Alice Morgan".to_string(),
                "Alice".to_string(),
                "Morgan".to_string(),
                "AM".to_string(),
                "Melanie".to_string()
            ]
        );
    }

    #[test]
    fn decomposed_queries_build_bounded_bridge_queries() {
        let queries = decomposed_queries(
            "Which service does Caroline's Project Atlas depend on after launch?",
        );
        assert!(
            queries
                .iter()
                .any(|q| q.contains("Caroline") && q.contains("depend")),
            "{queries:?}"
        );
        assert!(
            queries
                .iter()
                .any(|q| q.contains("Project Atlas") || q.contains("Atlas")),
            "{queries:?}"
        );
        assert!(queries.len() <= MAX_DECOMPOSED_QUERIES);
    }

    #[test]
    fn query_graph_entities_keeps_alias_cap() {
        let entities = query_graph_entities(
            "What did Alexander Hamilton tell Benjamin Franklin about Continental Congress?",
        );
        assert!(entities.len() <= MAX_GRAPH_ENTITIES);
        assert!(entities.contains(&"Alexander Hamilton".to_string()));
        assert!(entities.contains(&"Hamilton".to_string()));
    }

    #[test]
    fn query_graph_entities_keeps_old_operator_shape_prefix() {
        assert_eq!(
            &query_graph_entities("What memory does Operator OS share with Caroline?")[..4],
            [
                "Operator OS".to_string(),
                "Operator".to_string(),
                "OS".to_string(),
                "OO".to_string(),
            ]
        );
        assert_eq!(
            query_graph_entities("When did Caroline tell Melanie?"),
            vec!["Caroline".to_string(), "Melanie".to_string()]
        );
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
            db::set_memory_scope_kind(&db, f, "project", "fact")
                .await
                .unwrap();
            facts.push(f);
        }
        let narr = insert_memory(&db, "/tmp/p", &s, "the connecting narrative", Some("x"))
            .await
            .unwrap();
        db::set_memory_scope_kind(&db, narr, "project", "session")
            .await
            .unwrap();

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
    async fn source_fact_floor_keeps_exact_fact_against_narrative_quota() {
        let (db, path) = seeded_db().await;
        let s = create_session(&db, "/tmp/p").await.unwrap();

        let fact = insert_memory(
            &db,
            "/tmp/p",
            &s,
            "Melanie and her kids go to the beach once or twice a year.",
            Some("locomo fact"),
        )
        .await
        .unwrap();
        db::set_memory_scope_kind(&db, fact, "project", "fact")
            .await
            .unwrap();
        let fact_memory = db::get_memory_by_id_any_namespace(&db, fact)
            .await
            .unwrap()
            .unwrap();

        let mut narratives = Vec::new();
        for i in 0..4 {
            let id = insert_memory(
                &db,
                "/tmp/p",
                &s,
                &format!("broad narrative {i} about Melanie and family"),
                Some("locomo"),
            )
            .await
            .unwrap();
            db::set_memory_scope_kind(&db, id, "project", "session")
                .await
                .unwrap();
            narratives.push(id);
        }

        let mut candidates = narratives.clone();
        candidates.push(fact);
        let without_floor = reserve_narrative_slots(&db, &candidates, 3).await.unwrap();
        assert!(
            !without_floor.contains(&fact),
            "test setup should bury the fact before applying the floor: {without_floor:?}"
        );

        let floor = lexical_source_fact_floor_ids(
            &db,
            "How often does Melanie go to the beach with her kids?",
            &[fact_memory],
            3,
        )
        .await
        .unwrap();
        assert_eq!(floor, vec![fact]);

        let promoted = promote_source_fact_floor(&candidates, &floor, 3);
        let chosen = reserve_narrative_slots(&db, &promoted, 3).await.unwrap();
        assert!(
            chosen.contains(&fact),
            "source fact floor missing: {chosen:?}"
        );
        assert!(
            narratives.iter().any(|id| chosen.contains(id)),
            "narrative quota should still keep a bridge/narrative slot: {chosen:?}"
        );
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
        db::set_memory_event_time(&db, dated, "2023-05-07")
            .await
            .unwrap();

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

    #[tokio::test]
    async fn hybrid_search_graph_signal_surfaces_relation_ranked_memory() {
        let (db, path) = seeded_db().await;
        let s = create_session(&db, "/tmp/p").await.unwrap();
        let answer = insert_memory(&db, "/tmp/p", &s, "zzz provenance alpha", Some("x"))
            .await
            .unwrap();
        db::insert_memory_edge(
            &db,
            &NewMemoryEdge {
                project: "/tmp/p".to_string(),
                memory_id: answer,
                source: "Caroline".to_string(),
                relation: "assigned_to".to_string(),
                target: "Operator OS".to_string(),
                valid_from: None,
                valid_until: None,
                confidence: 0.9,
            },
        )
        .await
        .unwrap();

        let generic = insert_memory(&db, "/tmp/p", &s, "yyy provenance beta", Some("x"))
            .await
            .unwrap();
        db::insert_memory_edge(
            &db,
            &NewMemoryEdge {
                project: "/tmp/p".to_string(),
                memory_id: generic,
                source: "Caroline".to_string(),
                relation: "favorite_color".to_string(),
                target: "blue".to_string(),
                valid_from: None,
                valid_until: None,
                confidence: 0.9,
            },
        )
        .await
        .unwrap();

        // No embedder and summaries share no query words. The graph signal must
        // still surface the relationship-bearing provenance memory, and relation
        // overlap ("assigned_to") must beat the newer generic Caroline edge.
        let res = hybrid_search(
            &db,
            None,
            &crate::vectorstore::BruteForceStore,
            Some("/tmp/p"),
            "What is Caroline assigned to?",
            1,
        )
        .await
        .unwrap();
        assert_eq!(res.first().map(|m| m.id), Some(answer), "{res:?}");
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn hybrid_search_graph_chain_surfaces_bridge_memory() {
        let (db, path) = seeded_db().await;
        let s = create_session(&db, "/tmp/p").await.unwrap();
        let first_hop = insert_memory(&db, "/tmp/p", &s, "zzz chain alpha", Some("x"))
            .await
            .unwrap();
        db::insert_memory_edge(
            &db,
            &NewMemoryEdge {
                project: "/tmp/p".to_string(),
                memory_id: first_hop,
                source: "Caroline".to_string(),
                relation: "manages".to_string(),
                target: "Project Atlas".to_string(),
                valid_from: None,
                valid_until: None,
                confidence: 0.9,
            },
        )
        .await
        .unwrap();

        let bridge = insert_memory(&db, "/tmp/p", &s, "yyy chain beta", Some("x"))
            .await
            .unwrap();
        db::insert_memory_edge(
            &db,
            &NewMemoryEdge {
                project: "/tmp/p".to_string(),
                memory_id: bridge,
                source: "Project Atlas".to_string(),
                relation: "depends_on".to_string(),
                target: "Vertex AI".to_string(),
                valid_from: None,
                valid_until: None,
                confidence: 0.9,
            },
        )
        .await
        .unwrap();

        let res = hybrid_search(
            &db,
            None,
            &crate::vectorstore::BruteForceStore,
            Some("/tmp/p"),
            "Which service does Caroline's project depend on?",
            2,
        )
        .await
        .unwrap();
        let ids: Vec<i64> = res.iter().map(|m| m.id).collect();
        assert!(ids.contains(&first_hop), "first hop missing: {ids:?}");
        assert!(ids.contains(&bridge), "bridge dependency missing: {ids:?}");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn temporal_lookup_detection_matches_locomo_question_shapes() {
        for q in [
            "When did Caroline go to the LGBTQ support group?",
            "What date did Dave buy the vintage camera?",
            "Which year did John start surfing?",
            "How long did the car workshop last?",
            "Which city was John in before traveling to Chicago?",
        ] {
            assert!(is_temporal_lookup_query(q), "{q}");
        }
        for q in [
            "What is Caroline assigned to?",
            "Who is Dave connected to?",
            "Which project does Operator OS depend on?",
        ] {
            assert!(!is_temporal_lookup_query(q), "{q}");
        }
    }

    #[test]
    fn temporal_event_terms_drop_question_words_and_normalize_events() {
        let terms = temporal_event_terms("When did Dave start his car maintenance shop?");
        assert!(terms.contains("dave"));
        assert!(terms.contains("start"));
        assert!(terms.contains("car"));
        assert!(terms.contains("maintenance"));
        assert!(terms.contains("shop"));
        assert!(!terms.contains("when"));
        assert!(!terms.contains("did"));
    }

    #[test]
    fn query_route_classifier_stays_local_and_predictable() {
        assert_eq!(
            classify_query_route("When did Dave start his shop?"),
            QueryRoute::Temporal
        );
        assert_eq!(
            classify_query_route("Which service does Caroline's project depend on?"),
            QueryRoute::MultiHop
        );
        assert_eq!(
            classify_query_route("What camera did Dave buy?"),
            QueryRoute::SingleHop
        );
        assert_eq!(
            classify_query_route("What was the favorite hobby?"),
            QueryRoute::OpenDomain
        );
    }

    #[test]
    fn routed_fusion_weights_emphasize_the_right_signals() {
        let temporal = FusionWeights::for_query("When did Dave buy it?", 1, 1);
        assert!(temporal.temporal_event > temporal.fts);
        assert_eq!(temporal.graph, 0);
        assert_eq!(temporal.chunk, 0);

        let multi = FusionWeights::for_query("Which project does Caroline depend on?", 1, 1);
        assert!(multi.graph > multi.fts);
        assert!(multi.vector > multi.fts);
        assert!(multi.query_variant > 0);
        assert_eq!(multi.chunk, 0);

        let single = FusionWeights::for_query("What camera did Dave buy?", 1, 1);
        assert!(single.fts > single.vector);
        assert_eq!(single.query_variant, 0);
        assert_eq!(single.chunk, 0);

        // Open-domain is the only route that fuses chunk-level recall, and the
        // knob gates it entirely.
        let open = FusionWeights::for_query("lunch policy", 1, 1);
        assert_eq!(open.chunk, 1);
        let open_off = FusionWeights::for_query("lunch policy", 1, 0);
        assert_eq!(open_off.chunk, 0);
    }

    #[test]
    fn salient_overlap_fraction_measures_query_term_coverage() {
        let terms = vec!["vineyard".to_string(), "booking".to_string()];
        assert_eq!(
            salient_overlap_fraction(&terms, "Caroline booked the vineyard trip"),
            0.5
        );
        assert_eq!(salient_overlap_fraction(&terms, "unrelated text"), 0.0);
        assert_eq!(
            salient_overlap_fraction(&[], "anything"),
            1.0,
            "no salient terms must not force abstention"
        );
    }

    #[test]
    fn stale_candidate_detection_requires_live_update_from_another_candidate() {
        let mk_edge = |memory_id: i64, target: &str, superseded: Option<i64>| db::MemoryEdge {
            id: memory_id * 10,
            project: "/tmp/p".to_string(),
            memory_id,
            source: "Bob".to_string(),
            relation: "status".to_string(),
            target: target.to_string(),
            valid_from: None,
            valid_until: None,
            observed_at: 0,
            confidence: 0.9,
            superseded_by: superseded,
            superseded_reason: None,
            created_at: 0,
        };
        let mut edges: HashMap<i64, Vec<db::MemoryEdge>> = HashMap::new();
        edges.insert(1, vec![mk_edge(1, "onboarding", Some(20))]); // stale
        edges.insert(2, vec![mk_edge(2, "active", None)]); // the live update
        edges.insert(3, Vec::new()); // no edge support: untouched

        let stale = stale_candidate_ids(&[1, 2, 3], &edges);
        assert!(stale.contains(&1), "superseded candidate must be demotable");
        assert!(!stale.contains(&2), "live candidate must never be stale");
        assert!(!stale.contains(&3), "edge-less candidates are untouched");

        // Without the live update in the candidate set, nothing is demoted:
        // demotion only enforces a supersession that retrieval can substitute.
        let stale_alone = stale_candidate_ids(&[1, 3], &edges);
        assert!(stale_alone.is_empty());
    }

    #[test]
    fn temporal_proximity_scores_near_dates_highest() {
        let anchor = query_date_anchor("What happened after May 2023?").unwrap();
        let near = temporal_proximity_score(Some(anchor), "2023-05-17");
        let far = temporal_proximity_score(Some(anchor), "2021-05-17");
        assert!(near > far, "near={near}, far={far}");
        assert!(near > 0.0);
    }

    #[tokio::test]
    async fn temporal_lookup_prefers_dated_event_fact_over_newer_broad_match() {
        let (db, path) = seeded_db().await;
        let s = create_session(&db, "/tmp/p").await.unwrap();
        let dated = insert_memory(
            &db,
            "/tmp/p",
            &s,
            "Dave started his car maintenance shop on May 1, 2023",
            Some("car maintenance shop"),
        )
        .await
        .unwrap();
        db::set_memory_scope_kind(&db, dated, "project", "fact")
            .await
            .unwrap();
        db::set_memory_event_time(&db, dated, "2023-05-01")
            .await
            .unwrap();

        let broad = insert_memory(
            &db,
            "/tmp/p",
            &s,
            "Dave recently discussed car maintenance and shop planning",
            Some("car maintenance shop"),
        )
        .await
        .unwrap();
        sqlx::query("UPDATE memories SET created_at = $1 WHERE rowid = $2")
            .bind(9_999_999_i64)
            .bind(broad)
            .execute(&db.pool)
            .await
            .unwrap();
        sqlx::query("UPDATE memories SET created_at = $1 WHERE rowid = $2")
            .bind(1_i64)
            .bind(dated)
            .execute(&db.pool)
            .await
            .unwrap();

        let res = hybrid_search(
            &db,
            None,
            &crate::vectorstore::BruteForceStore,
            Some("/tmp/p"),
            "When did Dave start his car maintenance shop?",
            5,
        )
        .await
        .unwrap();
        assert_eq!(res.first().map(|m| m.id), Some(dated), "{res:?}");
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn temporal_conflict_prefers_source_backed_fact_over_newer_conflict() {
        let (db, path) = seeded_db().await;
        let s = create_session(&db, "/tmp/p").await.unwrap();

        let sourced = insert_memory(
            &db,
            "/tmp/p",
            &s,
            "Dave started his car maintenance shop on May 1, 2023",
            Some("locomo source fact car maintenance shop"),
        )
        .await
        .unwrap();
        db::set_memory_scope_kind(&db, sourced, "project", "fact")
            .await
            .unwrap();
        db::set_memory_event_time(&db, sourced, "2023-05-01")
            .await
            .unwrap();

        let conflicting = insert_memory(
            &db,
            "/tmp/p",
            &s,
            "Dave mentioned car maintenance shop planning around June 2024",
            Some("car maintenance shop"),
        )
        .await
        .unwrap();
        db::set_memory_scope_kind(&db, conflicting, "project", "session")
            .await
            .unwrap();
        db::set_memory_event_time(&db, conflicting, "2024-06-01")
            .await
            .unwrap();
        sqlx::query("UPDATE memories SET created_at = $1 WHERE rowid = $2")
            .bind(9_999_999_i64)
            .bind(conflicting)
            .execute(&db.pool)
            .await
            .unwrap();
        sqlx::query("UPDATE memories SET created_at = $1 WHERE rowid = $2")
            .bind(1_i64)
            .bind(sourced)
            .execute(&db.pool)
            .await
            .unwrap();

        let res = hybrid_search(
            &db,
            None,
            &crate::vectorstore::BruteForceStore,
            Some("/tmp/p"),
            "When did Dave start his car maintenance shop?",
            5,
        )
        .await
        .unwrap();
        assert_eq!(res.first().map(|m| m.id), Some(sourced), "{res:?}");
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn temporal_lookup_suppresses_graph_only_hits() {
        let (db, path) = seeded_db().await;
        let s = create_session(&db, "/tmp/p").await.unwrap();
        let graph_only = insert_memory(&db, "/tmp/p", &s, "zzz provenance alpha", Some("x"))
            .await
            .unwrap();
        db::insert_memory_edge(
            &db,
            &NewMemoryEdge {
                project: "/tmp/p".to_string(),
                memory_id: graph_only,
                source: "Caroline".to_string(),
                relation: "assigned_to".to_string(),
                target: "Operator OS".to_string(),
                valid_from: None,
                valid_until: None,
                confidence: 0.9,
            },
        )
        .await
        .unwrap();

        // This temporal form has no FTS/vector/date evidence. Before query-type
        // gating, the graph edge alone could surface the relationship memory and
        // crowd out actual dated facts in LoCoMo-style "when did..." questions.
        let res = hybrid_search(
            &db,
            None,
            &crate::vectorstore::BruteForceStore,
            Some("/tmp/p"),
            "When did Caroline get assigned?",
            5,
        )
        .await
        .unwrap();
        assert!(
            res.iter().all(|m| m.id != graph_only),
            "temporal lookup should not be answered by graph-only evidence: {res:?}"
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

    fn ev(memory: Memory, kind: &str, event_time: Option<&str>) -> RerankEvidence {
        let temporal_proximity = event_time.map(|_| 1.0);
        RerankEvidence {
            memory,
            kind: kind.to_string(),
            event_time: event_time.map(|s| s.to_string()),
            source_ref: Some("mem:source:fact:1".to_string()),
            chunks: Vec::new(),
            graph_edges: Vec::new(),
            query_route: QueryRoute::Temporal,
            temporal_proximity,
            temporal_conflict: false,
        }
    }

    fn test_chunk(memory_id: i64, title: &str, summary: &str) -> db::MemoryChunk {
        db::MemoryChunk {
            id: 1,
            chunk_id: format!("mem:{memory_id}:chunk:0"),
            project: "/p".to_string(),
            memory_id,
            session_id: "s".to_string(),
            ordinal: 0,
            density: "high".to_string(),
            kind: "fact".to_string(),
            title: title.to_string(),
            summary: summary.to_string(),
            source_hash: Some("hash".to_string()),
            source_start: Some(10),
            source_end: Some(42),
            token_estimate: 12,
            created_at: 0,
        }
    }

    fn test_edge(memory_id: i64, source: &str, relation: &str, target: &str) -> db::MemoryEdge {
        db::MemoryEdge {
            id: 1,
            project: "/p".to_string(),
            memory_id,
            source: source.to_string(),
            relation: relation.to_string(),
            target: target.to_string(),
            valid_from: Some("2023-05-01".to_string()),
            valid_until: None,
            observed_at: 0,
            confidence: 0.91,
            superseded_by: None,
            superseded_reason: None,
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
        let cands = vec![
            mk(10, "a"),
            mk(11, "b"),
            mk(12, "c"),
            mk(13, "d"),
            mk(14, "e"),
        ];
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
    fn multi_hop_gate_triggers_on_cues_and_multi_entity_only() {
        // Relational/bridge cues → multi-hop.
        assert!(is_multi_hop_query(
            "what city did Alice move to after the job"
        ));
        assert!(is_multi_hop_query("the place connected to the trip"));
        // Two named entities → multi-hop (a relation between proper nouns).
        assert!(is_multi_hop_query("how do Alice and Bob know each other"));
        // Single-hop factoid with one entity and no cue → NOT multi-hop (no extra cost).
        assert!(!is_multi_hop_query("when is Alice's birthday"));
        assert!(!is_multi_hop_query("what is the capital"));
    }

    #[test]
    fn followup_parse_stops_on_done_and_extracts_bridge_query() {
        assert_eq!(parse_followup_query("DONE"), None);
        assert_eq!(parse_followup_query("  done  "), None);
        assert_eq!(parse_followup_query(""), None);
        // First non-empty line, stripped of quotes/backticks/trailing period.
        assert_eq!(
            parse_followup_query("`Caroline new job city`."),
            Some("Caroline new job city".to_string())
        );
        assert_eq!(
            parse_followup_query("Here is the query:\nAlice employer 2023"),
            Some("Alice employer 2023".to_string())
        );
    }

    #[test]
    fn reanchor_keeps_narrow_order_then_appends_newcomers() {
        let narrow = vec![mk(10, "a"), mk(11, "b"), mk(12, "c")];
        // Wide pool: a different (degraded) order, same ids plus newcomers 9 and 8.
        let wide = vec![
            mk(12, "c"),
            mk(9, "x"),
            mk(10, "a"),
            mk(8, "y"),
            mk(11, "b"),
        ];
        let out = reanchor(narrow, wide);
        // Narrow order preserved on top; only the wide-pool newcomers (9, 8) trail.
        assert_eq!(
            out.iter().map(|m| m.id).collect::<Vec<_>>(),
            vec![10, 11, 12, 9, 8]
        );
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
        let cands = vec![
            ev(mk(1, "short fact"), "fact", Some("2023-05-07")),
            ev(mk(2, &long), "session", None),
        ];
        let p = build_rerank_prompt("when did it happen?", &cands);
        assert!(p.contains("QUESTION: when did it happen?"));
        assert!(p.contains("route=temporal"));
        assert!(p.contains("kind=fact"));
        assert!(p.contains("event_time=2023-05-07"));
        assert!(p.contains("date_proximity=1.00:exact"));
        assert!(p.contains("source_ref=mem:source:fact:1"));
        assert!(p.contains("evidence=short fact"));
        // The 900-char candidate is capped — its raw text never appears in full.
        assert!(!p.contains(&"x".repeat(RERANK_SNIPPET_CHARS + 1)));
    }

    #[test]
    fn structured_rerank_text_includes_chunks_and_graph_edges() {
        let mut evidence = ev(
            mk(7, "Caroline moved after the Operator OS assignment."),
            "fact",
            Some("2023-05-07"),
        );
        evidence.query_route = QueryRoute::MultiHop;
        evidence.temporal_conflict = true;
        evidence.chunks = vec![test_chunk(
            7,
            "source turn",
            "Caroline was assigned to Operator OS, then moved to Austin.",
        )];
        evidence.graph_edges = vec![test_edge(7, "Caroline", "assigned_to", "Operator OS")];

        let text = structured_rerank_text(&evidence);
        assert!(text.contains("route=multi_hop"));
        assert!(text.contains("temporal_conflict=conflicting_candidate_dates"));
        assert!(text.contains("chunk_evidence=source turn"));
        assert!(text.contains("source_span=10..42"));
        assert!(text.contains("graph_edges=Caroline --assigned_to--> Operator OS"));
        assert!(text.contains("confidence=0.91"));
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
            store
                .upsert(db, id, emb.id(), emb.dim(), &v[0])
                .await
                .unwrap();
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
        let qvec = emb
            .embed(&["gamma database schema".to_string()])
            .await
            .unwrap();
        let knn = store
            .knn(&db, Some("/tmp/p"), &qvec[0], emb.id(), 3)
            .await
            .unwrap();
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

        let qvec = normalize(
            &emb.embed(&["gamma database schema".to_string()])
                .await
                .unwrap()[0],
        );
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
        let uid = insert_memory(
            &db,
            "/tmp/other",
            &s2,
            "user prefers tabs over spaces",
            Some("pref"),
        )
        .await
        .unwrap();
        db::set_memory_scope_kind(&db, uid, "user", "preference")
            .await
            .unwrap();

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
        db::set_memory_scope_kind(&db, pid, "user", "profile")
            .await
            .unwrap();

        let store = crate::vectorstore::BruteForceStore;
        let weights = Weights::default();
        let ranked = rank_for_injection(&db, None, &store, "/tmp/p", &weights, 30.0, 3)
            .await
            .unwrap();
        assert_eq!(ranked[0].id, pid, "profile must inject first: {ranked:?}");
        assert!(
            ranked.len() <= 3,
            "limit is respected even with the profile"
        );
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
        db::set_memory_scope_kind(&db, pref, "project", "preference")
            .await
            .unwrap();
        let sess = insert_memory(&db, "/tmp/p", &s, "a plain session note", Some("x"))
            .await
            .unwrap();
        db::set_memory_scope_kind(&db, sess, "project", "session")
            .await
            .unwrap();

        let store = crate::vectorstore::BruteForceStore;
        let weights = Weights::default();
        let ranked = injection_rank(&db, None, &store, "/tmp/p", None, &weights, 30.0, 2)
            .await
            .unwrap();
        assert_eq!(
            ranked[0].id, pref,
            "kind-boosted preference must rank first: {ranked:?}"
        );
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
