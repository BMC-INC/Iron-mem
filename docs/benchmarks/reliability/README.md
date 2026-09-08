# Coding evidence and reliability

These tools distinguish executable correctness, bounded reliability measurements,
and model task performance. None automatically establishes competitive superiority.
All included inputs are public synthetic fixtures. No personal memories are needed.

## Executable coding tasks

```sh
cargo build --release --locked
python3 scripts/benchmark_coding.py --binary target/release/ironmem --out /tmp/coding-harness.json
```

The default adapter deliberately chooses fixture implementations. This validates
retrieval plumbing and hidden checks, **not AI accuracy**. Four small Python tasks
cover a money representation decision, a corrected timezone assumption, a retry
regression, and reuse of an existing slug helper. Every arm receives the same task,
starter files, model settings and memory-byte ceiling. Arms are shuffled using a
fixed seed and run serially. Each implementation runs in a fresh temporary folder;
hidden checks and fixture solutions are not sent to an external model.

`no_memory` has no prior history. `full_history` receives prior messages in order,
truncated to the same byte ceiling. `ironmem` uses the real lexical search and
shared governance path, rendered with the production byte-budget function. This
baseline does not claim to measure optional semantic reranking or working sets.
Budgets apply to memory context; the identical task prompt/starter files are extra.
No-memory latency excludes storage work by design. Reuse is an executable import
check, not a measurement of developer time saved.

For a scored run, `--agent` accepts a JSON descriptor with `command` (an argv
array), `model`, `version`, and `settings`. The trusted local command reads one
JSON object from stdin and writes `{"files":{"solution.py":"..."}}` to stdout.
Its input contains `instruction`, `files`, `context`, `seed`, `model`, and `settings`.
An optional `measurement` object is retained. Commands and generated solutions
execute locally: use only trusted adapters and disposable, non-sensitive fixtures.
A timeout bounds each adapter and check. This is not an OS security sandbox.

The included `scripts/ollama_coding_adapter.py` only contacts a loopback endpoint,
refuses cloud-backed models, requires a pinned installed model digest, bypasses
proxy environment settings, and never downloads a model. Its descriptor can be:

```json
{
  "command": ["python3", "scripts/ollama_coding_adapter.py"],
  "model": "deepseek-r1:8b",
  "version": "ollama-0.17.4",
  "settings": {
    "endpoint": "http://127.0.0.1:11567",
    "model_digest": "6995872bfe4c521a67b32da386cd21d5c6e819b6e0d62f79f64ec83be99f5763",
    "temperature": 0,
    "num_ctx": 4096,
    "num_predict": 768
  }
}
```

Use your installed model's actual digest; a mismatch is an error. Run Ollama with
cloud access disabled, then pass the descriptor with `--agent /path/to/agent.json`.
The runner does not inherit API keys or database overrides. Adapter specifications
must not contain secrets because they are included in reports.

`--retriever` adds a named competitor arm through a descriptor containing `name`,
`version`, and `command`. Its JSON input contains `case_id`, `memories`, `query`,
`budget_bytes`, and `reset:true`; its output is `{"context":"..."}`. It must reset
its own state for each call and use exactly the supplied histories. Excess context
is an error, not silently accepted. A competitor adapter is not supplied or scored
by default; do not label the three built-in arms a vendor comparison.

The runner pins a temporary executable copy for the whole run, so rebuilding the
checkout cannot silently change an arm midway through execution. Adapter or
retrieval errors are reported separately from executed coding checks. Exploratory
paired intervals cluster results by task, rather than treating budgets/repeats as
independent tasks. With four tasks these intervals provide weak population evidence.

Reports retain failures, generated implementations, per-task outcomes, context
hashes, model measurements and binary/fixture/runner identities. A failed coding
test is a scored outcome. A failed deterministic harness exits nonzero. Outputs
are create-only so an earlier measurement cannot silently be overwritten.

## Recovery and scale

```sh
python3 scripts/benchmark_reliability.py --binary target/release/ironmem \
  --build-profile release --sizes 100,1000,10000 --samples 10 --cycles 20 \
  --out /tmp/reliability.json
```

This creates isolated SQLite stores, with synthetic summaries of approximately
1 KB. It verifies process exit before commit, `SQLITE_FULL` from page exhaustion,
twelve real CLI writes with four concurrent workers, corrupted-checkpoint refusal,
and repeated full/delta recovery. Scale measurements include CLI startup,
migration, lexical diagnostics and policy processing, not only an in-process
search. First calls and subsequent samples are separate; operating-system cache
and machine load are uncontrolled. Report the actual duration and sizes tested.
These bounded cycles are not proof of months-long reliability or PostgreSQL scale.

For real filesystem exhaustion, `--disk-full-root` accepts only a separate mounted,
disposable filesystem of at most 128 MiB. It fills that filesystem until ENOSPC,
checks a failed write preserves the existing memory, reclaims space and verifies a
new write. It refuses an ordinary directory. Provision and remove the disposable
mount outside the runner. Never point it at an existing data volume.

The concurrency fixture exposed a startup-repair race: memory insertion published
the FTS row before its relational metadata. Creation now commits the row, metadata
and primary evidence root together, and legacy metadata repair tolerates another
migrator inserting the same parent. A fault-injection unit test also verifies a
failed evidence insert leaves no partial memory.

## Interpretation

To support a competitive claim, run representative repository tasks against pinned
competitor versions, the same local model and hardware, and matched exposure
budgets. Publish all failures and paired uncertainty by task, including stale-fact,
source-recovery, abstention and policy-isolation cases. A small positive canary is
useful evidence for its four tasks; it is not a leaderboard or general superiority.

## Initial local-model pilot

The retained [Phi-3 pilot](local-model-pilot.json) failed all four tasks in all
three arms. Failures include incorrect return types, invalid Python, invented
dependencies and missing functions. It used an earlier adapter prompt without an
explicit standard-library-only environment. A debug rebuild also overlapped that
pilot, so its final binary hash does not identify every invocation. It is retained
as a failed pilot, not used for competitive claims. The final runner pins the
executable and the adapter now spells out the execution environment consistently
for every arm. A separate trivial addition task checks the adapter contract before
the next scored run; hidden coding checks are never shown to the model.
