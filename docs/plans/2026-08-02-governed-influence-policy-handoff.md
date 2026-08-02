# Governed Influence Policy Handoff

**Updated:** 2026-08-02
**Source plan:** PR #38, `docs/plans/2026-07-31-governed-memory-influence-plan.md`
**Active worktree:** `/Users/kingjames/Projects/Iron-mem-pr38-policy`
**Active branch:** `agent/governed-influence-policy`
**Stack base:** `agent/governed-influence-evidence-roots` at `10c20feeb3dc93ed71bc5bc8703c528874feda22` (draft PR #39)
**State:** Phase 2 implemented and locally verified; ready to commit and publish as delivery PR 2 of 5.

## Delivery boundary

This phase implements the influence-policy control plane only:

- policy model and deterministic state semantics
- sparse, versioned policy storage
- capability-checked CLI, REST, and MCP CRUD through shared handlers
- optimistic concurrency
- atomic policy-transition ledger receipts
- snapshot preservation

It intentionally does not change retrieval, ranking, context injection, source expansion, or any other content-bearing surface. The single shared egress evaluator and verified-purpose enforcement belong to Phase 3. This keeps the approved five-PR delivery sequence reviewable and avoids partially enforcing policy on only some output paths.

## Implementation completed

### Policy model

Added `src/influence.rs` with:

- `InfluenceState`: `eligible`, `quarantined`, `reasoning_only`, `action_restricted`, `blocked`, and `superseded`
- ordered `ActionRisk`: `none` through `critical`
- `MemoryInfluencePolicy` with the exact PR #38 fields and permissive version-1 defaults
- canonical task-type normalization, sorting, deduplication, and bounded validation
- pure state semantics for the Phase 3 evaluator seam
- reason-coded policy errors and a shared principal/capability authorization model

Memories without an explicit row read as:

```text
state = eligible
version = 1
allowed_task_types = []
denied_task_types = []
maximum_action_risk = critical
requires_original_source = false
requires_human_confirmation = false
maximum_derivation_depth = none
```

### Storage and concurrency

Migration ID: `2026-08-02-influence-policy-v1`.

The migration creates the sparse `memory_influence_policy` table and records a completed, zero-backfill migration report. Existing memory rows are not rewritten.

Policy updates:

- require the caller's expected version
- read, compare, write, and append the ledger entry in one transaction
- use the existing SQLite write lock plus `BEGIN IMMEDIATE`
- use a namespace-scoped Postgres advisory transaction lock
- reject stale writers with `policy_version_conflict` and the current version
- reject empty or no-op changes
- preserve legal hold while still allowing a policy to become more restrictive
- delete the policy child explicitly before deleting `memory_meta`

Every successful transition appends `influence_policy_update` with old/new policy, old/new version, actor, authority, reason, and request ID.

### Access surfaces

- CLI: `ironmem influence get` and `ironmem influence set`
- REST: `GET /memory/{id}/influence` and `PUT /memory/{id}/influence`
- MCP: `get_memory_influence` and `set_memory_influence`

All three surfaces call the same policy service and storage path.

Administrative authorization is explicit:

- `influence_policy:read`
- `influence_policy:write`

Agent-key namespace access does not imply either capability. Local CLI and stdio MCP use the local-operator authority. Shared-token HTTP MCP has no policy authority by default and must receive configured capabilities explicitly.

### Snapshot compatibility

Snapshot payload version 3 preserves explicit influence policies, versions, actor metadata, and timestamps while remapping memory IDs during restore. Older snapshot payloads remain readable through `serde(default)` and restore with implicit permissive policies.

## Verification completed

- `cargo fmt --all` — passed
- `git diff --check` — passed
- `cargo clippy --all-targets -- -D warnings` — passed with zero warnings
- `cargo test --all-targets` — passed: 248 passed, 0 failed, 1 intentional benchmark ignored; MCP clean-stdio integration 1 passed
- manual local SQLite CLI smoke — default read, versioned update, canonical task normalization, reread, and stale-version rejection passed
- focused REST test — read-only denial, writer update, stale conflict, and reader fetch passed
- focused MCP test — registration, shared handlers, version conflict, and shared-HTTP capability denial passed
- snapshot round trip — explicit version-2 policy restored unchanged

The full deterministic evaluation suite passed, providing the no-retrieval-regression check for permissive defaults.

## Verification limitation

SQLite received live migration, transactional, CLI, REST, MCP, and snapshot execution. The Postgres path is implemented with cross-backend SQL and compiled by the complete Rust build, but no live Postgres service was available for an end-to-end runtime check. Do not report live Postgres proof until CI or a configured service executes it.

## Publish checkpoint

1. Stage only the seven source files and this handoff document.
2. Commit as one Phase 2 unit, for example `implement governed influence policy`.
3. Push `agent/governed-influence-policy` to `origin`.
4. Open a draft PR with base `agent/governed-influence-evidence-roots`, not `main` while PR #39 remains unmerged.
5. Reference PR #38 and describe this as delivery PR 2 of 5, stacked on PR #39.
6. Verify local SHA, remote SHA, PR head SHA, mergeability, and every GitHub check before reporting success.

Do not merge the plan PR, PR #39, or this Phase 2 PR without explicit user direction.

## Remaining work

### Phase 3: purpose and shared egress gate

- add advisory wire-purpose and verified-purpose types
- implement local-operator and trusted-runtime attestations
- add scoped, expiring, replay-protected human-confirmation receipts
- implement deny-overrides-allow task evaluation and strict risk handling
- route every content-bearing surface through one evaluator
- add influence-event receipts and exact-source expansion controls

### Phase 4: contradiction sets

- add contradiction schema and versioned claim cardinality
- implement deterministic creation, reconciliation, management APIs, and retrieval annotation

### Phase 5: evaluation and Workbench

- add the counterfactual influence evaluation cluster
- add Workbench simulation and evidence controls
- finish compliance, lineage, status metrics, and performance/relevance gates

## Workspace safety

- The removed Desktop checkout is not a source of truth and must not be used.
- Continue Phase 2 only in `/Users/kingjames/Projects/Iron-mem-pr38-policy`.
- PR #39 remains in `/Users/kingjames/Projects/Iron-mem-pr38-implementation`; do not mix its worktree with this branch.
