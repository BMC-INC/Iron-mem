# Diagnostics and retention

## One governed diagnostic surface

CLI `ironmem diagnose '<JSON>'`, REST `POST /diagnostics`, and MCP
`memory_diagnose` (`{"request": {...}}`) share one handler. Local operators receive
`diagnostics:read`; remote principals require an explicit grant and a matching
namespace allowlist. Permission is checked before candidate or metadata reads.

```json
{
  "namespace": "local",
  "project": "/workspace/example",
  "query": "invoice cents",
  "budget_bytes": 2000,
  "include_content": false
}
```

Provide exactly one `query` or `memory_id`. The report includes runtime selection,
lexical candidate positions, actual shared-gate decisions/reasons, policy versions,
source-object presence, evidence-root/derivation metadata, access observations,
temperature, retention flags and registered context freshness. Context bytes and
written IDs come from the real bounded renderer. Memory content is absent by
default. Explicit content requests still withhold denied, source-required and
reasoning-only entries from the plain context string. Successfully returned IDs
alone receive recall telemetry. Policy decision receipts may be recorded for a
preview, so the command is not a strictly mutation-free database inspection.

This is a fresh lexical preview, not a replay of a past answer or an explanation
of optional hybrid/reranker scores. Only the top 100 lexical candidates are examined;
there is no invented reason for a memory absent from that pool. Mutations detected
between ranking and reporting cause a retry error. Object presence does not prove
byte-exact reconstruction: use the governed original-expansion path to verify it.
Expired/deleted IDs return unavailable in the active scope. Remote calls do not
read arbitrary filesystem paths. Local operators can compare a registered
`IRONMEM.md` hash with the file; user edits are reported and preserved.

## Retention has multiple layers

`forget` removes active eligibility and the live memory. It does not promise that
all representations disappear: session observations, shared source objects,
immutable assertions, audit receipts, retained snapshots, exported backups and
assistant transcripts may still contain information. Garbage collection protects
reachable objects, including snapshot dependencies; a shared source must not be
deleted just because one memory no longer references it.

The offline inventory describes the whole native SQLite store without printing
memory values:

```sh
python3 scripts/retention.py plan --database /absolute/path/to/mem.db
```

It reports stored-row counts by layer/table, current legal holds and a content-bound plan hash.
Fingerprinting scans the whole store in one read transaction. It is deliberately
an offline maintenance tool, not a cheap per-request API. The report lists external
copies it cannot inspect or erase. Keep reports containing local database paths
private when they describe your own installation.

For complete retirement of a native SQLite store, stop every IronMem worker,
scheduler, MCP client and other database user. Review all namespaces and retained
history in the plan. Then use `purge-store` with `--database`, `--confirm-plan`
containing the reviewed hash, and both `--offline` and `--erase-all-history`.
The command refuses changed contents, current legal holds and active SQLite locks.
It drops all application tables, including immutable chains and internal
snapshots, in one exclusive transaction, then vacuums with secure deletion enabled.
A compaction failure reports that deletion committed but compaction did not and
exits nonzero. It never claims complete erasure in that case.

This is intentionally a whole-store operation. Selective rewriting of immutable
hash chains would undermine historical integrity. Use governed forget for
selective active-memory deletion; do not advertise it as physical erasure.
The offline Python tool supports the normal SQLite/FTS store and safely refuses
unknown virtual modules such as `vec0`. Module-backed SQLite stores, PostgreSQL,
and external vector/graph services require their own operator-managed purge and
backup-retention procedure. No unsupported backend is silently skipped.

The exclusive lock prevents active readers/writers during the purge, but cannot
prevent an idle service from reconnecting later. The operator must stop services.
Exported checkpoints, OS snapshots, cloud backups, generated/user-edited context,
logs, assistant transcripts and external indexes remain outside this operation.
Vacuuming does not guarantee erasure of SSD cells, filesystem snapshots or other
copies. Reinitializing creates a new empty store; restoring an old backup can
reintroduce erased information. Preserve or destroy backups deliberately.

No personal store is purged as part of development or the included fixture tests.

Filesystem freshness inspection is limited to the local process working directory
when its absolute path exactly matches the requested project. Request paths never
select files to read; other project scopes report database registration freshness
without inspecting disk.
