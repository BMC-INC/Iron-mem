# Governed Influence Implementation Handoff

**Updated:** 2026-08-01
**Source plan:** PR #38, `docs/plans/2026-07-31-governed-memory-influence-plan.md`
**Active worktree:** `/Users/kingjames/Projects/Iron-mem-pr38-implementation`
**Active branch:** `agent/governed-influence-evidence-roots`
**Base:** `origin/main` at `b80f84d9f05dd5b080db561784b1007135268e9d`
**State:** Phase 1 implemented and fully verified on its dedicated delivery branch.

## Delivery strategy

PR #38 is the approved architecture plan and explicitly prohibits one oversized implementation PR. Implement it as five reviewable PRs:

1. Evidence roots.
2. Influence policy model and governed CRUD.
3. Purpose verification, confirmation receipts, and the shared egress gate.
4. Contradiction sets.
5. Counterfactual evaluation, Workbench, compliance completion, and benchmarks.

Do not add implementation code to PR #38. Phase 1 is a separate branch from `main` and should become a draft PR referencing #38.

## Phase 1 implementation completed locally

Changed files:

- `src/governance.rs`
- `src/db.rs`
- `src/compliance.rs`
- `src/corrections.rs`
- `src/profile.rs`
- `src/reflection.rs`
- `src/snapshot.rs`

Implemented:

- Versioned, length-delimited evidence-root hashing with unambiguous field boundaries.
- Deterministic roots for stable sources and durable UUID roots for direct records without a stable source reference.
- `memory_meta.evidence_root_id` and `memory_meta.derivation_depth`.
- `memory_evidence_roots` with `primary`, `supporting`, and compatible `contradicting` roles.
- Cross-backend relational parent is `memory_meta(memory_id)`, never `memories(id)`.
- Every new memory receives a metadata parent and root in `insert_memory` before its ID is returned.
- Derived writes inherit the parent root and increment depth.
- Derived writes reject cross-namespace parents.
- Multi-source profile and reflection synthesis add distinct supporting roots and reject cross-namespace evidence.
- Correction-miner writes now record governed tool-output provenance.
- Snapshot payload version 2 preserves roots, supporting roots, derivation depth, and remapped parent links; version 1 remains readable through `serde(default)`.
- Lineage output includes primary root, all roots, independent-root count, and derivation depth.
- Compliance output includes evidence inventory and migration status/report.
- Explicit child-first evidence cleanup before deleting `memory_meta`.
- SQLite memory IDs now allocate above both live FTS row IDs and retained metadata IDs, preventing reuse of an audited tombstone ID.

## Migration behavior

Migration ID: `2026-08-01-evidence-roots-v1`.

The migration:

- Creates missing `memory_meta` parents for live SQLite and Postgres memories.
- Adds evidence columns and indexes idempotently.
- Creates `schema_migrations` with status, version, durable cursor, JSON report, and timestamp.
- Backfills in batches of 256.
- Inherits roots recursively without loading the full graph into memory.
- Assigns a deterministic compatibility root to broken-parent records and reports their IDs.
- Detects parent cycles, stores the cycle IDs, marks the migration failed, and fails startup closed.
- Repairs late legacy/direct-SQL rows even after an earlier migration was marked complete.
- Recreates missing normalized primary-root rows from populated `memory_meta` values.
- Does not rewrite the memory ledger.

Operator seam: `db::repair_evidence_roots` reruns deterministic repair after malformed parents are fixed. The stored migration report is exposed through compliance output.

## Important implementation decisions

- Two identical explicit `remember` calls without stable source references are independent evidence and receive different UUID roots.
- Reapplying governance to such a record preserves its stored UUID root.
- Stable sources use the canonical hash path; session-backed records use `session:<session_id>` when no explicit source reference exists.
- Supporting roots are deduplicated by root identity, so several derived records from one source do not inflate independent support.
- Default retrieval/ranking was not changed.
- No cloud service, ExecLayer, network call, or additional model call was introduced.

## Verification completed

Final checks after the source-less-root adjustment:

- `cargo fmt --all` — passed.
- `git diff --check` — passed.
- `cargo clippy --all-targets -- -D warnings` — passed.
- `cargo test --all-targets` — passed: 239 passed, 0 failed, 1 benchmark ignored; MCP clean-stdio integration 1 passed.
- Migration, evidence, lineage, snapshot, storage-conformance, and deterministic eval coverage all passed in the complete suite.

## Publish checkpoint

After final verification:

1. Stage only the eight Phase 1 files plus this handoff document.
2. Commit with a terse message such as `implement evidence-root lineage`.
3. Push `agent/governed-influence-evidence-roots` to `origin`.
4. Open a draft PR to `main` titled `Implement evidence-root lineage`.
5. Reference PR #38 and describe this as delivery PR 1 of 5.
6. Verify local SHA, remote SHA, and all GitHub checks before reporting success.

Do not merge PR #38 or the Phase 1 PR without explicit user direction.

## Remaining work after Phase 1

### Phase 2: influence policy

- Add `src/influence.rs` types and state-transition matrix.
- Add `memory_influence_policy` with permissive defaults and versioning.
- Add policy-read and policy-write capabilities.
- Implement optimistic concurrency and ledgered CLI/REST/MCP policy CRUD.

### Phase 3: purpose and shared egress gate

- Add advisory and verified purpose types.
- Add local-operator and trusted-runtime attestation verification.
- Add scoped, expiring, replay-protected confirmation receipts.
- Add deterministic evaluator, influence events, consumer capabilities, exact-source expansion, and one gate across every content-bearing surface.

### Phase 4: contradiction sets

- Add contradiction schema, versioned claim cardinality, deterministic creation/reconciliation, management APIs, and retrieval annotation.

### Phase 5: evaluation and Workbench

- Add the deterministic counterfactual influence cluster.
- Add Workbench simulation and evidence controls.
- Finish compliance/lineage/status metrics and performance/relevance gates.

## Workspace safety

- Treat `/Users/kingjames/Projects/Iron-mem-retrieval-batch` as the source checkout, not the removed Desktop path.
- That checkout contains unrelated untracked evaluation files; do not stage or modify them.
- Continue Phase 1 only in `/Users/kingjames/Projects/Iron-mem-pr38-implementation`.
