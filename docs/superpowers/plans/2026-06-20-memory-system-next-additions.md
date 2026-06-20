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

## Verification State

Completed successfully:

```bash
CARGO_TARGET_DIR=/tmp/ironmem-codex-target cargo check --bin ironmem
CARGO_TARGET_DIR=/tmp/ironmem-codex-target cargo clippy --bin ironmem -- -D warnings
CARGO_TARGET_DIR=/tmp/ironmem-codex-target cargo test --bin ironmem memory_edges_
CARGO_TARGET_DIR=/tmp/ironmem-codex-target cargo test --bin ironmem relations
CARGO_TARGET_DIR=/tmp/ironmem-codex-target cargo test --bin ironmem -- --skip context::tests::git_query_returns_signal_for_this_repo
CARGO_TARGET_DIR=/tmp/ironmem-codex-target cargo test --test mcp_stdio_clean
```

Full unskipped `cargo test --bin ironmem` was interrupted because the existing `context::tests::git_query_returns_signal_for_this_repo` hung inside `git diff --name-only HEAD`. The graph tests passed before the hang.

## Immediate Next Step

Commit the Temporal Graph Lite slice as one atomic commit after a final `git diff` review.

Suggested message:

```text
feat: add temporal memory graph
```

## Remaining Additions

### 1. Graph-Aware Retrieval Fusion

Goal: make graph edges influence search, not only explicit `graph` queries.

Implementation:

- Extract candidate entities from the query.
- Fetch active `memory_edges` where source or target matches those entities.
- Add the edge memory ids as a third retrieval signal in `hybrid_search`.
- Keep the signal gated or weighted so broad entities do not flood results.
- Add tests where FTS/vector miss but graph entity relation surfaces the right memory.

Risk: existing entity signal was disabled because recency-ordered entity matches hurt LoCoMo precision. Graph fusion must rank by relation relevance, not just entity recency.

### 2. Reconciliation CLI and MCP Maintenance Tool

Goal: expose reconciliation as an operational command, not just insert-time behavior.

Implementation:

- Add `ironmem reconcile --project ... --all`.
- Add MCP `reconcile_memory_graph`.
- Scan existing edges and mark duplicates/current-state supersessions.
- Report counts: scanned, duplicates, current-state updates, active edges.
- Add a dry-run option.

### 3. Relation Backfill for Existing Memories

Goal: populate the graph from old memories that predate `RELATIONS:`.

Implementation:

- Add `ironmem graph-backfill --project ... --all --limit ...`.
- For each memory without graph edges, ask the configured provider to emit `RELATIONS:` from the stored summary and tags.
- Store edges with the original memory id as provenance.
- Respect local-first posture: skip gracefully when no provider/API key is configured.
- Do not mutate memory summaries.

### 4. Better Temporal Semantics

Goal: separate valid time from ingestion time more cleanly.

Implementation:

- Keep `observed_at` as ingestion/provenance time.
- Add explicit parser validation for `YYYY-MM-DD` and date ranges.
- Support query-at-time retrieval: active edges where `valid_from <= time < valid_until`.
- Add CLI/MCP args like `--at 2026-06-20` / `at_time`.

### 5. Procedural Memory

Goal: store durable "how work should be done" instructions separately from facts and events.

Implementation:

- Add memory kind `procedural`.
- Add prompt extraction for durable behavioral rules and workflow preferences.
- Make injection rank procedural memories high, below profile but above ordinary sessions.
- Operator OS should use procedural memory for tenant/team/project operating rules.

### 6. Operator OS Memory Adapter

Goal: make Operator OS consume IronMem as a memory fabric, not a note sidecar.

Implementation:

- Define an adapter contract around `memory_graph`, `remember`, `search_memories`, `get_profile`, and `retrieve_original`.
- Map Operator OS concepts:
  - tenant -> project/user scope boundary
  - agent worker -> source entity
  - work item -> target entity
  - handoff/heartbeat -> relation/event
  - audit receipt -> provenance memory
- Keep tenant isolation explicit before any shared/team memory surface.

### 7. Evaluation Harness

Goal: make memory quality measurable.

Implementation:

- Add `ironmem eval` as a wrapper for repeatable suites.
- Keep LoCoMo dry-run gates for temporal/fact recall.
- Add LongMemEval-style checks for knowledge updates and abstention.
- Add a coding-agent memory benchmark: prior bug, file decision, command failure, fix, later recall.
- Record results under `results/` or `docs/evals/` with command, model, and commit hash.

## Design Constraints

- No placeholder code. Each slice must be complete and testable.
- Local-first remains the default. No hosted graph database dependency.
- Preserve history. Reconciliation should mark superseded facts, not delete them.
- Operator OS integration should treat memory as a product primitive: worker context, shared board memory, handoffs, heartbeats, audit, and graph export.
- Do not regress coding-session memory quality while improving conversational/temporal recall.

