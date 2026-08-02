# Governed Influence Policy Handoff

**Updated:** 2026-08-02
**Source plan:** PR #38, `docs/plans/2026-07-31-governed-memory-influence-plan.md`
**Active worktree:** `/Users/kingjames/Projects/Iron-mem-pr38-policy`
**Active branch:** `agent/governed-influence-policy`
**Stack base:** `agent/governed-influence-evidence-roots` at `10c20feeb3dc93ed71bc5bc8703c528874feda22` (draft PR #39)
**State:** Phase 2 published as draft PR #40. Phase 3 is explicitly paused until the urgent local-extraction recovery described below is implemented and verified.

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

## Published state

- Phase 2 implementation commit: `7dc3c0df19efcdc26ad82a0305428484002b1c2f`.
- Draft PR: #40, `Implement governed influence policy`.
- Base: `agent/governed-influence-evidence-roots` (draft PR #39).
- Local, remote, and PR implementation SHAs matched at publication.
- PR #40 was clean and mergeable.
- GitHub CI does not trigger while the stacked PR targets a non-`main` base. The exact Phase 2 code passed the complete local suite and strict lint; PR #39 passed all eight remote checks, including Windows.

Do not merge the plan PR, PR #39, or this Phase 2 PR without explicit user direction.

## Critical recovery checkpoint before Phase 3

### Incident summary

The post-publication runtime audit found that IronMem remained healthy as a local session archive but stopped producing typed fact and procedure memories after the local-first deployment on 2026-07-17.

This corrects the earlier, incomplete interpretation that slow memory-count growth was explained by empty or uncompressed sessions. Session lifecycle fragmentation explains why started sessions and memories are not one-to-one, but it does not explain the complete typed-extraction stop.

Stable database checkpoints prove the discontinuity:

- 2026-07-17 checkpoint: 30,298 total memories, 2,373 compressed sessions, and 29,141 fact memories.
- Current audit: 30,865 total memories, 2,872 compressed sessions, and the same 29,141 fact memories.
- Net after the checkpoint: 499 additional compressed sessions and zero additional facts.
- The 567 post-checkpoint memory IDs consist of 499 session archives, 64 `error_solution` memories, and four explicit/configuration memories. None are facts or procedures.
- Procedure-memory count was zero before the checkpoint and remains zero.

The server log agrees with the database:

- Last nonzero extraction: 2026-07-17T03:14:24Z, 83 facts.
- First zero-fact compression after the local-first deployment: 2026-07-17T17:11:37Z.
- The next 508 logged compressions produced zero facts and zero procedures.

### Root cause

Commit `9146b71bc51b97da8dcd5421632fc6d5ebe03a3d` made deterministic local compression the default. The deployed `~/.ironmem/settings.json` still names the Vertex provider but has no `compression` block, so serde defaults it to:

```json
{
  "compression": {
    "mode": "local"
  }
}
```

`CompressionMode::Local` calls `local_compression_result`, which writes a searchable session archive, CCR transcript, chunks, embeddings, and explicit machine-readable relation markers. It deliberately leaves `facts` and `procedures` empty. Only `cloud_with_local_fallback` calls the provider extraction path.

This behavior guaranteed offline graduation but created a product regression: local-first became archive-only rather than full on-device memory extraction. The healthy `/status` response did not expose the zero-extraction streak.

Do not fix this by silently changing the deployed configuration to `cloud_with_local_fallback`. That would send session material to Vertex when credentials are available and would not satisfy genuine offline-first extraction.

### Recovery evidence

The missing layer is derived data; the captured sources were not deleted.

For the 499 surviving post-checkpoint session archives:

- 499 of 499 have a `memory_meta.session_blob` transcript reference.
- 499 of 499 referenced CCR blobs are physically present.
- 486 blobs use `dict+zstd`; 13 use `zstd`.
- all 499 have searchable memory chunks, 8,984 chunks total.
- the archives cover 8,485 captured observations.
- none of those observations required a separate large-output overflow blob, so the CCR transcript contains the complete observation data IronMem captured rather than only a truncated preview.

There are also eight recent nonempty, uncompressed sessions containing 94 observations. They remain in the database and can be processed after the extraction fix.

Recovery is therefore expected for the preserved data. CCR loading must verify every decompressed blob hash during backfill and fail closed on corruption. If a work detail was never captured by IronMem, scan the original Codex/Claude session histories as a separate recovery source. Keep genuine-work recovery isolated from LoCoMo or other synthetic benchmark data.

No configuration was changed and no archived session content was sent to Vertex or any other online provider during this audit.

### Required recovery PR

Pause Phase 3 and implement a dedicated prerequisite recovery PR. It is not a partial Phase 3 implementation and should remain separately reviewable.

The recovery PR must deliver a complete solution:

1. Genuine on-device fact and procedure extraction for local compression. Offline mode must not depend on cloud credentials, network access, or a hosted service.
2. Optional `cloud_with_local_fallback` enrichment that remains explicitly opt-in and never changes local storage ownership.
3. A durable, idempotent extraction receipt keyed by source memory/transcript hash and extractor version so retries cannot duplicate child memories.
4. A dry-run mode and checkpoint/resume support for long backfills.
5. Provenance-preserving child writes using the existing parent memory, evidence root, derivation depth, governance, event-time, embedding, and ledger paths.
6. Deny-safe deduplication that never deletes or rewrites the source session archive.
7. A canary backfill over a small selection of long, content-bearing sessions. Measure facts/procedures per observation, source-span coverage, duplicates, latency, and retrieval quality before the full run.
8. Full local backfill of the 499 archived transcripts plus the eight nonempty uncompressed sessions only after the canary passes.
9. Status and compliance metrics for extraction mode, last successful extraction, facts/procedures per compression, zero-yield streak, backfill progress, failures, and source coverage.
10. A reason-coded zero-yield warning/alert that distinguishes an actually content-free session from a configured archive-only or failed extractor.

Before any backfill mutation:

- create a new timestamped database backup and verify its checksum;
- keep the live server local/offline;
- inventory exact candidate IDs without reading content into chat;
- verify a representative CCR blob round trip;
- record baseline counts by memory kind and source type;
- run the canary in dry-run mode first.

Do not delete, replace, or bulk rewrite the 499 session archives. Do not backfill synthetic benchmark conversations into the genuine-work namespace.

## Remaining work

### Prerequisite: local extraction recovery

- Implement and publish the complete recovery PR above.
- Verify the on-device extractor and canary backfill.
- Backfill preserved local transcripts safely and report exact receipts.
- Resume the approved governed-influence sequence only after recovery succeeds.

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
- Do not implement the extraction recovery in the Phase 2 worktree or append it to PR #40.
- Create a dedicated Projects worktree and branch for the recovery PR. Because complete recovered child provenance depends on Phase 1 evidence roots, use `agent/governed-influence-evidence-roots` as the stack base while PR #39 is unmerged, then re-evaluate and simplify the PR bases after PR #39 lands.
- Before creating that worktree, re-check `origin`, PR #39, PR #40, and all branch SHAs rather than trusting this snapshot as current.
