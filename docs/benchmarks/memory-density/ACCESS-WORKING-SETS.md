# Delivery telemetry and working sets

This batch implements phases E and F of the upgrade plan. The production injection ceiling remains 24,000 decimal bytes. Dynamic selection is disabled by default; no scored accuracy improvement is claimed. The full regression/evaluation run is deferred until the remaining upgrade phases are complete, as requested.

## Delivery contract

- Recall means authorized memory content returned by CLI search/global search/list, MCP context/list/search/skim, and REST context/evaluate/skim. Candidate retrieval, ranking, evidence lookups, diagnostics, and internal source expansion do not count.
- Expansion means a successful original-source response from MCP/REST, or an original included in REST context evaluation. A chunk-summary fallback without an original hash does not count as expansion. Only authorized owners of the reference actually resolved receive credit; unused request selectors do not.
- Delivery is the handler response boundary. HTTP/MCP transport acknowledgement is unavailable, so a later disconnect cannot be distinguished from receipt by the client. CLI accounting follows printing.
- Injection continues to use `injection_events` as its only truth source. Counts and timestamps are derived from existing history, including pre-upgrade events. Only successfully rendered IDs earn events. Injection reports distinguish `selection_omitted` (authorized candidates excluded by working-set selection) from `omitted` (entries excluded during rendering); CLI omission totals include both.
- Recall/expansion use one aggregate row per memory, with `observed_since`. Older history is unknown. A retry carrying a stable operation identity can be deduplicated for seven days using SHA-256 operation hashes; no query text is retained. REST context evaluation uses its namespace-scoped purpose request ID. Other deliveries count each handler invocation. Receipts expire on subsequent recording; idle installations do not run a cleanup daemon.
- Mutation counts are committed **row changes** in `memory_meta`, influence policies, evidence roots, entities, edges, chunks, code anchors, and contradiction membership. Contradiction-set updates count once per affected member. Rollbacks do not count. Metadata creation counts once; no-op UPDATE statements also count. These are operational write counts, not a count of semantic assertions, ledger entries, or arbitrary direct SQL edits to FTS content. Application memory text is immutable; replacement creates a new identity.
- `logical_source_bytes` is the attached transcript's original length. It is not unique physical disk use. The existing CCR density/stats surfaces measure physical storage.

`ironmem access-stats MEMORY_ID --namespace local` exposes per-memory counters, observation windows, and temperature through the local influence gate. It does not itself create recall events. REST/MCP status includes process-local telemetry call/failure/mean-duration diagnostics. Telemetry failures warn without withholding already prepared content.

Project snapshots remain semantic recovery artifacts: operational access counters, retry receipts, and temperature inputs are deliberately excluded. Restoring a project creates a new observation window for restored memories and invalidates generated context; it does not pretend historical counters survived. Full database backups retain operational telemetry. This choice avoids turning every read into another incremental snapshot payload. Legacy snapshot formats are unchanged.

## Optional selection

Settings example (merge this section into an existing settings file):

```json
{
  "working_set": {
    "enabled": true,
    "profile": "debug",
    "budget_bytes": 24000
  }
}
```

Supported profiles are `generic`, `coding`, `planning`, and `debug`. Budgets must be 2,000–24,000 bytes inclusive. Generic is the stable fallback. These are operator choices; measurements have not established a smaller universal default.

The selector receives an already ranked and authorized pool, capped at 200 candidates (four times the requested limit, minimum 16). It preserves the first relevant candidate, reserves fitting entries for unresolved contradictions and useful kinds (constraints, decisions, procedures, error solutions), then fills remaining capacity. Reservations reuse existing kinds: `project_config` for constraints, `architecture` for decisions, `procedural` for procedures, and `error_solution` for corrections; no prose is silently reclassified. A too-large candidate is skipped so smaller entries can still fit. Exact serialized output accounting reserves room for metadata and omission text.

Temperature uses a seven-day half-life, up to eight units of recall/expansion demand, and deterministic hysteresis. An expansion contributes two units, recall one. A peak of four units qualifies as hot and remains hot down to two; a warm peak remains warm down to half a unit. The bounded lifetime-demand heuristic decays from the most recent recall/expansion; it is not an event-by-event frequency estimator. Injection alone never makes a memory hot, avoiding a self-reinforcing injection loop. Temperature only reorders within four-position relevance bands. Explicit kind reservations can select later relevant candidates. Popularity never changes policy, trust, or authority, and cold memories remain in ordinary retrieval.

No content cache or cloud cold tier is introduced. Every injection re-reads live candidates and gates them. A database revision is captured before ranking and verified under the write lock at publication; changes require retry rather than writing an obsolete selection. Expiry and deletion are checked again before output. Registered generated files are invalidated after known metadata/policy mutations. Registration hashes protect user-edited files, which are preserved with a warning. Invalidation failures remain queued for retry. Already imported model context cannot be recalled retroactively. Changes made by external SQL clients are tracked for metadata mutations but require a later application invalidation pass; arbitrary FTS shadow-table edits are unsupported.

## Interning decision

The repeatable screening experiment is `scripts/profile_metadata_interning.py`. It creates and removes disposable SQLite databases, tests exact NULL/empty/case/value/payload recovery, alternates paired lookup order, and reports complete fixture file sizes including both indexes and symbol tables. It never opens a live memory database. Each row also carries a synthetic 1 KiB incompressible payload; this is a single-family projection, not an end-to-end production measurement.

The predeclared migration screen requires at least 10% total fixture savings, no more than 20% lookup p95 overhead, and confirmation on an approved representative workload. Short strings saved under 3% in the first run. High-cardinality tags increased storage. Deliberately long paths passed the synthetic screen, but there is no representative-workload evidence to justify migrating public query, snapshot, and governance contracts. **No interning schema migration is enabled.** This completes the evidence-gated investigation without adding duplicate shared-state tables. Revisit long paths if approved workload profiling establishes material net benefit.

Raw measurements: [metadata-interning.json](metadata-interning.json). The manifest records the script hash, base commit, SQLite/Python/platform, seed, workload, and cache condition. Latency is a local microbenchmark, not a production guarantee.

## Verification scope

Focused tests cover delivery counters, seven-day retry expiry, concurrent writes, failed/denied delivery, actual expansion ownership, injection-history reuse, mutation rollback, generated-file invalidation, stale-selection rejection, preservation of edited files, snapshot observation windows, temperature decay, profile byte budgets, and cold-memory retention. SQLite and PostgreSQL use the same telemetry/mutation contracts. Only synthetic fixtures belong in version control; personal memory files and databases are excluded.

### Recorded local checkpoint

The final focused run passed **16 tests**, with the opt-in PostgreSQL test separately passing **1 test** against PostgreSQL 16.13. No full-suite, scored benchmark, or remote CI run was started. Production-target Clippy with warnings denied and formatting checks passed.

```sh
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test --bin ironmem -- access_ working_set hooks::tests blocked_memory_never_crosses --test-threads=1
# In an isolated PostgreSQL database, supply IRONMEM_TEST_POSTGRES_URL:
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test --bin ironmem access_postgres -- --ignored
CARGO_PROFILE_DEV_DEBUG=0 cargo clippy --bin ironmem -- -D warnings
cargo fmt --check
```

[access-overhead.json](access-overhead.json) records the exact compiled source fingerprint and final sequential timing fixture: 1,560 microseconds median, 2,335 microseconds p95 for a one-memory aggregate write (100 measured samples after 20 warm-ups). This is local debug-build timing, not a production latency guarantee. The recording operation uses batched IDs and does not write for candidate reads.

Only one final set of synthetic artifacts is retained. Temporary profiling databases and the isolated PostgreSQL instance are removed after verification. No personal memories, generated context, live database contents, or research workfiles are part of this commit. The installed runtime remains unchanged.
