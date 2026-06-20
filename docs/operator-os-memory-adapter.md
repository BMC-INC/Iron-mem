# Operator OS Memory Adapter

This contract defines how Operator OS should consume IronMem as its memory
fabric. IronMem remains local-first and project-scoped by default; Operator OS
adds product-level semantics such as tenants, workers, work items, handoffs,
heartbeats, and audit receipts.

## Adapter Surface

Operator OS should call IronMem through these stable surfaces:

| Capability | MCP tool / CLI / REST | Purpose |
| --- | --- | --- |
| Store memory | `remember` / `ironmem remember` | Durable curated memory, including procedural rules. |
| Search memory | `search_memories`, `search_global` / `ironmem search` | Hybrid FTS + vector + temporal graph retrieval. |
| Query graph | `memory_graph` / `ironmem graph` / `GET /graph` | Entity relationship lookup, with optional history and valid-time filters. |
| Reconcile graph | `reconcile_memory_graph` / `ironmem reconcile` | Operational maintenance for duplicate and superseded graph edges. |
| Retrieve original | `retrieve_original` | Audit-grade recovery of verbatim source behind compressed memory. |
| User profile | `get_profile`, `refresh_profile` | Cross-project durable user profile. |

## Tenant And Scope Model

Tenant isolation is mandatory before shared/team memory is exposed.

| Operator OS concept | IronMem mapping |
| --- | --- |
| Tenant | Project scope boundary. Use one IronMem project identifier per tenant/workspace. |
| User-wide preferences | `scope=user`, only for facts that intentionally cross tenants. |
| Tenant rule | `scope=project`, `kind=procedural`. |
| Agent worker | Graph source entity, for example `Agent:researcher`. |
| Work item | Graph target entity, for example `WorkItem:launch-readiness`. |
| Handoff or heartbeat | Relation edge plus provenance memory. |
| Audit receipt | Original transcript/blob reachable through `retrieve_original`. |

Project identifiers should be opaque, stable, and tenant-qualified:

```text
operator-os:tenant:<tenant_id>
```

Do not use display names as tenant boundaries. Display names can change; tenant
IDs should not.

## Event Mapping

When Operator OS records a worker event, it should store a provenance memory and
let IronMem extract graph edges during compression. For explicit product events,
Operator OS can also store a procedural or event memory directly.

### Worker Handoff

```json
{
  "project": "operator-os:tenant:acme",
  "scope": "project",
  "kind": "session",
  "text": "Agent:researcher handed off WorkItem:launch-readiness to Agent:builder because the research phase is complete. Next action: implement reconciliation tooling.",
  "tags": "operator-os handoff worker work-item"
}
```

Expected graph relation:

```text
Agent:researcher | handed_off | WorkItem:launch-readiness
WorkItem:launch-readiness | assigned_to | Agent:builder
```

### Worker Heartbeat

```json
{
  "project": "operator-os:tenant:acme",
  "scope": "project",
  "kind": "session",
  "text": "Agent:builder heartbeat for WorkItem:launch-readiness: status=in_progress, blocker=none, observed_at=2026-06-20.",
  "tags": "operator-os heartbeat worker work-item"
}
```

Expected graph relation:

```text
Agent:builder | status | in_progress
Agent:builder | assigned_to | WorkItem:launch-readiness
```

### Tenant Operating Rule

```json
{
  "project": "operator-os:tenant:acme",
  "scope": "project",
  "kind": "procedural",
  "text": "For tenant acme, do not expose shared team memory until tenant isolation checks pass.",
  "tags": "operator-os tenant-isolation procedural"
}
```

## Retrieval Pattern

For an Operator OS worker turn:

1. Query `get_profile` for user-level continuity.
2. Query `search_memories` with the active work item, worker name, and user task.
3. Query `memory_graph` for the work item and assigned worker.
4. Query procedural memories with `search_memories` using the tenant/project ID
   and terms like `operating rule`, `tenant isolation`, or the work item name.
5. Retrieve original provenance with `retrieve_original` only when audit detail
   is needed.

## Maintenance Pattern

Run graph reconciliation after bulk imports, graph backfills, or migration jobs:

```bash
ironmem reconcile --project operator-os:tenant:acme --dry-run
ironmem reconcile --project operator-os:tenant:acme
```

Use `--all` only for local administrative maintenance where cross-tenant scans
are acceptable. Product workflows should prefer explicit tenant project IDs.
