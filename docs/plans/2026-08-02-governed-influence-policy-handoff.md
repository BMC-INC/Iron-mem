# Governed Influence Policy Handoff

**Updated:** 2026-08-02
**Source plan:** PR #38, `docs/plans/2026-07-31-governed-memory-influence-plan.md`
**Active worktree:** `/Users/kingjames/Projects/Iron-mem-pr38-contradictions-eval`
**Active branch:** `agent/governed-influence-contradictions-eval`
**Stack base:** Phase 3 PR #42 at `5a1a88becfe049b505c7837cb78db13cf9e0891e`
**State:** PRs #38-#41 are merged. Phase 3 PR #42 is open and all eight checks are green. Phases 4 and 5 are implemented together on the active stacked branch and await final publication/CI. Do not merge either open PR until both are green and the user gives the final merge instruction.

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

### Recovery completed 2026-08-02

- Draft PR #41: `Restore offline fact and procedure extraction`, stacked on PR #39.
- Branch/head: `fix/offline-extraction-recovery` at `c2c30ae46589a231e05b100c79356c016fb03442`; local, remote, and PR head matched and the PR was clean/mergeable.
- Complete local verification: strict clippy passed; 244 tests passed, zero failed, one intentional benchmark ignored; clean MCP stdio passed.
- A timestamped pre-recovery SQLite backup was created at `~/.ironmem/backups/pre-offline-extraction-canary-20260802T050000Z-c2c30ae/mem.db` with SHA-256 `21d533fdf9d481e420dbb6f5146987477ae2ce686b0dc300f37fb5e140f7248a`.
- Five-session live canary: 56 facts, eight procedures, 64/64 source-supported items, zero failures, unique child refs, and zero lineage/governance mismatches.
- Full bounded recovery: all 499 preserved archives plus eight nonempty uncompressed post-checkpoint sessions now have completed `local-extractive-v1` receipts; zero eligible candidates remain.
- Receipt totals: 507 complete, zero failed, 1,455 facts, 88 procedures, and 1,543/1,543 source-supported extracted items.
- Deny-safe dedupe avoided 210 redundant fact writes. The 1,320 recovery-owned children have 1,320 distinct source refs, 1,320 derive-ledger entries, and zero evidence-root/depth/namespace/classification mismatches.
- Source archives were not deleted or rewritten. Configuration was unchanged. Recovery disabled `auto`/cloud embeddings and sent no archive content to a provider.
- A release binary with `local-onnx` support was built from PR #41 and deployed locally after preserving the prior executable as `~/.ironmem/bin/ironmem.pre-offline-extraction-c2c30ae`.
- Installed binary SHA-256: `e35b09f865b6ef62b8ccaaac27761e4299929b1897f774a7c1ef77a0c66c6672`, matching the verified release artifact.
- The worker was restarted and `/status` verified `ok=true`, `compression.mode=local`, `cloud_required=false`, and extractor `local-extractive-v1` with 507 completed/zero failed receipts. Future local sessions now use on-device typed extraction.
- `/status` candidate totals are intentionally global and include older pre-checkpoint archives; the timestamp/id-bounded post-checkpoint recovery query returned zero remaining archives and zero remaining nonempty uncompressed sessions.

Runbook: `docs/plans/2026-08-02-offline-extraction-recovery-runbook.md` in PR #41.

### Prerequisite: local extraction recovery

- Completed in PR #41 and the live receipt-backed recovery described above.
- Do not repeat the backfill; re-check receipt status and candidate count if the database changes before deployment.

### Phase 3: purpose and shared egress gate

- Completed in PR #42; all eight GitHub checks are green.
- Every REST, MCP, CLI, Workbench, source-expansion, and file-injection content path uses the shared gate.
- Audit receipts store hashes rather than raw queries/actions; confirmations and attestations are scope-bound and replay-protected.

### Phase 4: contradiction sets

- Implemented on `agent/governed-influence-contradictions-eval` together with Phase 5.
- Includes versioned schema/cardinality, project and user realms, deterministic graph conflict detection, atomic ledgered management, REST/MCP/CLI surfaces, snapshot preservation, and evaluator annotation.

### Phase 5: evaluation and Workbench

- Implemented on the same combined branch.
- Includes the deterministic counterfactual CI cluster, read-only Workbench simulation, policy/roots/contradiction/event evidence controls, compliance and lineage extensions, p50/p99 metrics, and permissive-relevance/unauthorized-release gates.
- Local verification at this checkpoint: 275 passed, 0 failed, 1 ignored plus MCP stdio; strict clippy, diff check, and embedded JavaScript syntax check passed.

## Workspace safety

- The removed Desktop checkout is not a source of truth and must not be used.
- Phase 3 remains in `/Users/kingjames/Projects/Iron-mem-pr38-egress`; Phases 4+5 remain in `/Users/kingjames/Projects/Iron-mem-pr38-contradictions-eval`.
- PR #42 is deliberately open with auto-merge disabled. Publish the cumulative combined PR against `main`, let both PRs turn green, and merge only at the very end in dependency order.
- Do not repeat the completed live extraction backfill unless receipt-backed status proves the database changed and new candidates exist.
