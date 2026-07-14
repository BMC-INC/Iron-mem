//! Governance cost instrumentation (paper RQ5: utility–latency trade-off).
//!
//! IronMem claims governance overhead is justified in regulated sectors. That is
//! an assertion until the overhead is a published number. This module records the
//! latency of each governance operation with near-zero overhead (lock-free
//! atomics) and surfaces per-op `count / avg_us / max_us` on `/status`, so a
//! governed write or governed delete carries a measured cost, not a hand-wave.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// The governance operations whose cost we publish.
#[derive(Debug, Clone, Copy)]
pub enum GovOp {
    /// `MemoryGovernance::validate` — the consent/classification gate.
    ConsentCheck,
    /// `parse_trust_tier` — trust-tier resolution from input.
    TrustEval,
    /// `normalize_namespace` — namespace authority resolution.
    NamespaceResolve,
    /// The full governed `memory_meta` write (the INSERT/UPSERT).
    GovernedWrite,
    /// Governed delete: the tombstone UPDATE.
    TombstoneWrite,
}

impl GovOp {
    const COUNT: usize = 5;

    fn idx(self) -> usize {
        match self {
            GovOp::ConsentCheck => 0,
            GovOp::TrustEval => 1,
            GovOp::NamespaceResolve => 2,
            GovOp::GovernedWrite => 3,
            GovOp::TombstoneWrite => 4,
        }
    }

    fn label(idx: usize) -> &'static str {
        match idx {
            0 => "consent_check",
            1 => "trust_eval",
            2 => "namespace_resolve",
            3 => "governed_write",
            4 => "tombstone_write",
            _ => "unknown",
        }
    }
}

struct OpStat {
    count: AtomicU64,
    total_nanos: AtomicU64,
    max_nanos: AtomicU64,
}

impl OpStat {
    const fn new() -> Self {
        Self {
            count: AtomicU64::new(0),
            total_nanos: AtomicU64::new(0),
            max_nanos: AtomicU64::new(0),
        }
    }
}

// One fixed slot per GovOp — no allocation, no lock, no map lookup on the hot path.
static STATS: [OpStat; GovOp::COUNT] = [
    OpStat::new(),
    OpStat::new(),
    OpStat::new(),
    OpStat::new(),
    OpStat::new(),
];

/// Record a single observation of `op` taking `dur`.
pub fn record(op: GovOp, dur: Duration) {
    let s = &STATS[op.idx()];
    let nanos = dur.as_nanos().min(u64::MAX as u128) as u64;
    s.count.fetch_add(1, Ordering::Relaxed);
    s.total_nanos.fetch_add(nanos, Ordering::Relaxed);
    // Monotonic max via CAS loop.
    let mut cur = s.max_nanos.load(Ordering::Relaxed);
    while nanos > cur {
        match s
            .max_nanos
            .compare_exchange_weak(cur, nanos, Ordering::Relaxed, Ordering::Relaxed)
        {
            Ok(_) => break,
            Err(prev) => cur = prev,
        }
    }
}

/// Time a synchronous closure and record it under `op`, returning its value.
pub fn timed<T>(op: GovOp, f: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let out = f();
    record(op, start.elapsed());
    out
}

/// Start handle for async / multi-statement ops a closure can't wrap.
/// Pair with [`record`]: `let t = metrics::start(); … ; metrics::record(op, t.elapsed());`
pub fn start() -> Instant {
    Instant::now()
}

fn round3(us: f64) -> f64 {
    (us * 1000.0).round() / 1000.0
}

/// Retrieval pipeline tiers (memory-leadership roadmap Phase 1): which stage
/// resolved a query and what each costs. Published so "most queries resolve
/// without LLM calls" is a measured number, not a claim.
#[derive(Debug, Clone, Copy)]
pub enum RetrievalTier {
    /// Lexical T0 early exit: FTS+graph resolved the query, deeper signals skipped.
    T0LexicalExit,
    /// Full RRF fusion over every collected signal (the T1 default).
    FullFusion,
    /// Cross-encoder rerank accepted (T2).
    RerankCrossEncoder,
    /// Cross-encoder margin too low → escalated to the LLM reranker (T3).
    RerankEscalated,
    /// LLM rerank ran directly (no cross-encoder available/selected).
    RerankLlm,
}

impl RetrievalTier {
    const COUNT: usize = 5;

    fn idx(self) -> usize {
        match self {
            RetrievalTier::T0LexicalExit => 0,
            RetrievalTier::FullFusion => 1,
            RetrievalTier::RerankCrossEncoder => 2,
            RetrievalTier::RerankEscalated => 3,
            RetrievalTier::RerankLlm => 4,
        }
    }

    fn label(idx: usize) -> &'static str {
        match idx {
            0 => "t0_lexical_exit",
            1 => "full_fusion",
            2 => "rerank_cross_encoder",
            3 => "rerank_escalated",
            4 => "rerank_llm",
            _ => "unknown",
        }
    }
}

static TIER_STATS: [OpStat; RetrievalTier::COUNT] = [
    OpStat::new(),
    OpStat::new(),
    OpStat::new(),
    OpStat::new(),
    OpStat::new(),
];

/// Record one query resolving at `tier` after `dur`.
pub fn record_tier(tier: RetrievalTier, dur: Duration) {
    let s = &TIER_STATS[tier.idx()];
    let nanos = dur.as_nanos().min(u64::MAX as u128) as u64;
    s.count.fetch_add(1, Ordering::Relaxed);
    s.total_nanos.fetch_add(nanos, Ordering::Relaxed);
    let mut cur = s.max_nanos.load(Ordering::Relaxed);
    while nanos > cur {
        match s
            .max_nanos
            .compare_exchange_weak(cur, nanos, Ordering::Relaxed, Ordering::Relaxed)
        {
            Ok(_) => break,
            Err(prev) => cur = prev,
        }
    }
}

fn stat_json(s: &OpStat) -> serde_json::Value {
    let count = s.count.load(Ordering::Relaxed);
    let total = s.total_nanos.load(Ordering::Relaxed);
    let max = s.max_nanos.load(Ordering::Relaxed);
    let avg_us = if count > 0 {
        (total as f64 / count as f64) / 1000.0
    } else {
        0.0
    };
    serde_json::json!({
        "count": count,
        "avg_us": round3(avg_us),
        "max_us": round3(max as f64 / 1000.0),
    })
}

/// JSON snapshot for `/status`: per governance op and per retrieval tier
/// `{count, avg_us, max_us}`.
pub fn snapshot() -> serde_json::Value {
    let mut ops = serde_json::Map::new();
    for (i, s) in STATS.iter().enumerate() {
        ops.insert(GovOp::label(i).to_string(), stat_json(s));
    }
    let mut tiers = serde_json::Map::new();
    for (i, s) in TIER_STATS.iter().enumerate() {
        tiers.insert(RetrievalTier::label(i).to_string(), stat_json(s));
    }
    ops.insert(
        "retrieval_tiers".to_string(),
        serde_json::Value::Object(tiers),
    );
    serde_json::Value::Object(ops)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_count_avg_and_max() {
        // Use a dedicated op so the test is order-independent within the suite.
        record(GovOp::TombstoneWrite, Duration::from_micros(10));
        record(GovOp::TombstoneWrite, Duration::from_micros(30));
        let snap = snapshot();
        let tomb = &snap["tombstone_write"];
        assert!(tomb["count"].as_u64().unwrap() >= 2);
        assert!(tomb["max_us"].as_f64().unwrap() >= 30.0);
    }

    #[test]
    fn timed_returns_inner_value() {
        let v = timed(GovOp::TrustEval, || 7 + 1);
        assert_eq!(v, 8);
    }

    #[test]
    fn retrieval_tiers_surface_in_snapshot() {
        record_tier(RetrievalTier::T0LexicalExit, Duration::from_micros(5));
        record_tier(RetrievalTier::FullFusion, Duration::from_micros(50));
        let snap = snapshot();
        let tiers = &snap["retrieval_tiers"];
        assert!(tiers["t0_lexical_exit"]["count"].as_u64().unwrap() >= 1);
        assert!(tiers["full_fusion"]["count"].as_u64().unwrap() >= 1);
        assert!(tiers["rerank_escalated"]["count"].as_u64().is_some());
    }
}
