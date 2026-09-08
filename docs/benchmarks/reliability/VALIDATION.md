# Integrated validation — 2026-09-07

Implementation and local validation completed on macOS. The measured build
(preceding the final diagnostic filesystem-scope restriction) has Rust source fingerprint:
`24762423d2a2e5d0f92dd51fa2e61a9247b59d79f46bdf07bd14f4e65c1a681e`.
Release binary SHA-256:
`aec12e23dc73202c3388f17266fa513376109d87b20d8f4e0beb5dd7fa702901`.
The fingerprint follows build.rs (Cargo manifests/build script and sorted Rust
source paths/bytes); it is not a claim that a dirty parent Git commit identifies
uncommitted code. Debug and release build outputs recorded that same fingerprint.
A subsequent CodeQL-driven change limits diagnostic disk reads to the process
working directory. Focused diagnostics tests and PR CI validate that change;
these benchmark results retain the original measured binary identity.

## Correctness and integration

- Complete Rust suite: **316 unit tests passed**, zero failed; four intentionally
  ignored tests. Three PostgreSQL tests were run explicitly and passed; the fourth
  is the optional CCR dictionary timing benchmark.
- Real MCP stdio integration passed. After isolating its settings file, the focused
  transport test was rerun and passed.
- PostgreSQL: storage/checkpoints, access/mutations, and temporal assertions/snapshots
  all passed against a disposable local PostgreSQL instance, then it was stopped
  and removed. No personal database was used.
- All-target Clippy with `-D warnings`, formatting and diff checks passed.
- Deterministic quality evaluation: **72/72 passed**. Both seven-budget density
  smoke suites plus their full-context baselines completed without model calls.
- Five local Python integration tests cover explicit install paths, preserved
  settings/hooks, idempotence, native SQLite backup, real MCP initialization/tool
  listing/diagnostics, export/import recovery, purge refusal/cleanup, and streamed
  installer bootstrap/temporary-checkout cleanup. The streamed bootstrap test mocks
  the remote clone transport while executing the real installer.
- CLI/REST/MCP diagnostics tests cover namespace/capability refusal and omitted
  content. A blocked-policy fixture verifies no source summary or recall count
  escapes through an explicit diagnostic content request.

## Recovery and performance

The [200-cycle run](recovery-200.json) completed 200 full/delta recovery cycles in
189.6 seconds. It also verified interrupted transactions, page exhaustion,
four concurrent CLI writers producing twelve distinct memories, and corruption
refusal before destructive restore. A local model workload overlapped that run;
its latency figures are preserved but not used as idle-machine measurements.

The [real disk-full rehearsal](disk-full.json) used a disposable 32 MiB HFS+ disk
image. Actual ENOSPC caused a write failure without losing the existing memory;
reclaiming space allowed another write. The image was unmounted and removed.

The [separate release scale run](release-scale.json) ran after task-owned model and
compiler work stopped. Each size has twenty subsequent samples after a separately
recorded first call. OS cache and unrelated host load remain uncontrolled.

| Memories | Median CLI diagnostics | p95 | Database plus WAL after checkpoint |
|---:|---:|---:|---:|
| 100 | 46.5 ms | 48.1 ms | 704,512 bytes |
| 1,000 | 56.2 ms | 60.3 ms | 2,457,600 bytes |
| 10,000 | 153.1 ms | 161.7 ms | 19,685,376 bytes |

These include process startup, migration and diagnostics. The corpus contains
synthetic ~1 KB summaries. This is bounded SQLite evidence, not a production SLO,
PostgreSQL scale result, or proof of months-long operation.

## Coding outcomes, including failures

The [deterministic harness](coding-harness.json) passed all twelve IronMem and all
twelve bounded full-history checks across three budgets. Its no-memory fixture
implementations fail by design. These are harness checks, never model accuracy.

The [initial Phi-3 pilot](local-model-pilot.json) failed all twelve attempts and is
retained with its prompt/build-identity limitations. The adapter environment was
then clarified uniformly and validated with a separate trivial addition function.
The [pinned local DeepSeek run](local-model-coding.json) used existing
`deepseek-r1:8b` Q4_K_M weights, Ollama 0.17.4, CPU inference, temperature zero,
seed zero, a 4,096-token model window, a 768-token generation ceiling, and a
2,000-byte memory budget. No models were downloaded and no cloud calls were made.

| Arm | Passed | Executed checks | Infrastructure errors |
|---|---:|---:|---:|
| No memory | 0 | 4 | 0 |
| Bounded full history | 2 | 3 | 1 timeout |
| IronMem lexical/rendered context | 0 | 4 | 0 |

**This run demonstrates no coding-accuracy advantage for IronMem.** IronMem-arm
failures were a double-escaped source response, incorrect timezone parsing,
failure to re-raise the original retry exception, and duplicating the slug helper.
Full history passed retry and helper reuse; its timezone attempt timed out. The
raw report retains generated implementations and test failures. No vendor memory
system was run, and no competitive-superiority claim is supported.

The corpus has only four synthetic tasks and no distractor corpus; query terms
are also used as memory tags. It measures downstream use of supplied context,
not broad retrieval discrimination or real developer time saved. The task-cluster
bootstrap interval is exploratory and weak at this sample size. Initial compilation
and later recovery work overlapped the model run; model timings are not isolated
latency claims. The executable itself was pinned before the run.

The runner subsequently froze the fixture render date to 2026-09-08 UTC. **All
12 attempt context hashes match** the scored run, so this removes future clock
drift without relabeling or rerunning its outcomes. The report retains the runner
hash actually sampled at the start of the scored run. No prompt was tuned to the
hidden check results after this run to manufacture a win.

## Release boundaries

The five implementation areas are present, with explicit limits: diagnostics are
lexical previews; physical purge is whole-store native SQLite/FTS and refuses
unknown virtual modules; other backends require operator-managed retention;
newcomer automation verifies protocol/CLI behavior rather than every client GUI;
and comparative model quality remains unproven. Existing experimental defaults
remain unchanged. No installed IronMem binary, service, settings or active personal
memory store was upgraded. Public artifacts contain synthetic fixtures only.
