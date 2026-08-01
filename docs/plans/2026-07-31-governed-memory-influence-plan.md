# Governed Memory Influence Implementation Plan

**Date:** 2026-07-31  
**Status:** Proposed  
**Target:** IronMem after v0.4.0  
**Primary goal:** Extend IronMem from governed memory storage and retrieval into governed memory influence, without weakening retrieval quality or coupling IronMem to ExecLayer.

---

## 1. Executive decision

IronMem already governs memory records through namespaces, provenance, trust tiers, consent, retention, legal hold, tombstones, lineage, and a hash-chained ledger. It also records which memories enter agent context through `injection_events`.

The missing layer sits between ranked retrieval and context injection:

> A memory may exist, rank highly, and remain valid for reasoning while still lacking authority to influence a specific task or action.

This plan adds a narrow, backward-compatible influence-control layer. It does not replace the retrieval stack. It does not turn IronMem into an execution policy engine. It emits an auditable memory influence decision and evidence package that an agent or external control plane may consume.

The implementation order is deliberate:

1. Evidence-root lineage and derivation depth.
2. First-class influence state and policy.
3. Purpose-bound recall requests.
4. Pre-injection influence evaluation.
5. Contradiction sets.
6. Counterfactual influence evaluation.

Each phase lands behind configuration or permissive defaults. Existing callers retain current behavior until they opt into enforcement.

---

## 2. Product boundary

### IronMem owns

- memory formation
- durable storage
- provenance and evidence lineage
- retrieval and ranking
- contradiction representation
- influence eligibility
- context-injection decisions
- memory-to-context audit trails

### IronMem does not own

- external action authorization
- deployment approval
- financial approval
- credential rotation approval
- regulated workflow approval
- general-purpose policy orchestration

IronMem returns an influence decision. ExecLayer or another runtime authority may enforce the resulting action boundary.

---

## 3. Design principles

1. **Storage is not influence.** A retained memory may be blocked from retrieval or action use without deletion.
2. **Coherence is not evidence.** Multiple derived records from one source count as one evidentiary root.
3. **Reasoning authority differs from action authority.** A memory may inform an answer while remaining barred from authorizing an external action.
4. **Contradictions remain visible.** IronMem must not delete or silently overwrite competing claims to create artificial certainty.
5. **Default behavior remains compatible.** Existing callers without a recall-purpose envelope receive current retrieval behavior.
6. **Governance must not become a benchmark tax.** Influence evaluation runs after relevance ranking and must not alter base relevance scores unless explicitly configured.
7. **Every denial must be explainable.** Decisions return machine-readable reason codes and evidence.
8. **No quantum or consciousness claims.** The architecture uses conventional state, graph, retrieval, and policy mechanisms.

---

## 4. Current-state assessment

Existing primitives to reuse:

- `MemoryGovernance` in `src/governance.rs`
- `memory_meta` governance and maturity fields
- `memory_ledger`
- `injection_events`
- `parent_memory_id`
- `memory_edges`
- derived-memory quarantine
- temporal supersession and reconciliation
- retrieval routes and ranked fusion
- CCR exact-source expansion
- `lineage` and compliance reporting
- deterministic eval gate

Current gaps:

- no independent evidence-root identity
- no explicit derivation depth
- no first-class influence state
- no task-purpose envelope on recall
- no pre-injection influence evaluator
- no unresolved contradiction object
- no reason-coded influence decision in REST or MCP responses
- no counterfactual benchmark for memory-caused behavior changes

---

## 5. Target architecture

```text
memory write
  -> provenance + evidence root
  -> storage governance
  -> retrieval candidates
  -> relevance ranking
  -> contradiction annotation
  -> influence policy evaluation
  -> allowed / reasoning-only / source-required / confirmation-required / denied
  -> context injection
  -> injection ledger + lineage
  -> optional external execution authority
```

The influence evaluator must run after retrieval ranking and before context injection. This preserves retrieval quality measurement while controlling downstream use.

---

## 6. Phase 1: Evidence-root lineage

### Objective

Prevent confidence amplification when one source produces several downstream facts, inferences, graph edges, summaries, or profile statements.

### Schema changes

Add to `memory_meta`:

```sql
ALTER TABLE memory_meta ADD COLUMN evidence_root_id TEXT;
ALTER TABLE memory_meta ADD COLUMN derivation_depth INTEGER NOT NULL DEFAULT 0;
```

Recommended index:

```sql
CREATE INDEX IF NOT EXISTS idx_memory_meta_evidence_root
ON memory_meta(namespace, evidence_root_id);
```

### Semantics

- Direct user input, external evidence, tool output, and session archives create a new `evidence_root_id`.
- Derived memories inherit the parent memory's `evidence_root_id`.
- `derivation_depth = parent.derivation_depth + 1`.
- Multi-source synthesis stores a primary root and separate supporting-root edges. Do not concatenate roots into one opaque string.

### Supporting-root table

```sql
CREATE TABLE IF NOT EXISTS memory_evidence_roots (
    memory_id INTEGER NOT NULL,
    evidence_root_id TEXT NOT NULL,
    role TEXT NOT NULL DEFAULT 'supporting',
    created_at INTEGER NOT NULL,
    PRIMARY KEY(memory_id, evidence_root_id)
);
```

Roles:

- `primary`
- `supporting`
- `contradicting`

### Root generation

Use a deterministic root identifier when a stable source exists:

```text
sha256(namespace | source_type | source_ref | canonical_source_hash)
```

Use a generated UUID when the source lacks a stable reference. Store it durably at first write.

### Code changes

- Extend `MemoryGovernance` or introduce `MemoryEvidence` in `src/governance.rs`.
- Update all write paths in `src/db.rs`.
- Update compression, reflection, observer, profile, correction-miner, and sync write paths.
- Update lineage output to display root IDs and derivation depth.
- Update compliance inventory with independent-root counts.

### Ranking behavior

Do not change default ranking in Phase 1.

Add optional retrieval diagnostics:

- raw supporting memory count
- independent evidence-root count
- maximum derivation depth
- direct versus derived support ratio

### Migration

Backfill rules:

1. Memories with no parent receive a root derived from namespace, source type, source ref, record hash, and memory ID.
2. Memories with a parent inherit recursively.
3. Broken parent references receive a new root and an audit warning.
4. Cycles fail migration and produce a repair report.

### Tests

- direct writes receive unique roots
- derived memories inherit roots
- depth increments correctly
- multi-source synthesis records multiple roots
- root backfill is idempotent
- root cycles are detected
- lineage reports independent roots
- namespace isolation applies to root queries

### Exit criteria

- all memories have a root
- derivation depth is populated
- no retrieval regression
- migration passes SQLite and Postgres tests

---

## 7. Phase 2: Influence state and policy

### Objective

Represent whether a memory is eligible to influence reasoning, context, or action.

### New types

Create `src/influence.rs`:

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InfluenceState {
    Eligible,
    Quarantined,
    ReasoningOnly,
    ActionRestricted,
    Blocked,
    Superseded,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ActionRisk {
    None,
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryInfluencePolicy {
    pub state: InfluenceState,
    pub allowed_task_types: Vec<String>,
    pub denied_task_types: Vec<String>,
    pub maximum_action_risk: ActionRisk,
    pub requires_original_source: bool,
    pub requires_human_confirmation: bool,
    pub maximum_derivation_depth: Option<u32>,
}
```

### Storage

Prefer a separate table over expanding `memory_meta` with many nullable columns:

```sql
CREATE TABLE IF NOT EXISTS memory_influence_policy (
    memory_id INTEGER PRIMARY KEY,
    state TEXT NOT NULL DEFAULT 'eligible',
    allowed_task_types TEXT,
    denied_task_types TEXT,
    maximum_action_risk TEXT NOT NULL DEFAULT 'critical',
    requires_original_source BOOLEAN NOT NULL DEFAULT FALSE,
    requires_human_confirmation BOOLEAN NOT NULL DEFAULT FALSE,
    maximum_derivation_depth INTEGER,
    updated_by TEXT,
    updated_at INTEGER NOT NULL,
    FOREIGN KEY(memory_id) REFERENCES memories(id)
);
```

Store task lists as canonical JSON arrays for SQLite/Postgres parity.

### Default policy

Existing memories and callers receive:

```text
state = eligible
allowed_task_types = []
denied_task_types = []
maximum_action_risk = critical
requires_original_source = false
requires_human_confirmation = false
maximum_derivation_depth = none
```

This reproduces current behavior.

Derived memories default to `quarantined` only where current derived-memory rules already quarantine them. Do not expand quarantine silently in the first release.

### Policy mutation

Add governed policy updates through:

- CLI: `ironmem influence set <memory-id> ...`
- REST: `PUT /memory/{id}/influence`
- MCP: `set_memory_influence`

Each change must append a ledger entry containing old policy, new policy, actor, and reason.

### Tests

- default policy preserves current behavior
- blocked memories never inject
- reasoning-only memories remain available to reasoning contexts
- action-restricted memories fail above their risk threshold
- policy mutation writes ledger entries
- policy updates respect namespace access
- legal hold does not prevent policy restriction

### Exit criteria

- policy CRUD works through shared handlers
- ledger captures every policy transition
- permissive defaults show zero retrieval-score change

---

## 8. Phase 3: Purpose-bound recall

### Objective

Let callers state why memory is being requested and what downstream consequence is expected.

### New request type

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecallPurpose {
    pub agent_id: Option<String>,
    pub namespace: String,
    pub project: String,
    pub task_type: String,
    pub intended_action: Option<String>,
    pub action_risk: ActionRisk,
    pub human_confirmed: bool,
    pub require_source_backing: bool,
}
```

### Backward compatibility

- Missing `RecallPurpose` means legacy permissive behavior.
- Config flag `influence.require_purpose` defaults to `false`.
- When enabled, missing purpose returns a reason-coded error rather than silently guessing.

### API changes

REST:

- Extend `GET /context` with optional purpose query fields for simple clients.
- Add `POST /context/evaluate` with a structured JSON body for full fidelity.

MCP:

- Extend `get_context` and `memory_skim` with optional purpose fields.
- Add structured influence metadata to each result.

CLI:

```bash
ironmem search "deployment key" \
  --task-type deploy \
  --intended-action rotate_credentials \
  --action-risk critical
```

### Validation

- normalize task types to lowercase snake case
- reject control characters and unbounded strings
- cap allowed/denied task-list lengths
- resolve agent identity from authenticated key when present
- do not trust caller-supplied `agent_id` when bearer-key identity exists

### Tests

- legacy requests remain unchanged
- authenticated identity overrides spoofed request identity
- purpose-required mode rejects absent purpose
- task normalization is deterministic
- REST, MCP, and CLI use the same evaluator

### Exit criteria

- all retrieval surfaces accept a purpose envelope
- existing integrations continue to work without configuration changes

---

## 9. Phase 4: Pre-injection influence evaluator

### Objective

Make one deterministic decision for each ranked memory before it enters context.

### Decision type

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum InfluenceDecisionKind {
    Allow,
    AllowReasoningOnly,
    RequireOriginalSource,
    RequireHumanConfirmation,
    Deny,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InfluenceDecision {
    pub memory_id: i64,
    pub decision: InfluenceDecisionKind,
    pub reason_codes: Vec<String>,
    pub policy: MemoryInfluencePolicy,
    pub purpose: RecallPurpose,
    pub evidence_root_count: usize,
    pub derivation_depth: u32,
    pub contradiction_set_ids: Vec<String>,
}
```

### Reason codes

Use stable constants:

- `state_blocked`
- `state_quarantined`
- `state_superseded`
- `task_not_allowed`
- `task_explicitly_denied`
- `action_risk_exceeded`
- `source_expansion_required`
- `human_confirmation_required`
- `derivation_depth_exceeded`
- `consent_not_granted`
- `expired`
- `tombstoned`
- `unresolved_contradiction`

### Evaluation order

1. storage-governance eligibility
2. influence state
3. task allow/deny rules
4. action-risk threshold
5. derivation-depth limit
6. source requirement
7. confirmation requirement
8. contradiction annotation

Deny rules override allow rules.

### Context assembly behavior

- `Allow`: inject normally.
- `AllowReasoningOnly`: inject with a machine-readable header stating it carries no action authority.
- `RequireOriginalSource`: expand CCR or source-backed chunk before injection. Deny if exact evidence is unavailable.
- `RequireHumanConfirmation`: exclude unless `human_confirmed=true`.
- `Deny`: exclude and report the reason in trace metadata.

### Injection audit

Extend `injection_events` or create `memory_influence_events`:

```sql
CREATE TABLE IF NOT EXISTS memory_influence_events (
    id INTEGER PRIMARY KEY,
    memory_id INTEGER NOT NULL,
    project TEXT NOT NULL,
    namespace TEXT NOT NULL,
    session_id TEXT,
    agent_id TEXT,
    task_type TEXT NOT NULL,
    intended_action TEXT,
    action_risk TEXT NOT NULL,
    decision TEXT NOT NULL,
    reason_codes TEXT NOT NULL,
    query TEXT,
    rank INTEGER,
    created_at INTEGER NOT NULL
);
```

Record both allowed and denied decisions. This creates a full memory-to-context influence trail.

### Performance target

- p50 evaluator overhead under 1 ms per candidate for local SQLite
- p99 evaluator overhead under 5 ms per candidate
- batch-load policies and contradiction annotations to avoid N+1 queries

### Tests

- table-driven evaluator tests for every decision path
- deny precedence
- exact-source expansion success and failure
- confirmation behavior
- batch evaluation ordering
- influence events record allowed and denied cases
- latency benchmark or bounded microbenchmark

### Exit criteria

- all context injection flows call the evaluator
- lineage shows both allowed and denied influence attempts
- no N+1 policy reads

---

## 10. Phase 5: Contradiction sets

### Objective

Represent unresolved competing claims without deleting evidence or manufacturing certainty.

### Schema

```sql
CREATE TABLE IF NOT EXISTS contradiction_sets (
    id TEXT PRIMARY KEY,
    namespace TEXT NOT NULL,
    project TEXT NOT NULL,
    claim_key TEXT NOT NULL,
    preferred_memory_id INTEGER,
    status TEXT NOT NULL DEFAULT 'unresolved',
    resolution_basis TEXT,
    resolved_by TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS contradiction_members (
    contradiction_set_id TEXT NOT NULL,
    memory_id INTEGER NOT NULL,
    stance TEXT NOT NULL DEFAULT 'competing',
    created_at INTEGER NOT NULL,
    PRIMARY KEY(contradiction_set_id, memory_id)
);
```

Statuses:

- `unresolved`
- `preferred`
- `resolved`
- `obsolete`

Stances:

- `supports`
- `contradicts`
- `competing`

### Detection

Version 1 uses deterministic creation only:

- explicit user or agent command
- temporal conflict handler
- graph reconciliation when two active edges share a claim key and incompatible targets

Do not ship broad LLM contradiction mining in the first version.

### Recall behavior

- unresolved sets do not automatically deny reasoning
- high-risk action requests may require confirmation or source expansion
- return competing memories together when budget permits
- mark the preferred memory without suppressing the others from audit views

### CLI and API

```bash
ironmem contradiction create --claim-key "project.auth.mode" --members 42,88
ironmem contradiction prefer <set-id> --memory 88 --basis "runtime config and passing tests"
ironmem contradiction resolve <set-id> --memory 88 --basis "user confirmed"
```

REST and MCP mirror shared handlers.

### Tests

- create and update sets
- namespace isolation
- incompatible graph targets form a set in deterministic reconciliation
- unresolved sets annotate influence decisions
- preferred status does not delete competing memories
- resolution writes ledger entries

### Exit criteria

- contradictory claims remain inspectable
- high-risk influence decisions expose contradiction state
- graph reconciliation does not silently overwrite active evidence

---

## 11. Phase 6: Counterfactual influence evaluation

### Objective

Measure whether memory governance changes behavior correctly, not merely whether retrieval returns the expected fact.

### Eval matrix

For each scenario run:

1. memory absent
2. memory present and eligible
3. memory present but quarantined
4. memory present from an untrusted source
5. memory contradicted
6. memory superseded
7. memory revoked after prior injection
8. memory reasoning-only during an action request
9. memory requiring exact-source expansion
10. memory exceeding derivation-depth limit

### Metrics

- retrieval hit rate
- injection eligibility accuracy
- unauthorized influence rate
- required-source compliance
- confirmation enforcement
- stale influence rate
- contradiction disclosure rate
- independent-root counting accuracy
- refusal correctness
- governance latency

### Deterministic suite

Add a new cluster to `src/eval.rs`:

- `influence_allow`
- `influence_deny_task`
- `influence_deny_risk`
- `influence_reasoning_only`
- `influence_source_required`
- `influence_confirmation_required`
- `influence_derivation_depth`
- `influence_contradiction_annotation`
- `evidence_root_deduplication`
- `revocation_no_stale_injection`

### Model-backed benchmark

Add an optional harness that compares generated answers or actions under counterfactual memory states. Keep it outside the deterministic CI gate until model variance and cost are controlled.

### Exit criteria

- deterministic influence cluster gates CI
- governance-on relevance remains within 0.5 points of governance-off when all policies are permissive
- unauthorized influence is zero in deterministic cases

---

## 12. Configuration

Add an optional section:

```json
{
  "influence": {
    "enabled": false,
    "require_purpose": false,
    "record_denials": true,
    "fail_closed_on_policy_error": true,
    "high_risk_requires_source": false,
    "critical_risk_requires_confirmation": false,
    "contradiction_high_risk_mode": "annotate"
  }
}
```

Allowed `contradiction_high_risk_mode` values:

- `annotate`
- `require_source`
- `require_confirmation`
- `deny`

Environment overrides:

- `IRONMEM_INFLUENCE_ENABLED`
- `IRONMEM_INFLUENCE_REQUIRE_PURPOSE`
- `IRONMEM_INFLUENCE_RECORD_DENIALS`
- `IRONMEM_INFLUENCE_FAIL_CLOSED`

Defaults preserve current behavior.

---

## 13. MCP tool plan

Do not expand the tool surface excessively.

### Extend existing tools

- `get_context`
- `memory_skim`
- `search_memories`
- `search_global`

Add optional purpose fields and return influence metadata.

### Add tools

1. `set_memory_influence`
2. `get_memory_influence`
3. `manage_contradiction`

Do not create separate tools for every contradiction operation. Use an `operation` enum.

Expected total after implementation: 24 MCP tools.

---

## 14. REST plan

New endpoints:

- `POST /context/evaluate`
- `GET /memory/{id}/influence`
- `PUT /memory/{id}/influence`
- `GET /memory/{id}/influence-events`
- `POST /contradictions`
- `PUT /contradictions/{id}`
- `GET /contradictions/{id}`

Extend:

- `GET /memory/{id}/lineage`
- `GET /compliance/report`
- `GET /status`

`/status` additions:

- influence decisions by type
- denials by reason code
- evaluator latency
- source-expansion count
- confirmation-required count
- unresolved contradiction count

---

## 15. CLI plan

```bash
ironmem influence get <memory-id>
ironmem influence set <memory-id> --state reasoning_only
ironmem influence set <memory-id> --deny-task deploy --max-risk medium
ironmem influence set <memory-id> --require-source --require-confirmation
ironmem influence events <memory-id>

ironmem contradiction create --claim-key <key> --members <ids>
ironmem contradiction show <set-id>
ironmem contradiction prefer <set-id> --memory <id> --basis <text>
ironmem contradiction resolve <set-id> --memory <id> --basis <text>
```

Every mutation requires `--reason` unless performed interactively through the local UI.

---

## 16. Workbench plan

Add to the evidence inspector:

- evidence-root ID
- independent-root count
- derivation depth
- influence state
- task allow/deny lists
- action-risk limit
- source and confirmation requirements
- contradiction memberships
- recent allowed and denied influence events

Add an influence simulation panel:

- task type
- intended action
- action risk
- human confirmed
- require source

The panel evaluates without mutating state and displays reason codes.

---

## 17. Compliance and lineage

Extend lineage to answer:

- which independent source roots supported this memory
- how many derivation steps separate it from the source
- which contradiction sets include it
- every influence decision involving it
- whether it entered context as reasoning-only
- whether source expansion occurred
- whether human confirmation was present

Extend the compliance report with:

- policy-state inventory
- denied influence count by reason
- high-risk requests involving unresolved contradictions
- memories exceeding configured derivation depth
- independent-root versus derived-record counts

Do not claim EU AI Act compliance from this feature alone. Retain obligation mapping language.

---

## 18. Migration strategy

### Migration A

- add evidence-root columns and table
- backfill roots and depth
- verify no cycles

### Migration B

- create influence-policy and influence-event tables
- insert no policy rows unless a memory deviates from permissive defaults

### Migration C

- create contradiction tables

All migrations must:

- support SQLite and Postgres
- remain restart-safe
- record migration version
- avoid rewriting the memory ledger
- produce a deterministic repair report for malformed lineage

Rollback means disabling enforcement, not deleting new metadata.

---

## 19. Security review

Threats to test:

- caller spoofs `agent_id`
- caller omits purpose to bypass policy
- malformed task names bypass deny rules
- derived memories reset their depth
- multi-source synthesis hides weak roots
- policy update occurs outside namespace allowlist
- policy errors fail open
- contradiction preference hides competing evidence
- source-required decisions inject compressed summaries instead of originals
- denied memory still enters `IRONMEM.md`
- stale cached policies survive mutation

Security decisions:

- authenticated identity overrides supplied identity
- enforcement mode fails closed on evaluator errors
- policy cache invalidates on ledgered mutation
- all injection surfaces use one shared evaluation path
- no client may mark human confirmation without an authenticated actor when strict mode is enabled

---

## 20. Performance plan

- batch-load policy rows for candidate memory IDs
- batch-load contradiction memberships
- calculate evidence-root counts in one grouped query
- preserve relevance ranking before influence evaluation
- expose evaluator timing separately from retrieval timing
- cap reason-code and contradiction metadata returned to agents

Performance gates:

- permissive influence evaluation adds less than 5% to p50 context latency
- no additional LLM call in the evaluator
- no per-candidate database query
- deterministic evaluator remains allocation-bounded for standard candidate pools

---

## 21. Delivery sequence

### PR 1: Evidence roots

- schema and migration
- write-path propagation
- lineage output
- deterministic tests

### PR 2: Influence model

- types and policy storage
- CLI/REST/MCP policy CRUD
- ledger integration
- tests

### PR 3: Purpose-bound evaluation

- recall-purpose envelope
- evaluator
- context integration
- influence events
- metrics

### PR 4: Contradiction sets

- schema
- deterministic conflict creation
- management APIs
- retrieval annotation

### PR 5: Evaluation and workbench

- counterfactual eval cluster
- UI simulation
- compliance and lineage extensions
- benchmark report

Do not implement the full plan as one oversized PR. The phases touch core retrieval, storage, MCP, REST, CLI, and UI surfaces. Smaller PRs reduce regression risk and make benchmark attribution possible.

---

## 22. Definition of done

The project is complete when:

- every memory has an evidence root and derivation depth
- derived records do not inflate independent support counts
- callers may supply a structured recall purpose
- influence policy evaluates before every context injection path
- denied memories never enter context
- reasoning-only memories are labeled and audited
- source-required memories expand exact evidence before injection
- contradictions remain visible and auditable
- lineage includes allowed and denied influence attempts
- deterministic counterfactual cases gate CI
- permissive mode preserves current retrieval scores within 0.5 points
- enforcement remains disabled by default for existing users

---

## 23. Non-goals

- claims of artificial consciousness
- quantum-computing implementation
- replacement of ExecLayer
- generalized action-policy orchestration
- automatic trust inference from model confidence
- LLM-based contradiction mining in the first release
- signing every vector lookup or low-level retrieval operation
- changing the public adoption wedge away from coding-assistant memory

---

## 24. Recommended release language

Keep the top-level product message simple:

> Persistent memory for AI coding assistants.

Describe the advanced capability lower in the README:

> IronMem governs which memories may influence a task, preserves independent evidence lineage, and records every allowed or denied context-injection decision.

This preserves the narrow adoption wedge while accurately describing the technical moat.
