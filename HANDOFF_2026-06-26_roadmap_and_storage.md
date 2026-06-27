# HANDOFF — IronMem roadmap build (resume after compact)

> Snapshot 2026-06-26 evening PDT. Read this top section first, then go straight to
> "THE REMAINING BUILD". Goal this session: build ALL of #4 (storage adapters,
> hybrid) tonight. No deferring.

## STATUS UPDATE (2026-06-26 late) — #4 DONE
**#4 storage adapters is COMPLETE (P1–P4), committed `cccaf95` (NOT pushed).**
- P1: `src/storage.rs` `StorageBackend` trait + `NativeBackend`; `retrieval.rs`
  hybrid + graph + temporal helpers repointed through the trait. Behavior-identical
  (same db/store calls); 178 tests green, clippy clean; DEPLOYED to the live binary
  (backup `ironmem.bak-pre-storage-20260626`) and live-funnel-verified, no regression.
- P2: backend-agnostic governance conformance suite (consent gate ENFORCED before
  write, namespace isolation, tombstone-hides, ledger hash-chain + tamper-evidence,
  trust-tier fidelity, vector/graph seams). Green on native. §4 answer for native: ENFORCE.
- P3/P4: REAL external adapters, committed `b2ce833`. `QdrantVectorBackend`
  (embeddings via Qdrant REST) and `Neo4jGraphBackend` (entities/edges via Neo4j
  transactional Cypher HTTP), both over the existing `reqwest` dep (no new crate).
  Mem0/Zep split: content + governance stay native (local i64 spine), only the
  vector index / edge store is external. Native stays the deployed default;
  these are config-selectable (marked allow(dead_code) until server wiring).
  Proven against LIVE services by env-gated integration tests running the SAME
  conformance suite — 180 tests green, clippy clean. The in-process adapters
  remain as always-on (no-service) generalization proofs.
- Live services were brought up in Docker to prove the adapters:
  `docker run -d --name ironmem-qdrant -p 6333:6333 qdrant/qdrant:latest`
  `docker run -d --name ironmem-neo4j -p 7474:7474 -p 7687:7687 -e NEO4J_AUTH=neo4j/ironmempass neo4j:5`
  Run the integration tests with:
  `IRONMEM_TEST_QDRANT_URL=http://localhost:6333 IRONMEM_TEST_NEO4J_URL=http://localhost:7474 IRONMEM_TEST_NEO4J_PASS=ironmempass cargo test --release --features local-onnx conformance::`
  (containers may still be running; `docker rm -f ironmem-qdrant ironmem-neo4j` to stop.)
- Remaining productionization (not blocking): wire a config selector
  (IRONMEM_STORAGE_VECTOR_BACKEND=qdrant etc.) so the server can construct an
  external backend, plus a backfill to migrate existing embeddings/edges; native
  default is intentionally unchanged to protect the live 28k-memory store.
- OPEN: push `d8c648d` + `cccaf95` + `b2ce833` + this handoff to origin/main (needs James's OK).

Everything below this line is the original pre-build handoff, kept for context.

## One-line state
Path-to-70 retrieval batch is BUILT, tested, clippy-clean, DEPLOYED to the live
binary, and committed locally (`d8c648d`, NOT pushed). The only roadmap item left
is **#4 storage adapters**, decided as **HYBRID**. Build P1 through P4 this session.

## RESUME HERE (first action after compact)
1. Confirm live binary healthy: `curl -s localhost:37778/status` shows
   `governance_cost` and ~28,411 memories.
2. Start #4 **P1** immediately (see plan below): `StorageBackend` trait + native
   adapter + repoint retrieval call-sites + parity check. Then P2, P3, P4.

---

## What shipped tonight (deployed + smoke-verified live, commit d8c648d)
All are quality-hardening of code IronMem already owns (no new engine).

| Item | File / fn | Change |
|---|---|---|
| W1.2 pool 3x to 5x | retrieval.rs `hybrid_search_in_namespace` (~:554) | `(limit*5).max(50)` |
| W1.3 context order + dedup | server.rs `get_context`, `u_shaped_order`, `dedup_by_summary` | U-shaped order + near-dup filter on ranked results |
| #1 governed retrieval router | governance.rs `tier_authority_boost`, db.rs `trust_tiers_for`, retrieval.rs `apply_tier_boost` | writer trust-tier authority at query time; config `governance_router.weight=0.05`; env `IRONMEM_GOVERNANCE_ROUTER_WEIGHT` |
| #5 temporal-trust ON | config.rs `default_trust_weight` 0.0 to 0.05 | env `IRONMEM_TEMPORAL_TRUST_WEIGHT`; wiring (apply_trust_boost) already existed |
| W3.3 dates as fields | db.rs `event_times_for`, server.rs ContextResponse `event_times` | `/context` exposes event dates per memory id |
| W3.1 iterative multi-hop | retrieval.rs `iterative_rerank_search_in_namespace`, `is_multi_hop_query`, `build_followup_prompt`, `parse_followup_query` | capped retrieve to reason to re-query loop, safe fallback to hop-1; gated by `is_multi_hop_query` + `config.multi_hop`; env `IRONMEM_MULTI_HOP_ENABLED` |

Already built before tonight (verified by survey, no work needed): W1.1
recall-preserving rerank (`fuse_rerank` RRF union), W3.2 graph-edge traversal
(`graph_ids_for_query`/`graph_edge_score`), #3 cost (`governance_cost` on
`/status`), Track B synthesis (`reflection.rs::synthesize`), W3.3 schema +
invalidate-don't-delete supersession.

Defaults note: live `~/.ironmem/settings.json` has rerank OFF and NO
`temporal_trust`/`governance_router`/`multi_hop` blocks, so the new code defaults
apply. #5 and #1 boosts are small (0.05, reorder near-ties only). Multi-hop only
fires when rerank is on (so dormant in daily use; active in benchmark/explicit
rerank calls). Off-switches: the three env vars above set to 0.

## Build / deploy / verify (copy-paste)
```bash
cd ~/Projects/Iron-mem-fix
cargo build --release --features local-onnx     # local-onnx is REQUIRED (fastembed)
cargo test  --release --features local-onnx     # 175 pass, 1 ignored
cargo clippy --release --features local-onnx    # clean
cp target/release/ironmem ~/.ironmem/bin/ironmem
launchctl kickstart -k gui/$(id -u)/com.execlayer.ironmem
curl -s localhost:37778/status                  # expect governance_cost + ~28411 memories
```
Backup of pre-batch live binary: `~/.ironmem/bin/ironmem.bak-pre-roadmap-20260626`
(instant revert: cp it back + kickstart). Auth token for `/context` etc:
`94bda8a4-c39d-4b47-9006-d2f2b92ba216` (Bearer).

## Locations / gotchas
- Rust source: `~/Projects/Iron-mem-fix`, git `main`, HEAD `d8c648d` (the batch),
  parent `8547836`. Remote github.com/BMC-INC/Iron-mem. Push only with James's OK.
- `phase1_provider_DRAFT.patch` is untracked on purpose (marginal effect). Leave it.
- Vertex ADC expires ~24h; org blocks SA keys, so re-auth before any Vertex run:
  `gcloud auth application-default login`. Project queueflow-sentinel.
- Benchmark repo `~/Projects/ironmem-locomo-benchmark` (local-only, no remote);
  the 2x2 result (pool100/k25 = 63.25% Flash) is committed there as `3d7ef85`.
- User wants NO more benchmark runs until all build work is done. No em dashes in prose.

---

## THE REMAINING BUILD: #4 storage adapters (HYBRID). Build ALL tonight.

**Decision locked: HYBRID.** One `StorageBackend` trait. IronMem's native engine
is the default adapter (keeps the funnel gains and full enforcement). Governance
(ledger, consent, tombstone, trust, namespace) stays ABOVE the trait so it applies
to any backend. External backends become optional adapters that IronMem governs
(observe-tiered). Do NOT pick shim-vs-native POSITIONING now: build the trait,
decide positioning at P2 when enforcement reality is known. Spec:
`~/Projects/ironmem-locomo-benchmark/IRONMEM_STORAGE_ADAPTER_SPEC.md`.

### Trait (from spec §3, finalize at P1)
```rust
#[async_trait]
pub trait StorageBackend: Send + Sync {
    fn capabilities(&self) -> BackendCaps; // {vector, graph, fulltext, native_namespacing}
    async fn put_memory(&self, rec: &BackendMemory) -> Result<BackendId>;
    async fn get_memory(&self, id: &BackendId) -> Result<Option<BackendMemory>>;
    async fn vector_search(&self, ns: &Namespace, q: &Embedding, k: usize) -> Result<Vec<Candidate>>;
    async fn fulltext_search(&self, ns: &Namespace, q: &str, k: usize) -> Result<Vec<Candidate>>;
    async fn put_edge(&self, e: &BackendEdge) -> Result<()>;
    async fn neighbors(&self, ns: &Namespace, entity: &str, hops: u8) -> Result<Vec<BackendEdge>>;
    async fn hide(&self, id: &BackendId, reason: TombstoneReason) -> Result<()>; // governed delete
    async fn purge(&self, id: &BackendId) -> Result<()>;                          // hard delete
}
```

### P1: trait + native adapter + repoint + parity
1. New module (e.g. `src/storage.rs`): the trait above + `BackendCaps`,
   `Candidate`, `BackendMemory`, `BackendEdge`, `BackendId` (wrap existing types).
2. `NativeBackend` implementing the trait by delegating to the existing `db::` and
   vectorstore functions. The logic already exists; the adapter just wraps it.
3. Repoint these retrieval.rs call-sites to go through the trait (the storage seams):
   - `db::search_memories_in_namespace` / `db::search_all_memories_in_namespace` (FTS, ~:557)
   - `store.knn` (vector, ~:566)
   - `db::memories_by_event_time` (~:589)
   - `db::memories_for_entity` (~:620, entity signal currently off)
   - `temporal_event_ids_for_query` (~:607) and `graph_ids_for_query` (~:641) internals
   - `db::get_memory_by_id_in_namespace` (~:699)
   Keep `hybrid_search_in_namespace` shape; only swap the data source.
4. PARITY GATE (the one real check): rebuild, run the funnel
   (`~/Projects/ironmem-locomo-benchmark/scripts/funnel_probe.py`, `--store-limit
   2000`) and/or a Flash scored pass on the native backend; confirm no regression
   vs the deployed numbers. This is a no-regression guard, not score-chasing.

### P2: governance conformance suite
Port the governance invariants to run against ANY `StorageBackend`: consent gate,
namespace isolation (no cross-namespace leakage), tombstone-hides-from-retrieval,
ledger hash-chain continuity, trust-tier priority. Gate: native adapter passes 100%.
This suite is the real deliverable (it proves governance generalizes). At P2,
make the shim-vs-native positioning call using what enforcement actually allows.

### P3: vector adapter (Qdrant / Mem0-style)
External vector store for content + embeddings; IronMem keeps governance + ledger
local. Gate: conformance suite passes; vector recall within noise of native.
Genuine unknown here is the backend API surface + enforcement reality (prevent vs
observe), not time. Document the guarantee tier honestly.

### P4: graph adapter (Zep / Graphiti-style)
External entity/edge store; one-hop bridge retrieval still works. Gate: conformance
suite passes.

### §4 crux (answer per adapter, do not paper over)
- enforce vs observe: can the adapter PREVENT a non-consented PHI write, or only
  record it? Tier the promise by backend (full on native, best-effort on external).
- tombstone honesty: `hide()` must filter at query time; document bytes persist
  until `purge()` if the backend has no soft-delete.
- ledger / backend divergence: ledger-after-ack + periodic divergence check.
- namespace authority: map IronMem namespaces to the backend's isolation; prove no
  cross-namespace leakage in the conformance suite.

### Acceptance for #4
One trait, native default adapter (P1) + at least the vector adapter (P3) and graph
adapter (P4), with the governance conformance suite (P2) green against each.

## Also outstanding (tonight)
- Decide push of `d8c648d` (and this handoff) to origin/main: needs James's OK.
- Optional: fold this doc's "what shipped" into a RUN_NOTE if you want an immutable
  record separate from the handoff.
