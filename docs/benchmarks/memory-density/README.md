# Memory density and recoverable storage

This batch follows PR #45. Production context retains a 24,000-byte ceiling. Chunked writes are opt-in. No model-based accuracy improvement or production latency claim is made by the local smoke tests.

## Context and measurements

The renderer owns every heading and memory handle. Summaries and tags are JSON strings with markup, import and fence delimiters escaped. Serialized field limits include quoting and escaping. Escaping prevents stored text from breaking the document structure; it does not guarantee that a model will ignore malicious instructions inside data. Existing egress authorization remains authoritative.

`ironmem density --out <new-directory>` runs a deterministic synthetic corpus in a temporary SQLite database. It covers duplicates, near-duplicates with insertions, binary data, large logs, Unicode and small objects. Every original is recovered and checked. Results include logical input bytes, unique object/chunk/manifest payload, dictionaries, actual database footprint, overhead, store wall time and load latency. The measured database overhead includes tables and indexes; it is not attributed to one feature without a separate controlled comparison.

Each of seven budgets (2/4/8/16/24/48/96 decimal KB) uses the real renderer. A fixed retrieval and original-expansion probe is recorded alongside injection bytes. Expansion bytes mean the serialized delivered response, including the public UTF-8 text representation; the separate CCR recovery check compares exact binary bytes. Total exposure includes all three outputs. This probe is not a model's adaptive tool-use policy. The current answer harness receives renderer context without subsequent model-requested expansion.

Unavailable accuracy, tokenizer counts, physical disk reads, process CPU and peak memory are null, never zero or substituted with response bytes. Latency combines the first read and two warm reads; OS cache is uncontrolled. These are local debug-build measurements, not production performance evidence.

Run paired question experiments with:

```sh
python3 scripts/run_memory_density.py --suite longmemeval --data DATA.json --out OUTPUT
python3 scripts/run_memory_density.py --suite locomo --data LOCOMO.json --out OUTPUT
```

Both default to an unscored canary of one question per category. Dry runs disable embedding-provider resolution and make no answer/judge calls. Scoring requires explicit `--scored --answer-model MODEL --judge-model MODEL`; `--per-category N` controls the paired sample. The script includes a full-context baseline and all seven budgets. LongMemEval and LoCoMo remain separate datasets and denominators. The LoCoMo adapter follows the existing external harness's session/date/caption parsing and categories 1–4; adversarial category 5 is excluded from its headline denominator. Its answer/judge pipeline here differs from the standalone LoCoMo benchmark, so scores cannot be compared to historical standalone scores without a paired rerun.

Run identity includes dataset hash, model/embedder identities, configuration hash, build-time Rust source fingerprint, code commit, budget, dry-run status and chunk threshold. Different identities cannot reuse answer checkpoints. Gold answers are used only for grading. The frontier output represents unscored accuracy as null and refuses to treat harness failures as successful measurements.

The predeclared overall noninferiority margin is one percentage point, using a conservative, distribution-free 95% paired Hoeffding interval. Category results are also reported and require review. An interval crossing the margin is inconclusive. A small successful smoke test does not establish noninferiority.

Optional charts use matplotlib:

```sh
python3 scripts/plot_memory_density.py --storage OUTPUT/report.json OTHER/report.json --frontier QUESTIONS/frontier.json --out FIGURES
```

## CCR format and rollout

The external address remains SHA-256 of the exact original. `fastcdc-v1` is a flat manifest envelope, independent of codec dispatch. It pins [FastCDC 4.0.1](https://docs.rs/fastcdc/4.0.1/fastcdc/v2020/index.html), the v2020 algorithm, normalization level 1, seed 0, and 16/64/256 KiB minimum/average/maximum chunks. The dependency is MIT licensed. Chunk-size parameters are part of the versioned contract; changes require a new format/version and measured comparison.

Objects record original hash and length, ordered chunk hashes/lengths, and a canonical manifest hash. Chunks use plain zstd; legacy whole-object and dictionary-zstd readers remain available. Every read checks each chunk and the final original. Objects are limited to 512 MiB in the chunked format, and readers reject unsupported versions, oversized manifests, impossible lengths and missing dependencies.

Set `IRONMEM_CCR_CHUNK_THRESHOLD_BYTES` (128 KiB through 512 MiB) to opt into chunked writes. Unset means legacy whole-object writes; it never disables reading existing chunked objects. The local comparison evaluates 128/256/512 KiB and 1 MiB thresholds. No production default is enabled from synthetic results alone.

Object publication, unique chunk insertion and ownership edges commit together. An ownership edge represents a unique object-to-chunk relationship; repeated occurrences remain ordered in the manifest. Garbage collection respects live memory, observation, skim-source and snapshot roots even when a reference counter reaches zero. Snapshot-parent dependencies also pin objects. Orphan chunks are reclaimed only after their ownership edges disappear. Dictionaries are conservatively retained because current dictionary selection can precede object publication; reclaiming them without a transactional pin protocol would race active writers.

`ironmem ccr-convert --threshold-bytes 262144` inventories legacy candidates without changes. `--apply` verifies each original and replaces that object's representation transactionally while preserving its address and reference count. Completed conversions are skipped on restart. Conversion does not automatically run against existing data. Take and test a consistent backup first. Older binaries cannot read the new format; rollback to an older binary requires restoring its compatible backup.

## Complete and incremental checkpoints

Project snapshots use version 5. Global legacy snapshots remain readable, with global restore blocked. Complete project capture includes memory content/timestamps, metadata and governance, evidence roots, policies, contradictions, entities, graph edges, skim/source links, code anchors, reflection proposals, sessions and observations. Exact source bytes are included independently of the source codec/dictionary representation. Relevant ledger records and imported audit evidence are retained as historical evidence, not replayed over the live audit log. These project subsets retain chain-boundary hashes; verification of the entire namespace ledger still requires its full ledger export.

Canonical embeddings, native ANN tables and PostgreSQL text-search vectors are rebuildable. Restore invalidates native embeddings/ANN rows and rebuilds PostgreSQL text search. External vector/graph indexes require an isolated native restore and an external rebuild before serving; destructive CLI/REST restores are blocked while external backends are configured.

Capture holds one consistent transaction. Restore validates the complete payload, schema and source closure before replacement, then commits its relational changes, source repair and restore receipt together. Stable IDs are retained; cross-project collisions fail before replacement. SQLite's durable ID high-water mark and PostgreSQL sequences prevent future memories from reusing removed handles. Existing ledger history remains intact. Generated context is invalidated after commit; a filesystem failure is explicitly reported as a completed database restore with failed context removal.

Current restrictions are preserved conservatively: tombstones, withdrawn/denied consent, lower trust, stronger compatible classification, legal holds and earlier expiry cannot be undone by an old snapshot. Incompatible namespace/provenance/residency/retention changes require separate-database recovery. Historical content does not acquire a new writer identity. Record hashes are recomputed for the restored content and effective metadata. A historical memory missing from a local snapshot's current project is quarantined. Cross-project contradiction sets require a shared-scope export and are rejected by this project-only format.

```sh
ironmem snapshot create --project /absolute/project --label before-change
ironmem snapshot create --project /absolute/project --incremental
ironmem snapshot restore SNAPSHOT --dry-run
ironmem snapshot restore SNAPSHOT
ironmem snapshot export SNAPSHOT /new/path/backup.json
ironmem snapshot import /path/backup.json --dry-run
ironmem snapshot import /path/backup.json
ironmem snapshot delete SNAPSHOT
ironmem gc
```

Exports flatten the dependency chain into an independently recoverable full checkpoint and use atomic, non-overwriting output. Restores can repair missing or damaged live source blobs using verified exported originals. Legacy v4 payloads are readable and report their omissions in a dry run; destructive v4 restore is blocked because missing provenance/source state cannot be recreated honestly.

### Incremental design adjustment

The existing mutation ledger does not cover every authoritative write transaction. This batch therefore computes typed canonical row-multiset differences between complete transactional captures. It captures inserts, updates and deletions without installing an incomplete per-write journal or relying on timestamps/sequence allocation before commit. This is a deliberate replacement for plan task 11, not a claim that the existing ledger became complete.

A delta records its parent content hash, parent state hash, chain depth and resulting state hash. Loading verifies the chain and each transition before restore. Publication updates a transactionally serialized project head, avoiding timestamp tie ordering. Unchanged snapshots reuse the same payload. A full checkpoint is chosen after eight delta links or when a delta is at least half the full serialized size. These are conservative bounds; future measurements may refine them. Snapshot deletion releases its root but leaves ancestors pinned while descendants depend on them. Repeated GC passes reclaim a chain after all roots are removed.

This reduces stored/write payload for small changes; capture still scans full state and is not a low-cost mutation-journal implementation. Full checkpoints include exact sources for recovery portability. Further storage optimization must retain that recovery contract.

## Validation boundary

CI runs the full suite on Linux/macOS/Windows, the existing deterministic quality eval, clippy, both seven-budget smoke experiments on Linux, and a dedicated PostgreSQL 16 integration job. Focused regressions cover hostile rendering, exact byte boundaries, file/telemetry failures, source corruption, interrupted publication, concurrent stores, migration restart, snapshot dependency pruning, source repair, native index rebuilds and non-reused IDs.

No production database migration, installed service restart or paid model evaluation is part of this batch. Telemetry/temperature/working-set selection, measured metadata interning and temporal assertions remain the next approved batches.
