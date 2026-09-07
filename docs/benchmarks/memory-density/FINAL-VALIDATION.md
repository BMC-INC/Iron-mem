# Integrated upgrade validation

Local validation completed on 2026-09-07. The final Rust source fingerprint is `3a23a3e496f2b56daefe003803a35a1691bf0a70017c008527f4a8505dbbfd78` (the build.rs hash of Cargo manifests/build script and sorted Rust sources). Reports were produced from the working tree based on commit `0c66eac`; their recorded parent commit is not the identity of the uncommitted implementation. The source fingerprint and checkpoint binary hash identify the tested implementation. Remote CI checks the subsequent PR commit separately.

## Results

| Check | Result |
|---|---|
| Default-feature `cargo build` | Passed |
| Complete `cargo test` | 310 unit tests and 1 MCP stdio integration test passed; zero failures |
| PostgreSQL 16.13 | 3 explicit integration tests passed: storage/checkpoints, telemetry/mutations, assertions/temporal snapshots |
| `cargo clippy --all-targets -- -D warnings` | Passed, including test targets |
| `cargo fmt --check`, `git diff --check` | Passed |
| Deterministic quality evaluation | 72/72 passed; [raw report](final-eval.md) |
| LongMemEval smoke | Full-context baseline and all seven budgets, two synthetic questions each; no harness errors |
| LoCoMo smoke | Full-context baseline and all seven budgets, one included synthetic question each; no harness errors |
| Temporal assertion fixture | 102 independently expected time points passed, alongside conflict, retraction, restriction, concurrency and recovery checks |
| Checkpoint rehearsal | Full/delta restoration and independent export validated; unchanged snapshot reused its exact payload |

The four tests ignored by default are the three explicit PostgreSQL tests (run separately above) and the optional CCR dictionary timing benchmark. No correctness test is silently counted as passing when ignored. ONNX/GPU backends were not changed or rebuilt for this batch.

Commands use `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0` to avoid duplicate debug-symbol storage. The final complete suite was rerun after source-order fixes required by all-target Clippy. PostgreSQL used a temporary local database, not the installed memory store.

```sh
cargo build
cargo test -- --nocapture
cargo clippy --all-targets -- -D warnings
cargo fmt --check
git diff --check
# Requires IRONMEM_TEST_POSTGRES_URL pointing to an isolated PostgreSQL database:
cargo test --bin ironmem -- postgres_foundations access_postgres assertions_postgres --ignored --test-threads=1
cargo run --quiet -- eval --out /temporary/eval
python3 scripts/run_memory_density.py --suite longmemeval --data tests/fixtures/density/longmemeval.json --out /temporary/lme
python3 scripts/run_memory_density.py --suite locomo --data tests/fixtures/density/locomo.json --out /temporary/locomo
python3 scripts/benchmark_checkpoints.py --out /temporary/checkpoints
```

## Measurements and limits

[Smoke results and all run manifests](final-smoke.json) retain the exact source and dataset hashes. These tiny fixtures verify execution and resume identity, not retrieval noninferiority. `accuracy` remains null. Model names in unscored manifests and the deterministic eval header are configuration labels; no answer/judge model calls were made. Private datasets and paid scored evaluations were not run.

[Temporal measurements](temporal-assertions.json) compare the complete assertion delivery path to a lower-level independent-memory read. The fixture's database grew from 524,288 to 532,480 bytes after ten events, ledger receipts and delivery observations. It retains two independent evidence memories. This is overhead measurement, not a ten-memory semantic compression comparison. Median/p95 assertion delivery was 10,918/13,687 microseconds versus 2,289/3,290 microseconds for the lower-level read. The assertion path additionally verifies history, applies governance and records delivery. Machine load and OS cache were uncontrolled during local debug tests. No production speed or storage benefit is inferred; default enablement remains false.

[Checkpoint measurements](final-checkpoints.json) retain the binary/script hashes and explicitly mark the dirty source checkout. A 50-memory fixture stored its full checkpoint in 2,254 compressed bytes and its one-row delta in 558 bytes; unchanged state reused the full object's hash. These synthetic timings include process startup and migration and do not guarantee production performance.

The earlier [interning profile](metadata-interning.json) remains the evidence for deferring a schema migration. Its script and method are unchanged. Existing [access timing](access-overhead.json) describes the earlier focused E/F checkpoint; it is not relabeled as a new production measurement.

## Plan reconciliation

| Phase | Completed implementation | Deliberate limit or adjustment |
|---|---|---|
| A | Exact bounds, stable handles, truthful injection reports | Merged as PR #45 |
| B | Escaped context, reproducible density accounting, paired budget harness and plots | Scored accuracy/noninferiority remains unmeasured |
| C | Verified content-defined CCR, legacy reads, recovery/conversion, shared-object GC | Writes remain opt-in; synthetic savings do not establish acceptable production latency |
| D | Complete atomic project checkpoints, independent export, verified bounded deltas | Complete row-set differences replace an incomplete legacy mutation journal; capture still scans state |
| E | Delivery telemetry, temperature, opt-in profiles, governed fresh context, invalidation | No content cache or cloud tier; operational counters reset on project restore |
| F | Net-size/join-cost profiling and a declared migration gate | No representative material benefit demonstrated; no interning migration added |
| G | Scoped immutable event journal, atomic ledger receipts, version conflicts, current/as-of/history queries, full/delta recovery, public APIs and fixtures | Pointers are derived at query time; local immutable history is merged on restore; semantic payload compression and default rollout await benefit evidence |

The G journal covers assertion events, not every legacy memory mutation. Existing correction mining and contradiction sets remain compatible through evidence/governance rather than an automatic prose-to-assertion conversion. No missing historical evidence is fabricated. Full database backups are still required when historical operational counters or complete namespace audit chains must be retained.

Final review also corrected PostgreSQL CI coverage, preserved user-edited generated files during restore, removed test-module placement warnings, and corrected the checkpoint benchmark's unsupported claim about concurrent machine load.

## Publication and cleanup boundaries

Only implementation files, public documentation, fixture code and small synthetic reports are included. Generated `IRONMEM.md`, personal memory databases, local `data/`, research notes and source archives are excluded. Ignore rules cover local memory database files and corpora; commits use an explicit file allowlist. Existing unrelated research files and experimental checkouts are preserved.

Temporary test/profile databases and repeated benchmark outputs are removed after their small reports are retained. Only the existing checkout's shared build cache is reused; no extra checkout is needed. Cleanup removes the disposable PostgreSQL instance and incremental build cache. No installed binary, live memory service, production configuration, or active memory database is upgraded by these commits.
