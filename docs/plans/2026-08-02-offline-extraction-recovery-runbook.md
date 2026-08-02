# Offline Extraction Recovery Runbook

IronMem remains offline-first. `compression.mode = "local"` performs deterministic on-device fact and procedure extraction and makes no provider call. `cloud_with_local_fallback` is an explicit opt-in enrichment mode; local extraction still runs first and remains the fallback and storage owner.

## Safety gates

Before writing recovered children to a real database:

1. Stop or otherwise quiesce the local IronMem writer.
2. Create a timestamped SQLite backup with the SQLite backup API and record its SHA-256 checksum.
3. Record baseline counts by memory kind, compressed session count, and observation count.
4. Run a long-session canary in dry-run mode. Dry-run never initializes an embedder, writes receipts, changes session state, or creates memories.
5. Confirm zero CCR integrity failures, nonzero supported yield, acceptable latency, and no synthetic benchmark projects in the candidate set.
6. Apply the same small canary to the backed-up database copy first. Re-run it to prove receipt and child-write idempotency.
7. Apply the canary to the real database, inspect retrieval, then continue in bounded batches using the returned `next_after_memory_id` cursor.

Example canary:

```text
ironmem extraction-backfill --dry-run --after-memory-id <checkpoint> --since-timestamp <checkpoint-time> --min-observations 50 --limit 5
```

Apply only after review:

```text
ironmem extraction-backfill --after-memory-id <checkpoint> --since-timestamp <checkpoint-time> --min-observations 50 --limit 5
ironmem extraction-status
```

Each completed extraction is keyed by source memory, verified transcript hash, and extractor version. Re-running a completed batch skips it. An interrupted child write adopts an identical same-session child before continuing, so retry does not duplicate it. Source session archives are never deleted or rewritten.

Recovery only embeds children when the configured embedding provider is explicitly local (`ollama` or `onnx`). `auto`, `openai`, and `google` are disabled for this command so archived content cannot leave the device accidentally. A later local `ironmem embed` run can fill any missing vectors.

## Health interpretation

`ironmem extraction-status` and `/status` report the active extractor version, completed and failed receipts, total facts and procedures, last success, current zero-yield streak, and remaining archived/uncompressed candidates. A nonempty session with no durable signals emits a reason-coded warning (`no_durable_signals`); an empty session is reported as `content_free`; CCR corruption records a failed receipt and stops that candidate.

Keep LoCoMo and other synthetic benchmark sessions outside the genuine-work recovery selection. Use a project restriction when project provenance is uncertain.
The recovery query also deny-filters project paths containing `locomo`, `longmemeval`, or `benchmark` so synthetic evaluation data cannot enter the genuine-work backfill accidentally.
