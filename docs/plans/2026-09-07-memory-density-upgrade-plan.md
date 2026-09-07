# IronMem memory density and storage upgrade plan

Status: Phase A merged in PR #45. Phases B–D implemented in the storage-foundations batch, with final validation recorded in `docs/benchmarks/memory-density/`. Task 11 uses complete transactional row-set differences rather than an incomplete mutation journal; see the implementation methodology. Phases E–G remain pending.
Prepared: 2026-09-07.

## Verified starting point

- Repository: https://github.com/BMC-INC/Iron-mem.
- Product checkout: `/Users/kingjames/Projects/Iron-mem-retrieval-batch`.
- Current branch: `fix/local-compression-summary-bloat`, HEAD `d27771f67c0c9571a9e557d13fbfe29bf6a6ef11`.
- Remote main: `78b680bc270565ecd02ff488b4c3c6baca04120f`; also the merge base of this branch. The branch changes only `CHANGELOG.md`, `src/compress.rs`, and `src/hooks.rs`.
- PR #45 is open and mergeable. GitHub reports successful Ubuntu, macOS, Windows and CodeQL checks on this head. These are existing remote results, not tests run for this plan.
- Preserve existing untracked `LONGMEMEVAL_HANDOFF_2026-07-15.md`, `data/`, `docs/evals/`, `findings.md`, `progress.md`, and `task_plan.md`. Other IronMem worktrees have separate work in progress.

The supplied conversation is a proposal, not execution authority or proof of current behavior. Source inspection confirms whole-blob CCR deduplication and full JSON snapshots. Several details change the implementation plan:

1. `src/strutil.rs::safe_truncate` appends `… [truncated]` after taking the requested byte allowance. It is UTF-8 safe, but does not enforce an inclusive output-byte limit.
2. `src/hooks.rs` omits memory IDs, permits the first entry to exceed budget, does not cap serialized tags, and does not reserve its omission footer. The digest expansion pointer in `src/compress.rs` can be truncated away.
3. Telemetry is partly present: `injection_events`, `record_injection_events`, score adjustments, and maturity promotion already exist in `src/db.rs`. No dedicated recall/expansion counters were found. CLI injection records all authorized candidates before serialization; MCP injection writes the file without the same event call. Neither result reliably means a memory was actually written.
4. `src/snapshot.rs` writes version 4 full payloads. It includes memories, edges, evidence roots, explicit influence policies and contradiction sets, but not complete memory metadata, session-blob links, or chunks. Restore deletes existing project state and performs multiple independent writes; it suppresses several errors. Exact-source recovery and atomicity need work before deltas.
5. `memory_ledger` exists, but a monotonic ledger ID alone does not establish complete, transactionally aligned mutation capture. This must be proven, not assumed.
6. CCR already records original and compressed lengths and has shared dictionaries. Avoid adding duplicate counters or renormalizing existing tables without measurement.

## Architecture and scope decisions

- Preserve the exact-original SHA-256 address and legacy blob readers. Content-defined chunking changes physical storage, not memory identity or governance.
- Keep small objects on the existing CCR path. Select the large-object threshold from measured results, initially testing 128, 256, 512 KiB and 1 MiB. No unmeasured default rollout.
- Treat generated context as an explicitly bounded index: injected working set → authorized retrieval → exact source expansion. Delimit memory data, but do not describe delimiters as a security boundary against prompt injection.
- Keep ranking, temperature and authorization separate. Existing egress controls remain authoritative on all paths; caches and raw hash expansion must not bypass them. Do not change strict-mode, attestation, OAuth or deployment configuration as part of this work.
- Retain SQLite and PostgreSQL support. New memory-dependent tables reference `memory_meta(memory_id)`, not SQLite FTS virtual-table identities.
- Work locally with isolated test databases and deterministic fixtures first. Paid answer/judge runs and production-data migrations are separate execution decisions. No benchmark score in the pasted conversation is adopted as a new measured result.
- All seven proposed upgrades are covered. Interning and semantic deltas have evidence gates because forcing them into production without a demonstrated benefit could make IronMem worse.

## Delivery sequence

| Phase | Outcome | Tasks | Dependency |
|---|---|---|---|
| A | Finish the bounded digest fix | 1–2 | Current PR #45 |
| B | Harden context and establish measurements | 3–5 | A |
| C | Deduplicate large exact sources | 6–8 | B baseline |
| D | Make snapshots complete, then incremental | 9–12 | B; integrate with C storage |
| E | Measure temperature and select working sets | 13–15 | B and stable storage metrics |
| F | Reduce measured metadata duplication | 16–17 | Profiling and D/E contracts |
| G | Introduce temporal assertion deltas carefully | 18–20 | First five upgrades validated |

Each numbered task is a focused implementation slice, normally 2–5 files. Split further if it grows beyond that. Phase D is deliberately larger than the conversation suggests because it changes recoverability. Ship complete slices; keep experimental behavior opt-in until its gate passes.

## Phase A: complete PR #45

### 1. Make digest and rendering limits exact

Files: `src/strutil.rs`, `src/compress.rs`, `src/hooks.rs`.

Add a strict byte-budget helper rather than silently changing every existing truncation caller. Its returned value, including any truncation marker, must fit the budget even for zero/tiny budgets and multibyte input. Reserve the digest expansion sentence before allocating fact/tool text. Put a renderer-owned memory ID and actionable `retrieve_original` argument outside truncatable summary content. Do not invent an ID before persistence; ensure rendered entries provide it and only promise exact expansion when a source exists.

Acceptance: digest ≤2,000 bytes; serialized summaries ≤4,000 bytes; complete default file ≤24,000 bytes, counting metadata, escaped tags, separators and omission footer. No first-entry bypass. Handles survive every supported budget. Keep these decimal byte units explicit.

Verification: extend `strutil`, `compress` and `hooks` tests with enormous tags/tool names/facts, many memories, invalid timestamps, Unicode, empty input, tiny budgets, boundary fits and pointer preservation. Check exact serialized bytes, not estimates.

### 2. Report what was actually injected

Depends on 1. Files: `src/hooks.rs`, `src/main.rs`, `src/mcp.rs`, `src/e2e.rs`, `CHANGELOG.md`.

Return a render/write report containing written IDs, omitted count and byte count. Record events after successful output, using written IDs only. Make CLI and MCP counts agree with the file. Preserve the last complete file on failed replacement using a platform-tested atomic write strategy. Describe a telemetry-write failure honestly without pretending the file write failed or recording duplicate successful injections on retries.

Acceptance: recorded/returned injection counts match file entries; failed writes produce no success events; IDs in generated entries resolve through the authorized expansion path when transcripts exist.

Verification: temporary-project CLI/MCP integration tests for oversized inputs, no authorized memories, file-write failure, stale-file removal and exact transcript retrieval. Then run the existing CI-equivalent build/test/clippy/eval checks. Refresh PR #45, verify checks on its new SHA and squash-merge when implementation is authorized and green.

## Phase B: safe context and a Memory Density Frontier

### 3. Render extracted content as untrusted memory data

Depends on A. Files: `src/hooks.rs`, `src/compress.rs`, `src/egress.rs`, `src/e2e.rs`.

Use a renderer-owned preamble and an escaped data representation whose delimiters cannot be closed by stored content. Keep generated handles separate from memory text. Inspect other context renderers for reuse in a later small slice; preserve existing authorization behavior now.

Acceptance: hostile facts, tags and tool names cannot create renderer-owned headings, fake handles or terminate the data container; denied content remains absent. No claim that text escaping alone prevents model instruction-following.

Verification: adversarial fixtures containing fences, closing delimiters, fake system messages, Markdown imports and multiline tags; existing egress regression suite.

### 4. Add reproducible byte and storage measurements

Depends on A. Files: new `src/density.rs`, `src/bench.rs`, `src/metrics.rs`, `src/main.rs`, benchmark fixtures.

Define a versioned run manifest: code SHA, dataset hash, split/question IDs, seed, configuration, model/tokenizer identities, codec/chunk configuration, hardware and cache condition. Measure original logical bytes, unique compressed payload bytes, manifests/dictionaries/index overhead, DB footprint, injected bytes/tokens, retrieval payload bytes, storage reads where observable, expansion bytes, compression CPU/wall time, peak memory and latency distributions. Mark unavailable measurements unavailable; do not relabel response bytes as disk I/O. Count shared chunks once physically and separately report logical references.

Acceptance: deterministic local dry runs need no API credentials, emit JSON/CSV and distinguish missing accuracy from zero accuracy. A reproducible synthetic corpus covers duplicates, near-duplicates, unique/incompressible data and large logs. Any private workload profiling stays local and uses a consistent copied database, not a raw copy of an active WAL database.

Verification: golden metric accounting on a tiny corpus with known bytes, duplicate references and known expansions. Record a baseline before any storage changes.

### 5. Sweep context budgets and publish comparable results

Depends on 3–4. Files: `src/density.rs`, `src/bench.rs`, `scripts/run_memory_density.sh` (new), `docs/evals` methodology document.

Sweep 2/4/8/16/24/48/96 decimal KB in an isolated benchmark configuration. Production retains its 24,000-byte default ceiling. Reuse the current LongMemEval harness and explicitly adapt the separate LoCoMo harness after checking its checkout; do not mix their denominators or labels. Exercise actual injection/retrieval/expansion paths, not merely prompt truncation. Record total context exposure including later expansion so savings are not hidden in extra tool output.

Acceptance: paired questions/configurations, fixed answer/judge settings, separate cold/warm runs, baseline and full-context comparison, accuracy by dataset category (including single-hop, multi-hop and temporal where defined), abstention, source recovery and byte-exact reconstruction. Publish accuracy-vs-context and storage-vs-latency plots with raw artifacts and confidence intervals. Resume identity includes budget and feature flags. Ground-truth answers never influence working-set selection.

Verification: local smoke run for all seven budgets and resume invalidation; representative scored canary before a full model-based run. Predeclare non-inferiority criteria before examining scored results; proposed initial ceiling is a one-percentage-point overall accuracy loss, assessed with paired uncertainty, with separate review of category regressions. Insufficient statistical power means inconclusive, not passed.

Checkpoint B: bounded context regressions pass; measurements are reproducible; baseline artifacts exist. No performance or accuracy improvement is claimed yet.

## Phase C: content-defined CCR chunking

### 6. Specify the versioned chunked-object contract

Depends on 4. Files: new CCR manifest/chunker modules, `src/ccr/mod.rs`, schema helpers in `src/db.rs`.

Select and pin a maintained FastCDC implementation after reviewing its primary documentation, license and test vectors. Store deterministic ordered chunk hashes/lengths with chunker version/parameters, original length/hash and a canonical manifest hash. Keep the external object address equal to the original SHA-256. Keep storage format separate from codec dispatch: loading a manifest requires database resolution, not a pure decompressor.

Acceptance: old zstd/dictionary objects remain readable; identical input has a deterministic manifest; malformed manifests, impossible lengths, unknown versions and excessive chunk counts fail safely. Define no recursive manifests for this first format.

Verification: fixed chunking vectors, insertions near the beginning/middle, empty/binary/Unicode bytes, manifest tampering and legacy fixtures.

### 7. Integrate chunked writes and verified reads

Depends on 6. Files: `src/ccr/mod.rs`, chunk/manifest modules, `src/db.rs`, `src/config.rs`.

Deduplicate/compress chunks, retain the dictionary dependencies and assemble the original in order. Verify every chunk, manifest and final original hash. Use transactional publication and race-safe upserts. Benchmark threshold and chunk-size combinations, including loss of cross-chunk compression efficiency; retain whole-object storage when the measured policy selects it. Keep feature disablement compatible with reading already-written chunked objects.

Acceptance: byte-exact recovery for every fixture; no partially published object after injected failures; concurrent identical stores produce valid ownership counts. Full and range expansion keep existing external behavior, with actual bytes read measured honestly.

Verification: CCR round-trip/corruption/concurrency suites and the density corpus, on SQLite and PostgreSQL. Compare total storage including manifests, CPU and p95 load/store latency against whole-object CCR.

### 8. Make chunk ownership, GC and migration safe

Depends on 7. Files: CCR ownership module, `src/db.rs`, `src/recovery.rs`, `src/main.rs`, integration tests.

Separate live object ownership from manifest-to-chunk ownership, including repeated chunks within one object. Enumerate roots from observations, memories, snapshots and other retained references. GC must retain all reachable chunks and dictionaries. Add a resumable opt-in legacy conversion with dry-run inventory and verification before replacement; do not migrate the live store automatically.

Acceptance: deleting one memory cannot break another memory or snapshot; crash/retry cannot leak or prematurely remove shared chunks; old backups remain recoverable. Older binaries that cannot read the new format require backup restoration for rollback, not merely a flag toggle.

Verification: shared-object deletion, snapshot pinning, missing dictionaries, fault injection, restart/resume and reference-reconciliation tests. Enable chunked writes by default only if representative measured savings justify CPU/latency overhead.

## Phase D: complete and incremental snapshots

### 9. Define and preserve complete recoverable state

Depends on 4; coordinate with 8. Files: `src/snapshot.rs`, `src/db.rs`, `src/recovery.rs`, snapshot fixtures.

Introduce a versioned full-checkpoint schema covering memory content/timestamps, complete authoritative metadata, namespace, governance, tombstones, evidence/lineage, policies, contradictions, graph edges, entities, source links and chunks. Explicitly classify embeddings/ANN indexes and temperature aggregates as rebuildable state; retain their reconstruction inputs. Classify sessions/observations and audit history as either included or referenced retained evidence, with a tested export closure. A snapshot is not an independent backup unless its blob/dictionary dependencies are exported too.

Acceptance: round-trip preserves authority, timestamps and original retrieval; snapshot roots pin every required source. Legacy v4 imports explicitly report fields they lack and do not silently fabricate original provenance or authority.

Verification: rich fixture including expired, restricted, tombstoned and contradictory memories; restore then compare canonical state and exact source hashes after GC.

### 10. Make full checkpoint capture and restore atomic

Depends on 9. Files: `src/snapshot.rs`, transaction helpers in `src/db.rs`, `src/vectorstore.rs`, restore tests.

Capture one consistent database state, not independently sampled queries. Validate the entire payload/dependency closure before modifying a project. Stage or transact the replacement, propagate errors, and commit once. Preserve logical identity through explicit mappings and stable handles; rebuild derived indexes and invalidate caches safely. Keep global restore blocked. Define restoration policy so restoring an old checkpoint does not accidentally reinstate currently revoked authority; preserve audit history and append a restore event.

Acceptance: failure at any restore step leaves the prior authoritative project state intact; namespace/project boundaries hold; dry-run performs full verification without mutation. Report actual restored counts.

Verification: interruption at each destructive step, concurrent readers/writers, ID collisions, cross-project edges, source expansion and policy-revocation tests on both databases.

### 11. Capture a complete monotonic mutation stream

Depends on 10. Files: `src/db.rs`, `src/snapshot.rs`, new mutation module, governance/contradiction tests.

Inventory all authoritative mutation paths. Reuse the governance ledger only if it captures every required change atomically; otherwise add a dedicated journal written in the same transaction as state changes, with ledger correlation. Cover inserts, updates, edges, removals, tombstones, source attachments, policy/evidence and contradiction changes. Exclude access telemetry from semantic deltas unless explicitly requested. Avoid timestamp boundaries.

Acceptance: a checkpoint has an unambiguous high-water mark; rolled-back changes emit no committed journal entry; concurrent commits cannot be skipped because sequence allocation preceded commit. Define pruning only after dependent checkpoints are safe.

Verification: mutation-coverage matrix plus concurrent transaction, rollback and restart tests; replayed state equals a fresh canonical full capture.

### 12. Add verified deltas and bounded checkpoint chains

Depends on 8, 10–11. Files: `src/snapshot.rs`, delta module, `src/db.rs`, CLI snapshot options, tests.

Store typed ordered changes with base checkpoint ID, parent hash, sequence interval and resulting-state hash. Replay a verified checkpoint and contiguous deltas to a target. Add periodic full checkpoints based on measured chain length, delta/full-size ratio and restore latency. Preserve dependencies during retention/GC; never silently fall back to partial restore.

Acceptance: full and delta restore produce equivalent canonical state; missing/reordered/corrupt deltas fail before replacement; no-change snapshots avoid another full-state payload. Measure capture write amplification and restore time separately.

Verification: long mutation sequences, deletions, governance changes, checkpoint rollover, chain pruning, crash recovery and legacy snapshot compatibility. Checkpoint D requires an independent backup/export restore rehearsal on temporary databases.

## Phase E: memory temperature and dynamic working sets

### 13. Consolidate accurate access telemetry

Depends on 2 and 4. Files: `src/db.rs`, new access-metrics module, `src/expansion.rs`, `src/metrics.rs`, tests.

Add `last_recalled_at`, `recall_count`, `last_expanded_at`, `expansion_count`, `last_injected_at`, `injection_count`, and `mutation_count` through a compact side table/aggregation model. Reuse existing injection events; do not add duplicate truth sources. Derive source sizes from CCR and distinguish logical ownership bytes from unique physical storage. Historical missing recall/expansion data stays unknown.

Acceptance: define recall as returned authorized content, expansion as successful source delivery and injection as successful rendered inclusion. Internal ranking/candidate reads do not count. Use stable operation IDs where available for retries and document remaining at-least-once semantics. Aggregate without hot-row writes for every candidate or recording sensitive query text unnecessarily.

Verification: counter semantics, concurrency, backfill of existing injection history, failed/denied requests and telemetry-overhead microbenchmarks.

### 14. Wire telemetry consistently across delivery surfaces

Depends on 13. Files: `src/main.rs`, `src/mcp.rs`, `src/server.rs`, `src/egress.rs`, `src/e2e.rs`.

Instrument shared successful-delivery boundaries, avoiding double counting nested retrieval and expansion calls. Expose compact operator diagnostics through existing status surfaces; update SDK contracts in a separate small slice only if public response fields change.

Acceptance: equivalent CLI, REST and MCP requests produce equivalent metric semantics; denied content creates no successful access; injection metadata cannot promote a memory's authority.

Verification: surface parity tests and existing governance/evidence tests.

### 15. Select bounded working sets with explicit temperature

Depends on 3, 5 and 14. Files: `src/retrieval.rs`, `src/context.rs`, `src/config.rs`, new working-set module, `src/hooks.rs`.

Define deterministic hot/warm/cold states with decay and hysteresis. Start with local cache/prefetch and context selection; cold is not deleted or uploaded. Add explicit coding/planning/debug profiles with configured byte budgets under the production ceiling, a stable generic fallback, and capacity for decisions, procedures, constraints and unresolved corrections/contradictions. Expand CLI/MCP configuration wiring as a follow-on slice if needed.

Acceptance: authorization and relevance precede selection; popularity never changes trust or policy; stale/restricted content cannot leak through caches. Handles remain resolvable after compaction/restore via stable identity or explicit invalidation/regeneration. Refresh generated files after known policy/memory changes; acknowledge that already-imported model context cannot be recalled retroactively.

Verification: deterministic clock tests, cold-memory retrieval, cache invalidation, all profile budgets, governance regressions and paired density evaluation. Choose defaults from the measured frontier, not an assumed 8 or 16 KB optimum.

## Phase F: evidence-led shared-state interning

### 16. Profile repeated metadata and define a migration candidate

Depends on 4 and stable D/E schemas. Files: density profiler, schema inventory report.

Measure unique values, repeated bytes, indexes, existing normalization and join cost for project paths, tools, writers, source/classification strings and tags. Compare database-level savings after index overhead, not string lengths alone. Preserve case, NULL/empty distinctions, tag search semantics and exact source bytes.

Acceptance: ranked candidates with measured net gain and migration cost; select only candidates with material end-to-end benefit. A documented no-benefit decision completes the investigation without adding useless tables.

Verification: repeatable profile on synthetic and approved local workload copies.

### 17. Intern one measured metadata family per migration

Depends on a passing 16 result. Files per slice: `src/db.rs`, affected query/serialization module, migration tests, density report.

Introduce shared IDs internally, preserving public values and original hashed payloads. Dual-read/backfill/validate before retiring old fields. Keep policy versions immutable; an interned record must not mutate multiple memories' effective governance.

Acceptance: equivalent API/search/governance behavior and lower total storage at an acceptable measured query cost. Rollback and snapshot import/export cover both representations.

Verification: SQLite/PostgreSQL migration round-trips, collision/concurrency tests, retrieval parity and before/after footprint. Repeat only for the next justified family.

## Phase G: base-plus-delta semantic memory

### 18. Specify assertion identity and temporal semantics

Depends on A–E validated; incorporate any adopted F changes. Files: architecture decision document, semantic fixtures.

Define canonical subjects scoped by namespace/project, immutable assertions, source evidence, valid time vs recorded time, explicit supersession and a derived current-state pointer. Contradictory assertions coexist; they are not overwritten by popularity or arrival order. Start with explicit structured claims such as toolchain versions, not automatic merging of arbitrary prose.

Acceptance: examples cover Rust version changes, ambiguous subjects, concurrent claims, correction/retraction, tombstones and source-required policies. New storage must not fabricate evidence or rewrite historic hashes.

Verification: executable expected-state fixtures and review of compatibility with corrections, contradictions and snapshots before adding schema.

### 19. Persist opt-in immutable assertions and supersession

Depends on 18. Files: new assertion module, `src/db.rs`, `src/corrections.rs`, `src/contradiction.rs`, tests.

Keep existing memory IDs and originals; add explicit lineage and derived pointers. Record changes through the mutation journal and governance ledger. Current-state updates use expected-version concurrency control. An adapter preserves existing write behavior when disabled.

Acceptance: no history loss; conflicting writes are detected; deleting or restricting evidence has defined effects on current-state eligibility.

Verification: history reconstruction, conflicting updates, authorization and journal replay tests.

### 20. Integrate temporal retrieval, snapshots and evaluation

Depends on 19. Split into focused retrieval, snapshot and public-surface slices, each ≤5 files.

Support current and as-of queries while retaining legacy search results and exact expansion. Include assertions and pointer changes in full/delta snapshots. Evaluate temporal accuracy, contradiction handling, footprint and retrieval latency against independent memories.

Acceptance: legacy fixtures remain compatible; as-of/current answers follow the declared semantics; complete snapshot restore survives pointer rebuild; public enablement requires evidence of benefit. If semantics or gains do not hold, retain the experiment disabled and report why.

Verification: temporal/update/abstention benchmark categories, snapshot round-trips and end-to-end CLI/REST/MCP flows.

## Verification and release discipline

For each slice, run focused tests first, such as `cargo test --bin ironmem hooks::tests`, `cargo test --bin ironmem compress::tests`, `cargo test --bin ironmem ccr::tests`, or `cargo test --bin ironmem snapshot::tests`. New filters must match real tests; zero matched tests is not evidence.

At phase integration boundaries, use the existing CI commands: `cargo build`, `cargo test`, `cargo clippy -- -D warnings`, and `cargo run --quiet -- eval --out target/eval-reports`. Schema/storage phases also need actual PostgreSQL integration coverage; passing default SQLite tests does not prove portability. Keep optional ONNX/GPU checks limited to changes affecting those paths. Existing CI covers three operating systems; verify atomic file replacement on each.

Each implementation PR should contain its own fixtures, migration/rollback notes and relevant benchmark artifacts. Avoid bundling schema rewrites with unrelated work. Release proof distinguishes local tests, remote SHA/checks, merged state, installed binary and live runtime. A merge is not proof that the local memory service was upgraded.

Before the first production schema migration, obtain a consistent recoverable backup, verify it by restoring to a temporary location, and record the installed binary/schema versions. Keep new readers backward-compatible; where old binaries cannot read new formats, rollback restores the preserved backup. Do not touch the user's active database during development.

## Decisions that remain measurement-dependent

- FastCDC library/version, threshold, chunk sizes and compression settings.
- Accuracy/latency acceptance bands and a scored-run model/cost budget.
- Checkpoint cadence and maximum replay chain.
- Working-set default profile allocations and temperature decay.
- Which metadata families merit interning.
- Whether semantic deltas outperform independent assertions enough to enable publicly.

These do not block Phase A or local benchmark tooling. Immediate next implementation: Tasks 1–2 on PR #45, followed by context hardening and the density baseline.
