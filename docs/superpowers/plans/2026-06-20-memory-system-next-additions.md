# IronMem Memory System Next Additions

> Durable handoff for future IronMem and Operator OS memory work. This file exists so conversation compaction does not lose the roadmap.

## Current Completed Slice

Temporal Graph Lite is implemented in the working tree:

- `memory_edges` table stores structured `source -> relation -> target` edges with `memory_id` provenance, dates, confidence, supersession fields, and created/observed timestamps.
- Compression prompt emits `RELATIONS:` alongside `SUMMARY`, `FACTS`, `WHEN`, and `ENTITIES`.
- Parsed relations persist automatically during compression.
- Exact duplicate edges are marked as `duplicate`.
- Older active current-state edges for the same source and relation are marked `current_state_update` and closed with `valid_until`.
- Query surfaces exist through:
  - CLI: `ironmem graph "Operator OS" --history`
  - REST: `GET /graph?entity=Operator%20OS&history=true`
  - MCP: `memory_graph`
- `status` includes `memory_edges`.
- Memory purge also deletes graph edges.
- Graph-aware retrieval fusion is implemented in `hybrid_search`: active graph edges are retrieved for entity phrases in the query, relation-ranked by source/relation/target overlap, and fused through RRF with FTS/vector/temporal signals.
- Graph reconciliation is available through `ironmem reconcile` and MCP `reconcile_memory_graph`, with dry-run counts for scanned, duplicate, current-state, and active edges.
- Relation backfill is available through `ironmem graph-backfill`; it asks the configured provider for `RELATIONS:` for memories without graph edges and skips cleanly when no provider/API key is configured.
- Graph queries support valid-time filtering through CLI `--at`, MCP `at_time`, and REST `at`.
- Procedural memories are first-class: compression extracts `PROCEDURES:`, persists them as `kind=procedural`, and injection ranking boosts them below profile but above ordinary sessions.
- Operator OS integration is specified in `docs/operator-os-memory-adapter.md`.
- `ironmem eval` runs deterministic graph, temporal-update, and procedural-ranking checks and writes markdown reports with command, model, and commit metadata.

## Verification State

Completed successfully:

```bash
CARGO_TARGET_DIR=/tmp/ironmem-codex-target cargo check --bin ironmem
CARGO_TARGET_DIR=/tmp/ironmem-codex-target cargo clippy --bin ironmem -- -D warnings
CARGO_TARGET_DIR=/tmp/ironmem-codex-target cargo test --bin ironmem memory_edges_
CARGO_TARGET_DIR=/tmp/ironmem-codex-target cargo test --bin ironmem relations
CARGO_TARGET_DIR=/tmp/ironmem-codex-target cargo test --bin ironmem -- --skip context::tests::git_query_returns_signal_for_this_repo
CARGO_TARGET_DIR=/tmp/ironmem-codex-target cargo test --test mcp_stdio_clean
CARGO_TARGET_DIR=/tmp/ironmem-codex-target cargo test --bin ironmem graph_signal
CARGO_TARGET_DIR=/tmp/ironmem-codex-target cargo test --bin ironmem query_graph_entities
CARGO_TARGET_DIR=/tmp/ironmem-codex-target cargo test --bin ironmem reconcile_memory_graph
CARGO_TARGET_DIR=/tmp/ironmem-codex-target cargo test --bin ironmem memories_without_edges
CARGO_TARGET_DIR=/tmp/ironmem-codex-target cargo test --bin ironmem procedures
CARGO_TARGET_DIR=/tmp/ironmem-codex-target cargo test --bin ironmem validates_temporal_dates
CARGO_TARGET_DIR=/tmp/ironmem-codex-target cargo run --bin ironmem -- eval --out /tmp/ironmem-evals
```

Earlier in the Temporal Graph Lite slice, full unskipped `cargo test --bin ironmem` was interrupted because `context::tests::git_query_returns_signal_for_this_repo` hung inside `git diff --name-only HEAD`. On 2026-06-20 after the later slices, that test passed directly; run the full unskipped suite before committing.

## Immediate Next Step

Run final verification and commit the completed memory-system expansion.

```text
feat: expand temporal memory system
```

## Remaining Additions

### 1. Graph-Aware Retrieval Fusion

Status: completed.

Goal: make graph edges influence search, not only explicit `graph` queries.

Implementation:

- Extracts candidate capitalized entity phrases from the query.
- Fetches active `memory_edges` where source or target matches those entities.
- Adds edge provenance memory ids as another retrieval signal in `hybrid_search`.
- Keeps the signal gated by max entity count and per-entity edge caps.
- Ranks graph candidates by relation/source/target overlap before RRF fusion.
- Covers the FTS/vector miss case with `hybrid_search_graph_signal_surfaces_relation_ranked_memory`.

Risk: existing entity signal was disabled because recency-ordered entity matches hurt LoCoMo precision. Graph fusion must rank by relation relevance, not just entity recency.

### 2. Reconciliation CLI and MCP Maintenance Tool

Status: completed.

Goal: expose reconciliation as an operational command, not just insert-time behavior.

Implementation:

- Added `ironmem reconcile --project ... --all --dry-run`.
- Added MCP `reconcile_memory_graph`.
- Scans existing edges and marks duplicates/current-state supersessions.
- Reports counts: scanned, duplicates, current-state updates, active edges.
- Covered by `reconcile_memory_graph_dry_run_then_marks_legacy_edges`.

### 3. Relation Backfill for Existing Memories

Status: completed.

Goal: populate the graph from old memories that predate `RELATIONS:`.

Implementation:

- Added `ironmem graph-backfill --project ... --all --limit ... --dry-run`.
- For each memory without graph edges, asks the configured provider to emit `RELATIONS:` from the stored summary and tags.
- Stores edges with the original memory id as provenance.
- Respects local-first posture: skips gracefully when no provider/API key is configured.
- Does not mutate memory summaries.

### 4. Better Temporal Semantics

Status: completed.

Goal: separate valid time from ingestion time more cleanly.

Implementation:

- Keeps `observed_at` as ingestion/provenance time.
- Adds explicit parser validation for `YYYY-MM-DD` and `YYYY-MM-DD..YYYY-MM-DD` ranges.
- Supports query-at-time retrieval with `valid_from <= time < valid_until`.
- Adds CLI `--at 2026-06-20`, MCP `at_time`, and REST `at`.

### 5. Procedural Memory

Status: completed.

Goal: store durable "how work should be done" instructions separately from facts and events.

Implementation:

- Added memory kind `procedural`.
- Added prompt extraction through `PROCEDURES:`.
- Persists procedures as separate project-scoped `kind=procedural` memories.
- Injection rank boosts procedural memories below profile but above ordinary sessions.
- Operator OS adapter uses procedural memory for tenant/team/project operating rules.

### 6. Operator OS Memory Adapter

Status: completed.

Goal: make Operator OS consume IronMem as a memory fabric, not a note sidecar.

Implementation:

- Defined `docs/operator-os-memory-adapter.md` around `memory_graph`, `remember`, `search_memories`, `get_profile`, `retrieve_original`, and `reconcile_memory_graph`.
- Mapped Operator OS concepts:
  - tenant -> project/user scope boundary
  - agent worker -> source entity
  - work item -> target entity
  - handoff/heartbeat -> relation/event
  - audit receipt -> provenance memory
- Kept tenant isolation explicit before any shared/team memory surface.

### 7. Evaluation Harness

Status: completed.

Goal: make memory quality measurable.

Implementation:

- Added `ironmem eval` as a wrapper for repeatable deterministic suites.
- Current suite covers graph relation recall, temporal knowledge updates, and procedural ranking.
- Reports are written under `docs/evals/` by default with command, model, and commit hash.
- The harness is intentionally local/no-network so it can run in CI and before commits.

## Design Constraints

- No placeholder code. Each slice must be complete and testable.
- Local-first remains the default. No hosted graph database dependency.
- Preserve history. Reconciliation should mark superseded facts, not delete them.
- Operator OS integration should treat memory as a product primitive: worker context, shared board memory, handoffs, heartbeats, audit, and graph export.
- Do not regress coding-session memory quality while improving conversational/temporal recall.
