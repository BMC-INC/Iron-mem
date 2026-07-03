# HANDOFF - 2026-07-02 - IronMem Sleep Cycle / Auto-Compression

## Resume Here

Build IronMem's real "sleep cycle" feature: unattended idle-session compression plus scheduled reflection/dream consolidation, with cloud-safe Vertex auth. This should be implemented as a product feature in IronMem, not only a local cron script.

The immediate product decision is settled:

- Do not rely on user ADC from `gcloud auth application-default login` for unattended automation.
- Do not make a service account JSON key the default cloud story.
- Prefer attached service accounts on GCE/Cloud Run/Cloud Scheduler for cloud runs.
- For local macOS unattended mode, support a service account key path only as an explicit advanced option, stored outside the repo and documented with rotation/security warnings.
- Add a proper `ironmem sweep` command and a scheduler/daemon path. A launchd plist can wrap it, but cron/launchd should not own the core logic.

## Current Repo State

Use the non-iCloud source checkout:

```bash
cd /Users/kingjames/Projects/Iron-mem-fix
```

Current known dirty state before this handoff:

- `phase1_provider_DRAFT.patch` is an existing untracked file. Do not touch it unless explicitly asked.
- This handoff doc is the only intended new file from this planning turn.

Before implementation, re-check:

```bash
git status --short
git branch --show-current
git remote -v
```

## Why This Matters

Two concrete failures are now proven:

1. Compression is not fully automatic. Sessions can remain open/uncompressed unless a client explicitly calls `session_end`, `compress_session`, or `ironmem compress <session-id>`.
2. User ADC expires and breaks unattended Vertex calls. We saw this block benchmark and cleanup flows repeatedly with:

```text
Reauthentication failed. cannot prompt during non-interactive execution.
```

The product should behave like a memory system with a sleep cycle:

- open sessions become compacted after an idle/volume threshold,
- duplicate/derived consolidation runs on a slower cadence,
- all decisions are auditable,
- raw observations and CCR originals remain preserved,
- provider failures are retried/backed off instead of marking work complete.

## Existing Code To Reuse

Important modules already exist:

- `src/compress.rs`
  - Shared compression logic and persistence.
  - `compress::run(...)` and `compress::persist(...)` are the central path to reuse.
  - Compression already writes narrative memories plus separate `kind=fact` memories.
  - It marks sessions compressed via `db::mark_compressed`.

- `src/db.rs`
  - Sessions have `started_at`, `ended_at`, and `compressed`.
  - Existing helpers include `mark_ended`, `mark_compressed`, `get_session`, `list_sessions`, `list_sessions_for_project`, `list_projects`.
  - Add focused query helpers here for sweep candidates instead of filtering everything in Rust.

- `src/auto_dream.rs`
  - Already has an opt-in, thin idle-gap watcher.
  - It runs reflection + synthesis for idle projects and writes a governance ledger entry.
  - Treat this as a foundation, not the full feature.

- `src/reflection.rs`
  - Has `run(...)` and `synthesize(...)` used by dream/reflection.

- `src/main.rs`
  - CLI already has `Compress`, `Reflect`, `Dream`, `Gc`, etc.
  - Add `Sweep` and likely `Scheduler` subcommands here.

- `src/server.rs` and `src/mcp.rs`
  - REST/MCP compression paths exist.
  - If new shared compression/sweep helpers are added, keep REST/MCP/CLI behavior aligned.

- `src/config.rs`
  - `AutoDreamConfig` already exists.
  - Extend config for auto-compress/scheduler instead of adding free-floating env-only behavior.

## Desired CLI

Build these commands:

```bash
ironmem sweep --compress-idle 30m --min-observations 50 --limit 20 --dry-run
ironmem sweep --compress-idle 30m --min-observations 50 --limit 20
ironmem sweep --dream-due --apply --limit 200
ironmem scheduler run
ironmem scheduler install-launchd
ironmem scheduler uninstall-launchd
```

Acceptable first implementation if time is tight:

```bash
ironmem sweep --compress-idle 30m --min-observations 50 --limit 20 --dry-run
ironmem sweep --compress-idle 30m --min-observations 50 --limit 20
ironmem sweep --dream-due --apply --limit 200
```

Then add `scheduler run` and launchd install as the next patch.

## Sweep Behavior

Candidate selection:

- Session is not compressed.
- Session has at least `min_observations` OR is idle past `compress_idle`.
- Idle means latest observation timestamp or session `ended_at`/`started_at` is older than threshold.
- Include sessions that never received `session_end`.
- Limit batch size.
- Stable ordering: oldest idle first.

Before compressing an open session:

- Mark it ended or treat it as logically closed for compression.
- Record a sweep decision in a ledger or dedicated sweep log.
- Never lose raw observations.
- Store the exact transcript via CCR as the current compression path already does.

Idempotency:

- Running `ironmem sweep` twice should not create duplicate memories.
- Use the `sessions.compressed` flag plus a lease/lock.
- Add a DB-level sweep lease table or optimistic update pattern so two sweepers cannot compress the same session.

Failure behavior:

- If provider/Vertex call fails, do not mark the session compressed.
- Log/report the failure.
- Add exponential/backoff metadata or at minimum avoid tight repeated retries in `scheduler run`.

Dry run:

- Must print candidate sessions, project, idle duration, observation count, and intended action.
- Must mutate nothing.

Suggested output:

```text
SWEEP dry_run=true compress_idle=30m min_observations=50 limit=20
candidate session=... project=/path idle=54m observations=73 compressed=false action=compress
summary candidates=4 compressed=0 skipped=0 failed=0
```

## Dream / Reflection Schedule

Current `auto_dream` is thin but useful. The upgraded sleep cycle should:

- keep dream/reflection lower-frequency than compression,
- default dream to proposal-first unless `--apply` is explicit,
- ledger every dream trigger with reason (`idle_gap`, `daily`, `manual`, `volume`),
- not run dream repeatedly for the same unchanged project state,
- not block compression if dream fails.

Recommended default:

- compress sweep: every 15-30 minutes,
- dream/reflection: daily or when a project crosses a meaningful new-memory threshold,
- `dream --apply` should be opt-in in config and explicit in CLI.

## Auth Design

Do not depend on user ADC for unattended automation.

Cloud mode:

- GCE/Cloud Run/Cloud Scheduler should use attached service accounts with `roles/aiplatform.user`.
- No key file required.
- Vertex provider should work through metadata-server ADC.

Local macOS mode:

- Allow `GOOGLE_APPLICATION_CREDENTIALS=/path/to/key.json` if the user explicitly chooses it.
- Key path must be outside the repo.
- Do not print key contents.
- Document rotation and least privilege.
- Prefer Keychain or a root-readable/private directory when possible.

Config shape to consider:

```json
{
  "auto_compress": {
    "enabled": true,
    "idle_minutes": 30,
    "min_observations": 50,
    "limit": 20,
    "provider_backoff_minutes": 30
  },
  "scheduler": {
    "enabled": true,
    "sweep_interval_minutes": 15,
    "dream_interval_hours": 24,
    "launchd_label": "com.execlayer.ironmem.sleep"
  }
}
```

Keep defaults conservative. Do not unexpectedly send private data to Vertex unless the user already configured a non-local provider.

## launchd Target

`ironmem scheduler install-launchd` should create a separate plist from the server plist, likely:

```text
~/Library/LaunchAgents/com.execlayer.ironmem.sleep.plist
```

It should run a sweep loop, not repeatedly spawn unbounded overlapping jobs.

Must include:

- explicit `PATH`,
- relevant provider env (`GOOGLE_APPLICATION_CREDENTIALS` only if configured),
- log paths under `~/.ironmem/logs/`,
- no secrets in logs,
- `KeepAlive` or `StartInterval`, but with locking so overlap is impossible.

Simpler first pass:

```bash
ironmem scheduler run
```

Then the plist launches that long-running process.

## DB Work

Add a focused session-candidate query in `src/db.rs`, for example:

```rust
pub struct SweepCandidate {
    pub session_id: String,
    pub project: String,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub last_observation_at: Option<i64>,
    pub observation_count: i64,
    pub compressed: bool,
}
```

Possible helpers:

```rust
list_sweep_candidates(db, idle_before_ts, min_observations, limit)
try_acquire_sweep_lease(db, session_id, lease_secs)
release_sweep_lease(db, session_id)
record_sweep_event(db, ...)
```

The lease can be a new table or a columns-backed optimistic update. Prefer simple and testable.

## Tests / Verification

Unit tests:

- candidate query returns uncompressed idle sessions,
- candidate query includes never-ended sessions,
- candidate query respects `min_observations`,
- `--dry-run` does not mark compressed,
- repeated sweep is idempotent,
- provider failure leaves session uncompressed,
- lease prevents duplicate compression.

Integration smoke:

```bash
DATABASE_URL=sqlite:///tmp/ironmem-sweep-smoke.db cargo test sweep --bin ironmem
cargo test --bin ironmem auto_dream
cargo test --bin ironmem compress
cargo clippy --bin ironmem --features local-onnx -- -D warnings
```

Use isolated temp SQLite DBs for behavioral smoke tests. Do not use the live `~/.ironmem/mem.db` for tests.

Live local smoke after build:

```bash
cargo build --release --features local-onnx
DATABASE_URL=sqlite:///tmp/ironmem-sweep-live.db ./target/release/ironmem sweep --compress-idle 1m --min-observations 1 --dry-run
```

If deploying locally:

```bash
cargo build --release --features local-onnx
cp ~/.ironmem/bin/ironmem ~/.ironmem/bin/ironmem.bak-pre-sleep-cycle-$(date +%Y%m%d-%H%M%S)
cp target/release/ironmem ~/.ironmem/bin/ironmem
launchctl kickstart -k gui/$(id -u)/com.execlayer.ironmem
curl -fsS http://127.0.0.1:37778/status
```

Do not run expensive LoCoMo or cross-encoder tests for this feature unless specifically asked.

## Product Acceptance Criteria

Done means:

- A session can be auto-compressed after idle time without manual `session_end`.
- The same session cannot be compressed twice by concurrent or repeated sweeps.
- Sweep exposes a safe dry-run.
- Provider/auth failures do not mark sessions compressed.
- Dream/reflection can run on a slower schedule and is ledgered.
- launchd installation is available or at least the CLI shape is ready for it.
- Documentation explains cloud attached service account vs local service account key vs user ADC.
- All new behavior is covered by focused tests.
- Work is committed and pushed when implementation is complete.

## What Not To Do

- Do not store service account keys in the repo.
- Do not print tokens or key material.
- Do not rely on `gcloud auth application-default login` for unattended automation.
- Do not implement only a shell script and call the feature done.
- Do not make dream `--apply` silently default-on.
- Do not delete raw observations after compression.
- Do not mutate unrelated files like `phase1_provider_DRAFT.patch`.
- Do not touch the LoCoMo benchmark repo unless the next session explicitly asks for benchmark work.

## Suggested Implementation Order

1. Add `AutoCompressConfig`/`SchedulerConfig` defaults in `src/config.rs`.
2. Add `sweep.rs` module with candidate selection, dry-run result structs, and compression runner.
3. Add DB helpers and lease/event recording in `src/db.rs`.
4. Wire CLI `ironmem sweep`.
5. Add tests for candidate selection, dry-run, idempotency, and failure behavior.
6. Extend/reuse `auto_dream.rs` or add `scheduler.rs` for `ironmem scheduler run`.
7. Add launchd install/uninstall commands.
8. Document auth and scheduler setup in README or `docs/`.
9. Build, test, commit, push.

## Last Known Benchmark Context

Separate but important context: the LoCoMo record run is complete and pushed in the benchmark repo:

- `ironmem-locomo-benchmark` commit: `deaf974`
- full run: `results/full_gpu_p100_k25_v2agg_normhints9_20260701T233607Z.json`
- score: 72.4% overall, `error_count=0`
- GCP VM was stopped/deleted afterward.

Do not reopen that benchmark loop unless asked. This handoff is for IronMem productizing unattended memory compression/sleep.
