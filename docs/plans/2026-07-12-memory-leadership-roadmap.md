# IronMem Memory Leadership Roadmap

**Date:** 2026-07-12
**Goal:** Make IronMem the best-scoring memory tool in the industry — with embedded governance as the moat no competitor can copy quickly.
**Priority (confirmed):** benchmark scores first; governance productization rides alongside, ecosystem last.

---

## 1. Where the market actually is (calibrated, not vendor-marketing)

### 1.1 The headline numbers — and why most of them are inflated

| System | Claimed score | Benchmark | Caveat |
|---|---|---|---|
| OMEGA | 95.4% | LongMemEval | vendor-run, own pipeline/judge |
| Mastra Observational Memory | 94.87% | LongMemEval | vendor-run (gpt-5-mini answerer) |
| Mem0 | 93.4% | LongMemEval | vendor-run (their published *paper* LoCoMo number is ~66.9%) |
| ByteRover 2.0 | 92.2% LoCoMo / 92.8% LongMemEval | both | vendor-run, Gemini 3 Pro justification stage |
| Zep | 75.14% | LoCoMo | disputed — Mem0's independent rerun measured 58.44% |
| **IronMem** | **68.4%** | **LoCoMo** | reproducible public harness, Gemini 2.5 Pro judge, 1,540 Qs |

**Key calibration facts (verified against multiple sources):**

- **There is no standardized pipeline.** Every vendor uses its own ingestion, answer prompts, answer model, and judge. Scores in the same comparison table often aren't comparable at all ([The Benchmark Theatre](https://essays.bloo-mind.ai/posts/2026-05-20-mem-eval/)).
- **LoCoMo itself is partly broken.** An independent audit ([Penfield Labs](https://dev.to/penfieldlabs/we-audited-locomo-64-of-the-answer-key-is-wrong-and-the-judge-accepts-up-to-63-of-intentionally-33lg)) found 6.4% of the answer key is wrong (hallucinated facts, bad temporal reasoning, speaker misattribution) and the standard gpt-4o-mini judge accepts up to ~63% of intentionally wrong-but-topically-adjacent answers. Honest ceiling ≈ 93–94%.
- **The Zep↔Mem0 war shows how soft the numbers are.** Zep claimed 84% on LoCoMo; Mem0 [showed](https://github.com/getzep/zep-papers/issues/5) the calculation included the excluded adversarial category in the numerator but not the denominator (~25pp inflation) and measured 58.44% ± 0.20 on a corrected rerun; Zep [countered](https://blog.getzep.com/lies-damn-lies-statistics-is-mem0-really-sota-in-agent-memory/) that Mem0 misconfigured Zep and claims 75.14%. Both accuse the other of methodology errors.
- **Full-context baseline is ~72–73%** on LoCoMo — several memory products score *below* just stuffing the whole conversation into a frontier model's context.
- **Bottom line: IronMem's 68.4% is at par with Mem0's peer-reviewed paper number (~66.9–68.5%), not 24 points behind the field.** The 90s-range numbers are a marketing race run on modern answer models with lenient judges — a race IronMem can enter on the same terms *while also publishing the honest methodology*, which nobody else does.

### 1.2 What the top systems actually do (techniques worth stealing)

- **Tiered retrieval pipeline** (ByteRover): 5 stages, most queries resolved sub-100ms with **no LLM calls**; their ablation attributes **+29.4pp** to this alone. Hierarchical "Context Tree" with importance scores 0–100, maturity tiers (draft→core), recency decay τ≈30d.
- **Observation logs instead of lossy summarization** (Mastra "Observational Memory"): an Observer agent continuously compresses conversation into an **append-only, timestamped, priority-tagged decision/event log** (3–6× compression, no destructive summarization ever), plus a Reflector agent that periodically reorganizes long-term memory. This is the highest LongMemEval architecture published.
- **Temporal knowledge graphs** (Zep/Graphiti): every fact edge carries valid_from/valid_until; strongest published temporal-reasoning results. *(IronMem already has this in `memory_edges`.)*
- **Reranking**: cross-encoder or reasoning-aware LLM rerank (MemReranker-style "name the bridging entity before ordering") is table stakes in every top system; reflective memory management (topic summarization + tuned rerank) is worth ~+10pp in published ablations.
- **Knowledge-update supersession and abstention**: two of LongMemEval's five scored abilities (information extraction, multi-session reasoning, temporal reasoning, knowledge updates, abstention) — and the two where most systems fail. Stale-fact suppression at *ranking* time and an explicit "no supporting memory" response are cheap wins.
- **Human-like memory mechanics** (research direction: ACT-R activation, Ebbinghaus decay, sleep-phase consolidation, reconsolidation on retrieval): converging on episodic/semantic/procedural typing — which IronMem's `kind` taxonomy and dream/sweep cycles already mirror.

### 1.3 The governance opening

- Analysts describe the same gap everywhere: **orgs have a memory layer but no governance layer** ([Atlan](https://atlan.com/know/ai-agent-memory-governance/)); memory poisoning, stale context, access violations, and regulatory exposure live in that gap.
- **EU AI Act Articles 12/13** (automatic event logging, traceability to source data) enforcement begins **August 2, 2026** — weeks away. Penalties up to 4% of global turnover.
- What enterprises are told to demand: versioned memory snapshots, memory→action lineage, per-agent access controls on shared memory, retention/deletion that satisfies privacy law.
- **IronMem already has the primitives** — hash-chained `memory_ledger`, `injection_events` (which *is* memory→action lineage), namespaces, consent state, legal hold, trust tiers, `brain_snapshots`, governed forget. No major competitor has any of this embedded. The move is to (a) make governance cost 0 points, (b) turn the primitives into a one-command compliance report, and (c) publish the **governance-on benchmark column** nobody else can print.

### 1.4 IronMem's real gaps

1. **No LongMemEval harness or score.** That's the benchmark the market quotes most in 2026; without a number IronMem is invisible in every comparison table.
2. **LoCoMo multi-hop (52.5%) and open-domain (50.0%)** are the two score sinks (temporal 78.2, single-hop 72.1).
3. **Governance-on costs 2.1pp** (66.3 vs 68.4). The moat currently reads as a tax.
4. **In-repo eval is thin** (3 deterministic cases) — no CI gate protects retrieval quality between benchmark runs.
5. Heavy LLM dependence in the retrieval hot path; cross-encoder reranker exists but is not the default.
6. Ecosystem: Qdrant/Neo4j adapters built but not config-wired; no Python/TS SDKs.

---

## 2. Existing infrastructure each phase builds on (verified in source)

| Asset | Where | Used by |
|---|---|---|
| QueryRoute classifier + per-route `FusionWeights`, RRF fusion, decomposition, entity-alias + multi-hop expansion | `src/retrieval.rs` | Phases 1, 4 |
| LLM rerank + ONNX cross-encoder backend | `src/retrieval.rs`, `src/reranker.rs` | Phase 1 |
| `memory_edges` temporal graph (valid_from/valid_until, superseded_by) | `src/db.rs` | Phases 1, 4 |
| `memory_chunks` skim layer, `observations` table, CCR lossless blob store | `src/db.rs`, `src/ccr/` | Phases 1, 2 |
| Sweep/dream background scheduler + reflection proposals | `src/sweep.rs`, `src/auto_dream.rs`, `src/reflection.rs` | Phase 2 |
| Dual narrative+facts extraction with coverage pass, multi-provider LLM plumbing | `src/compress.rs`, `src/provider.rs` | Phases 0, 2 |
| `EvalCase`/`EvalReport`/gate pattern | `src/eval.rs` | Phase 0 |
| Hash-chained ledger, injection_events, namespaces/consent/legal-hold, brain_snapshots | `src/db.rs`, `src/governance.rs` | Phase 3 |
| Trust-tier / temporal-trust retrieval boosts (implemented, weight 0) | `src/retrieval.rs` | Phase 3 |
| Qdrant/Neo4j `StorageBackend` adapters (conformance-tested, unwired) | `src/storage.rs` | Phase 5 |
| LoCoMo harness + prior tuning playbook (31.6% → 68.4%) | `BMC-INC/ironmem-locomo-benchmark`, `docs/superpowers/plans/2026-06-08-ironmem-locomo-improvements.md` | Phases 0, 4 |

---

## 3. The phased roadmap

### Phase 0 — Measurement foundation (~1–1.5 weeks) — *do first, everything else claims deltas against it*

1. **In-repo LongMemEval harness** — `ironmem bench longmemeval` (new `src/bench.rs`, modeled on `eval.rs`'s report pattern): ingest LongMemEval sessions through the existing `compress`/`remember` write path; answer via `retrieval::hybrid_search` + the provider abstraction; LLM-judge scoring via the same multi-provider plumbing. Emit per-ability breakdown (information extraction, multi-session, temporal, knowledge-update, **abstention**) — this table drives Phases 1–4. Keep a vendored "LongMemEval-lite" subset (~50 Qs) for cheap local iteration.
2. **Expand `src/eval.rs` from 3 → ~40 deterministic cases** (no API key needed, `BruteForceStore`): one cluster per query route — multi-hop chains through `memory_edges`, temporal supersession, open-domain paraphrase recall via chunks, knowledge-update (fresh fact must outrank stale), abstention (empty result above threshold), and a **governance-parity cluster** (same case through `remember_with_governance` must score identically).
3. **CI regression gate**: `ironmem eval --gate` on every PR; nightly LoCoMo-lite/LongMemEval-lite runs against stored baseline JSON, fail on >1pp regression.
4. **Ablation flags** on every retrieval feature (most already exist via `RetrievalTuning`) so the harness can print ByteRover-style ablation tables.
5. **Credibility play (the differentiator):** publish, next to every IronMem score — the full-context baseline on the *same* answerer/judge, the exact models used, per-category numbers, judge-agreement stats (Cohen's κ already computed for LoCoMo), and results on the **audited/corrected LoCoMo answer key**. Nobody else does this; in a market where every number is disputed, *rigor is marketing*.

**Exit criteria:** a LongMemEval number exists (expect ~65–75% baseline); CI blocks retrieval regressions; ablation tables generate.

### Phase 1 — Tiered retrieval + reranking defaults (~2–3 weeks) — *attacks multi-hop 52.5 and open-domain 50.0*

1. **Stage `hybrid_search` into explicit tiers with early exit:**
   - **T0 (<10ms, no LLM):** exact/alias entity match + edge lookup; profile/procedural pins.
   - **T1 (<100ms, no LLM):** today's FTS+vector+graph RRF with route-specific weights.
   - **T2 (confidence-gated):** cross-encoder rerank — **flip the default** to the ONNX cross-encoder when `local-onnx` is built.
   - **T3 (rare):** LLM rerank only when T2's top-score margin is below threshold.
   - Gate tiers on RRF score-margin; record tier-exit rates and latency in `metrics.rs`.
2. **Multi-hop — iterative retrieve-then-expand:** for `QueryRoute::MultiHop`, take round-1 hits' top entities from `memory_edges`, issue bridged sub-queries, fuse with `rrf_fuse`; raise the multi-hop entity fusion weight from 0 and tune via Phase 0 ablations.
3. **Open-domain — chunk-level recall:** fuse `memory_chunks` (FTS + embeddings) as a first-class RRF list for OpenDomain; map chunk hits back to parent memories with chunk text attached as rerank evidence; widen the candidate pool for this route.
4. **Importance/maturity/decay activation scoring** (ByteRover Context-Tree analog, no new tables): add `maturity` (draft→stable→core) to `memory_meta` via the existing migration helper; promote via dream sweep on survival/re-reference/user confirmation; activation = importance × maturity × exp(−age/τ), reinforced on retrieval through the existing feedback hooks; feed into RRF as an additive boost.
5. **Knowledge-update at ranking time:** when candidates share a (source, relation) edge key, suppress the one with `valid_until` set or the older edge — supersession is currently queryable but not enforced in ranking.
6. **Abstention:** post-rerank top score below threshold → explicit "no supporting memory" through MCP/REST instead of the best bad hit.

**Exit criteria:** LoCoMo overall ≥78, multi-hop ≥65, open-domain ≥62; LongMemEval knowledge-update/abstention +10pp each; p50 latency on T0/T1 exits <100ms.

### Phase 2 — Observation-log extraction (~2–3 weeks) — *raises the ceiling every retrieval gain is capped by*

1. **Observer pass** (new `src/observer.rs`, invoked from the compression sweep): produce an append-only, timestamped, priority-tagged observation/decision log (target 3–6× compression, never destructive) instead of summary-first storage. Each line: timestamp, priority, type (decision/fact/preference/error-fix), text. Persist as `kind='observation'` memories through `remember_with_governance` so embeddings, chunking, FTS, and governance apply automatically. Narrative summary becomes a secondary artifact.
2. **Reflector pass** (extends dream sweep + `reflection.rs`): merge duplicates, promote recurring observations to `maturity='core'`, extract edges into `memory_edges`, mark superseded observations with `valid_until`. Never deletes; governed forget stays the only deletion path.
3. **Coverage guarantee:** the existing coverage pass becomes a log-vs-transcript diff; misses become low-priority observations. CCR blobs remain the lossless fallback — expose `recall --verbatim` deep-fetch for T3 retrieval (EverMemOS-style "reconstructive" phase, nearly free here).
4. **Cost control:** Observer on the cheap/fast provider tier; Reflector on the strong model, idle-gated (auto_dream already does this).

**Exit criteria:** LongMemEval information-extraction ≥90%; new deterministic eval cluster "detail present in transcript must be retrievable post-compression" passes; compression ratios surfaced in `ironmem status`.

### Phase 3 — Governance: score-neutral → score-positive + compliance product (~1.5–2 weeks)

1. **Close the 2.1pp governance-on gap:** profile the source (likely PII fail-closed dropping answer-bearing text + namespace pool shrinkage); switch to redact-with-placeholder + governed reveal at answer time; enforce parity ≤0.5pp via the Phase 0 CI cluster.
2. **Make governance score-positive:** re-enable the zeroed trust/tier weights, but as **rerank evidence** rather than raw ranking boosts — add trust_tier and validity to the structured rerank text so authoritative, current facts win knowledge-update questions (user corrections outrank stale machine inferences). Ship non-zero defaults only when the eval gate proves gains.
3. **`ironmem compliance-report`** (new `src/compliance.rs` + REST routes): one command emits an EU-AI-Act Art. 12/13-mapped report from existing tables — ledger chain verification, memory→action lineage (injection_events: memory → injection → session → outcome), retention/consent/legal-hold status per namespace, snapshot versioning.
4. **Per-agent access controls:** API-key → agent identity → allowed namespaces/tiers on MCP/REST callers, ledger-logged.
5. **Marketing artifacts in-repo:** `docs/compliance/eu-ai-act-mapping.md`; README claim "governance costs 0pp — benchmarked"; publish the governance-on column beside every score.

**Exit criteria:** governance-on delta ≥ −0.5pp (target: positive on knowledge-update); compliance report generated end-to-end in tests; lineage answerable for any memory id.

### Phase 4 — Leaderboard push (~1–2 weeks, then ongoing)

1. **Temporal 78.2 → 90+:** absolute-date normalization at extraction (Observer stamps resolved dates), interval reasoning ("how long between X and Y") over `event_time`, temporal chain-walks over edge validity windows for before/after questions.
2. **Systematic tuning sweep:** grid/Bayes over per-route FusionWeights, RRF_K, pool multipliers, activation τ — on benchmark-lite with held-out validation to avoid overfitting.
3. **Reasoning-aware multi-hop rerank prompt:** ask the model to name the bridging entity before ordering (MemReranker-style).

**Exit criteria:** LoCoMo ≥88 overall / temporal ≥90; LongMemEval ≥90 — published with full methodology + baselines + the governance-on column.

### Phase 5 — Ecosystem & distribution (~2 weeks, parallelizable)

1. **Wire Qdrant/Neo4j:** config selectors (`vector_backend`, `graph_backend`) + construction in the server/CLI startup + a backfill command — the adapters and conformance tests already exist.
2. **Python + TypeScript SDKs:** thin typed clients over the ~35 REST routes (OpenAPI spec first); `pip install ironmem` / `npm i ironmem` with MCP config snippets.
3. **Publish** the LongMemEval harness beside the LoCoMo repo + a results page.

---

## 4. Sequencing

```
Phase 0 (measure) ──► Phase 1 (retrieval) ──► Phase 2 (extraction) ──► Phase 4 (tune/push)
        │                                                                    ▲
        └─► Phase 3.1/3.2 (governance parity/positive) ─────────────────────┘
Phase 3.3–3.5 (compliance report/marketing) — independent, start early (Aug 2026 deadline)
Phase 5 (ecosystem) — independent, interleave
```

- Every change lands behind config/env flags with eval-gated defaults, so live deployments are never at risk.
- Rough total: **~8–11 agent-weeks** to a credible "best-scoring memory tool, with governance at zero cost — and we show our work" position.

## 5. Sources

- [Penfield Labs LoCoMo audit](https://dev.to/penfieldlabs/we-audited-locomo-64-of-the-answer-key-is-wrong-and-the-judge-accepts-up-to-63-of-intentionally-33lg) · [The Benchmark Theatre](https://essays.bloo-mind.ai/posts/2026-05-20-mem-eval/)
- [Mem0's correction of Zep's 84%](https://github.com/getzep/zep-papers/issues/5) · [Zep's rebuttal](https://blog.getzep.com/lies-damn-lies-statistics-is-mem0-really-sota-in-agent-memory/) · [Mem0 paper (arXiv 2504.19413)](https://arxiv.org/html/2504.19413v1)
- [Mastra Observational Memory](https://mastra.ai/research/observational-memory) · [ByteRover 2.0 benchmark](https://www.byterover.dev/blog/benchmark-ai-agent-memory) · [Zep temporal KG paper](https://blog.getzep.com/content/files/2025/01/ZEP__USING_KNOWLEDGE_GRAPHS_TO_POWER_LLM_AGENT_MEMORY_2025011700.pdf)
- [2026 memory framework comparisons (Atlan)](https://atlan.com/know/best-ai-agent-memory-frameworks-2026/) · [AI agent memory governance (Atlan)](https://atlan.com/know/ai-agent-memory-governance/)
- [Human-inspired memory architecture (arXiv 2605.08538)](https://arxiv.org/abs/2605.08538) · [Mem0 benchmarks overview](https://mem0.ai/blog/ai-memory-benchmarks-in-2026)
