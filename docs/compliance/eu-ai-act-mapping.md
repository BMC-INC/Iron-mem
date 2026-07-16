# IronMem ↔ EU AI Act Mapping (Articles 12 & 13)

IronMem is a memory layer with governance embedded in the write, read, and
delete paths — not bolted on. This document maps the concrete mechanisms in
this codebase to the EU AI Act's record-keeping and transparency obligations
for providers of high-risk AI systems (enforcement from **2 August 2026**).
It is an engineering traceability aid, not legal advice: whether a given
deployment is "high-risk" and which obligations attach is a determination for
your counsel.

Generate the evidence for everything below with one command:

```bash
ironmem compliance-report            # markdown + JSON under docs/compliance/reports/
ironmem ledger-migrate --namespace local --out <evidence-dir>  # export only
ironmem ledger-migrate --namespace local --out <evidence-dir> --apply
curl :37778/compliance/report        # same report as JSON
ironmem lineage <memory_id>          # per-memory trail
```

## Article 12 — Record-keeping (automatic logging)

| Obligation (paraphrased) | IronMem mechanism | Where |
|---|---|---|
| Automatic recording of events over the system's lifetime | Every governed memory operation (write, governance update, forget) appends a ledger entry with actor, operation type, payload, and timestamp — automatically, in the same code path as the operation itself | `memory_ledger` table; `db::append_memory_ledger` called from `db::apply_memory_governance` / `db::governed_delete_memory` |
| Records must support traceability of the system's functioning | Ledger entries are SHA-256 hash-chained per namespace (`prev_hash` → `entry_hash`); the compliance report re-derives every hash, so any edit, deletion, or reordering of history is detected | `governance::ledger_entry_hash`; `compliance::verify_ledger_chain` |
| Historical concurrency evidence and forward repair | `ledger-migrate` exports every original entry and an explicit fork map, commits the deterministic bundle by SHA-256, then optionally starts a new append-only epoch without rewriting prior history | `compliance::build_ledger_evidence`; `db::append_memory_ledger_migration`; `memory_ledger_epochs` |
| Logging of periods of use / situations that may result in risk | Every injection of a memory into an agent context is recorded with project, session, rank, and the triggering query — the "memory → action" half of traceability | `injection_events` table; `db::record_injection_events`; surfaced per-memory via `compliance::memory_lineage` |
| Input data traceability | Every memory carries writer identity, source type, source reference, and (for derived memories) a parent chain back to its origin; verbatim originals are content-addressed and reversible in the CCR blob store | `memory_meta` governance columns; `src/ccr/` |
| State reconstruction | Versioned brain snapshots record memory/edge counts and a payload hash at a point in time and are restorable | `brain_snapshots`; `ironmem snapshot` / `/snapshots` |

## Article 13 — Transparency and provision of information

| Obligation (paraphrased) | IronMem mechanism | Where |
|---|---|---|
| Operation sufficiently transparent for deployers to interpret output | Any retrieved/injected memory can be traced to its writer, trust tier, classification, consent state, and full audit trail; retrieval-trace + evidence inspection ship in the workbench UI | `ironmem lineage` / `GET /memory/{id}/lineage`; `/ui` workbench |
| Disclosure of capabilities, limitations, and controls | Per-namespace inventory of what is stored under which classification/consent, with retention and erasure controls in force | `db::governance_inventory`; compliance report §"Data governance inventory" |
| Human oversight support | Governed deletion (`ironmem forget`) is blocked by legal holds, tombstones instead of erasing history, and is itself ledgered with actor and reason; reflection/consolidation are dry-run-first proposals | `db::governed_delete_memory`; `src/reflection.rs` |

## Controls that fail closed (defense in depth)

- **PII/PHI consent gate:** memories classified `pii`/`phi` cannot be stored
  without `consent_state = granted` — the write errors
  (`MemoryGovernance::validate`), it does not warn.
- **Namespace isolation:** every read path filters by namespace and excludes
  tombstoned/expired rows at the SQL layer, so out-of-tenant or erased data
  cannot rank, be injected, or leak into answers.
- **Per-agent access keys:** with `agent_keys` configured, every REST request
  must present a known bearer token; the resolved agent is confined to its
  namespace allowlist and all writes are attributed `agent:<id>` in the ledger
  — callers cannot spoof writer identity.
- **Score-neutral governance:** governance metadata is recorded on every
  memory, but its influence on ranking is a tunable lever (default: none), and
  the eval suite pins governed vs ungoverned retrieval parity
  (`governance_parity_ranking`).

## Verification

The deterministic eval suite (`ironmem eval`, run in CI on every change)
includes a compliance cluster: chain verification passes on honest history,
detects tampered history (naming the first broken entry), and lineage traces
a memory from write to agent action. The report generator itself is exercised
end-to-end.
