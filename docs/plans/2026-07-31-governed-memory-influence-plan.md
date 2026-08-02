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

The product supports two explicit operating modes:

- **Advisory mode** accepts a self-declared purpose, preserves legacy compatibility, and returns explainable influence metadata. It must never be marketed as a hard authorization boundary.
- **Strict mode** enforces allow/deny decisions only after a trusted local or external authority attests the purpose and, where required, supplies a scoped confirmation receipt. Unattested caller claims cannot reduce risk, satisfy confirmation, or unlock restricted memory.

IronMem hard-enforces whether memory content leaves an egress surface. A downstream runtime hard-enforces whether reasoning-only material may authorize an external action. IronMem must keep those guarantees separate in APIs, documentation, and release claims.

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
9. **Caller claims are not authority.** Task type, action risk, identity, and human confirmation are advisory until verified by a trusted attestor.
10. **One egress gate.** Every surface that can disclose memory text or source material uses the same influence evaluator.
11. **Audit decisions are reproducible.** Each decision records the policy, evaluator, configuration, purpose, and confirmation versions that produced it.

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
  -> purpose verification (advisory or attested)
  -> influence policy evaluation
  -> allowed / reasoning-only / source-required / confirmation-required / denied
  -> shared egress gate
  -> context / skim / search / source expansion / file injection
  -> injection ledger + lineage
  -> optional external execution authority
```

The influence evaluator must run after retrieval ranking and before any memory text leaves IronMem. This preserves retrieval quality measurement while controlling downstream use. Ranking diagnostics may inspect candidate IDs internally, but blocked content must not appear in REST, MCP, CLI, Workbench, logs, `IRONMEM.md`, source expansion, or trace payloads.

`ReasoningOnly` is a hard IronMem inclusion decision but only an advisory action-authority label unless the downstream runtime attests that it enforces separate reasoning and authorized-evidence channels. Strict action requests through an untrusted or file-based consumer exclude reasoning-only content rather than relying on prompt text to constrain the model.

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
    memory_id BIGINT NOT NULL,
    evidence_root_id TEXT NOT NULL,
    role TEXT NOT NULL DEFAULT 'supporting',
    created_at BIGINT NOT NULL,
    PRIMARY KEY(memory_id, evidence_root_id),
    FOREIGN KEY(memory_id) REFERENCES memory_meta(memory_id) ON DELETE CASCADE
);
```

`memory_meta(memory_id)` is the cross-backend relational parent. Do not reference `memories(id)` directly: Postgres exposes `memories.id`, while SQLite stores memories in an FTS5 virtual table addressed by `rowid`. Migration A must first ensure every live memory has a `memory_meta` row, and deletion/restore paths must preserve the parent-first cleanup contract.

Roles:

- `primary`
- `supporting`
- `contradicting`

### Root generation

Use a deterministic root identifier when a stable source exists:

```text
sha256(canonical_cbor({version, namespace, source_type, source_ref, canonical_source_hash}))
```

Do not hash delimiter-concatenated strings. Use a versioned, length-delimited canonical encoding and test it with adversarial field boundaries. Use a generated UUID when the source lacks a stable reference. Store it durably at first write.

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
- root encoding cannot collide through ambiguous field boundaries
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
    pub version: u64,
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
    memory_id BIGINT PRIMARY KEY,
    version BIGINT NOT NULL DEFAULT 1,
    state TEXT NOT NULL DEFAULT 'eligible',
    allowed_task_types TEXT,
    denied_task_types TEXT,
    maximum_action_risk TEXT NOT NULL DEFAULT 'critical',
    requires_original_source BOOLEAN NOT NULL DEFAULT FALSE,
    requires_human_confirmation BOOLEAN NOT NULL DEFAULT FALSE,
    maximum_derivation_depth INTEGER,
    updated_by TEXT,
    updated_at BIGINT NOT NULL,
    FOREIGN KEY(memory_id) REFERENCES memory_meta(memory_id) ON DELETE CASCADE
);
```

Store task lists as canonical JSON arrays for SQLite/Postgres parity.

### Default policy

Existing memories and callers receive:

```text
state = eligible
version = 1
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

Policy mutation is an administrative capability, not a consequence of namespace read access. Add explicit `influence_policy:read` and `influence_policy:write` capabilities to authenticated identities. A writer may not relax its own restriction unless it also holds the policy-write capability.

REST mutations require an expected version (`If-Match` or an equivalent body field). MCP and CLI mutations require `expected_version`. Conflicting updates fail with a reason-coded version conflict instead of silently overwriting each other. The ledger entry records the old version, new version, actor identity, authority, reason, and request ID.

The evaluator uses a documented state-transition matrix. In particular:

- deny rules override allow rules
- unknown task types never satisfy a positive allowlist
- `ActionRestricted` applies the risk threshold and cannot be unlocked by a self-declared lower risk in strict mode
- `ReasoningOnly` is excluded from strict action/file injection unless the downstream consumer is attested to enforce separate channels
- `Blocked`, `Quarantined`, and `Superseded` cannot be overridden by request fields

### Tests

- default policy preserves current behavior
- blocked memories never inject
- reasoning-only memories remain available to reasoning contexts
- action-restricted memories fail above their risk threshold
- policy mutation writes ledger entries
- policy updates respect namespace access
- policy updates require policy-write capability
- stale expected versions cannot overwrite newer policy
- legal hold does not prevent policy restriction

### Exit criteria

- policy CRUD works through shared handlers
- ledger captures every policy transition
- permissive defaults show zero retrieval-score change

---

## 8. Phase 3: Purpose-bound recall

### Objective

Let callers state why memory is being requested and what downstream consequence is expected.

### Wire request and verified purpose

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecallPurpose {
    pub request_id: String,
    pub namespace: String,
    pub project: String,
    pub task_type: String,
    pub intended_action: Option<String>,
    pub action_risk: ActionRisk,
    pub require_source_backing: bool,
    pub purpose_attestation: Option<String>,
    pub confirmation_receipt: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedRecallPurpose {
    pub request_id: String,
    pub subject_agent_id: Option<String>,
    pub namespace: String,
    pub project: String,
    pub task_type: String,
    pub intended_action: Option<String>,
    pub action_risk: ActionRisk,
    pub require_source_backing: bool,
    pub authority: PurposeAuthority,
    pub attestation_id: Option<String>,
    pub confirmation_receipt_id: Option<String>,
    pub issued_at: i64,
    pub expires_at: Option<i64>,
}
```

Only the verifier may construct `VerifiedRecallPurpose`. Evaluators never consume the unverified wire type directly.

### Purpose authority

Supported authorities:

- `self_declared` — advisory only
- `authenticated_agent` — identity is known, but task and risk remain advisory
- `trusted_runtime` — a configured issuer attests the full purpose claims
- `local_operator` — a local process/OS authority attests the full purpose claims without a cloud dependency

The attestation binds, at minimum, request ID, subject, namespace, project, task type, intended action, action risk, issue time, expiry, and a nonce. Use a versioned opaque token on the wire and a pluggable verifier interface. An ExecLayer-issued token is one supported integration, not a required dependency. Local deployments can use a locally stored key or process-bound verifier.

Strict mode requires a trusted-runtime or local-operator attestation. Missing, expired, replayed, mismatched, or unknown-issuer attestations fail closed. Self-declared or merely authenticated-agent claims may request stricter treatment, but may never lower risk, satisfy an allowlist, or unlock restricted memory.

### Human confirmation receipt

Replace the caller-provided `human_confirmed` boolean with an opaque, verified receipt. The receipt binds:

- receipt ID and confirming actor
- request ID or canonical purpose hash
- namespace, project, task type, and intended action
- maximum authorized risk
- issue time and expiry
- issuer and signature/MAC version

Critical confirmations are single-use by default; lower-risk deployments may configure bounded replay for an identical purpose. Authentication of an agent is not evidence that a human confirmed the action.

### Backward compatibility

- Missing `RecallPurpose` means legacy permissive behavior only while influence enforcement is disabled or advisory.
- Config flag `influence.require_purpose` defaults to `false`.
- When enabled, missing purpose returns a reason-coded error rather than silently guessing.
- Strict mode never falls back from failed attestation verification to self-declared purpose.

### API changes

REST:

- Keep `GET /context` as the legacy/advisory compatibility surface. Do not put intended actions, attestations, or confirmation receipts in URLs, proxy logs, or caches.
- Add `POST /context/evaluate` with a structured JSON body for governed retrieval.

MCP:

- Extend `get_context` and `memory_skim` with optional purpose fields.
- Add structured influence metadata to each result.
- Extend `search_memories`, `search_global`, `retrieve_original`, and `inject_context` through the same verified-purpose path before strict mode is available over MCP.

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
- bind namespace and project to the authenticated/attested scope rather than trusting duplicate caller fields
- reject unknown task types when a positive allowlist is in force; aliases require an explicit versioned registry
- validate purpose and confirmation expiry, nonce/replay state, issuer, signature/MAC, and claims hash

REST already supports per-agent key identity. MCP currently has a legacy shared bearer-token mode and therefore cannot claim per-agent identity from transport authentication alone. Before strict MCP enforcement ships, add per-agent MCP key/session identity or require a trusted purpose attestation on every strict call. Shared-token and unauthenticated stdio consumers remain advisory unless a local-operator verifier establishes identity and scope.

### Tests

- legacy requests remain unchanged
- authenticated identity overrides spoofed request identity
- purpose-required mode rejects absent purpose
- task normalization is deterministic
- self-declared purpose cannot lower risk or satisfy confirmation
- attestation and confirmation claims are scope-bound, expiring, and replay-checked
- a shared MCP bearer token cannot masquerade as per-agent authority
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
    pub decision_id: String,
    pub request_id: String,
    pub memory_id: i64,
    pub decision: InfluenceDecisionKind,
    pub reason_codes: Vec<String>,
    pub policy: MemoryInfluencePolicy,
    pub purpose: VerifiedRecallPurpose,
    pub policy_version: u64,
    pub evaluator_version: String,
    pub config_hash: String,
    pub evidence_root_count: usize,
    pub derivation_depth: u32,
    pub contradiction_set_ids: Vec<String>,
}
```

PR 3 defines contradiction annotation behind a provider interface that returns no memberships until PR 4 installs the contradiction-set backend. This preserves delivery order without coupling the evaluator to a table that does not yet exist.

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
- `purpose_unattested`
- `purpose_attestation_invalid`
- `purpose_attestation_expired`
- `purpose_attestation_replayed`
- `confirmation_receipt_invalid`
- `confirmation_receipt_expired`
- `confirmation_receipt_replayed`
- `consumer_cannot_enforce_reasoning_only`
- `policy_version_conflict`

### Evaluation order

1. purpose and consumer-capability verification
2. storage-governance eligibility
3. influence state
4. task allow/deny rules
5. action-risk threshold
6. derivation-depth limit
7. source requirement
8. confirmation-receipt verification
9. contradiction annotation

Deny rules override allow rules.

### Context assembly behavior

- `Allow`: inject normally.
- `AllowReasoningOnly`: return in a separate structured `advisory_memories` channel with a machine-readable no-action-authority marker. For strict action requests, omit it unless the attested downstream consumer declares and enforces channel separation. File-based `IRONMEM.md` injection is not such a consumer.
- `RequireOriginalSource`: expand CCR or source-backed chunk before injection. Deny if exact evidence is unavailable.
- `RequireHumanConfirmation`: exclude unless a valid, scope-bound confirmation receipt is present.
- `Deny`: exclude and report the reason in trace metadata.

### Shared egress gate

Create one shared service function that accepts ranked candidate IDs, a verified purpose, and consumer capabilities, then returns separately typed authorized, advisory, source-backed, and denied decisions. All content-bearing surfaces must call it:

- REST `/context`, `/skim`, `/context/evaluate`, source retrieval, and Workbench evidence views
- MCP `get_context`, `memory_skim`, `search_memories`, `search_global`, `retrieve_original`, and `inject_context`
- CLI search, context, lineage expansion, and injection
- session-start and hook-driven `IRONMEM.md` generation
- SDK methods that expose any of the above

Internal ranking and benchmark code may inspect candidate IDs before the gate. No blocked summary, tag, chunk, original-source text, graph payload, or sensitive reason detail crosses the boundary. Denied results expose identifiers and bounded reason codes only to callers authorized for denial diagnostics.

### Injection audit

Extend `injection_events` or create `memory_influence_events`:

```sql
CREATE TABLE IF NOT EXISTS memory_influence_events (
    id TEXT PRIMARY KEY,
    decision_id TEXT NOT NULL UNIQUE,
    request_id TEXT NOT NULL,
    memory_id BIGINT NOT NULL,
    project TEXT NOT NULL,
    namespace TEXT NOT NULL,
    session_id TEXT,
    agent_id TEXT,
    purpose_authority TEXT NOT NULL,
    attestation_id TEXT,
    confirmation_receipt_id TEXT,
    task_type TEXT NOT NULL,
    intended_action_hash TEXT,
    action_risk TEXT NOT NULL,
    decision TEXT NOT NULL,
    reason_codes TEXT NOT NULL,
    policy_version BIGINT NOT NULL,
    evaluator_version TEXT NOT NULL,
    config_hash TEXT NOT NULL,
    purpose_hash TEXT NOT NULL,
    query_hash TEXT,
    memory_record_hash TEXT,
    rank BIGINT,
    created_at BIGINT NOT NULL
);
```

Record both allowed and denied decisions. This creates a full memory-to-context influence trail.

Influence events intentionally do not cascade from live memory rows: governed deletion may remove memory content while the append-only audit trail retains a non-content record hash and tombstone-safe decision evidence. Access, retention, and erasure rules for audit events are applied explicitly rather than through a content-table foreign key.

Do not store raw queries or intended-action secrets by default. Store a canonical purpose hash and query hash; an optional redacted excerpt may be retained under the namespace's classification, retention, legal-hold, and erasure rules. Audit events need their own retention and access policy so a governance feature does not become a new sensitive-data leak.

### Performance target

- p50 evaluator overhead under 1 ms per candidate for local SQLite
- p99 evaluator overhead under 5 ms per candidate
- batch-load policies and contradiction annotations to avoid N+1 queries

### Tests

- table-driven evaluator tests for every decision path
- deny precedence
- exact-source expansion success and failure
- confirmation behavior
- strict mode rejects self-declared risk and confirmation
- every content-bearing surface invokes the shared gate
- blocked content is absent from payloads, files, chunks, source expansion, and logs
- reasoning-only content is excluded when the consumer cannot enforce channel separation
- batch evaluation ordering
- influence events record allowed and denied cases
- recorded policy/config/evaluator versions reproduce the same deterministic decision
- latency benchmark or bounded microbenchmark

### Exit criteria

- all context injection flows call the evaluator
- all search, skim, expansion, Workbench, SDK, and file-injection egress flows call the evaluator
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
    preferred_memory_id BIGINT,
    status TEXT NOT NULL DEFAULT 'unresolved',
    resolution_basis TEXT,
    resolved_by TEXT,
    version BIGINT NOT NULL DEFAULT 1,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL
);

CREATE TABLE IF NOT EXISTS contradiction_members (
    contradiction_set_id TEXT NOT NULL,
    memory_id BIGINT NOT NULL,
    stance TEXT NOT NULL DEFAULT 'competing',
    created_at BIGINT NOT NULL,
    PRIMARY KEY(contradiction_set_id, memory_id),
    FOREIGN KEY(contradiction_set_id) REFERENCES contradiction_sets(id) ON DELETE CASCADE,
    FOREIGN KEY(memory_id) REFERENCES memory_meta(memory_id) ON DELETE CASCADE
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

"Incompatible" must be defined by a versioned claim schema, not by unequal target strings alone. The schema canonicalizes claim keys and declares relation cardinality (`single`, `set`, `ordered`, or custom). Two languages, dependencies, or team members may be simultaneously true; a single-valued deployment target or authentication mode may not be. Unknown relations are annotated as potential conflicts but do not automatically create a contradiction set.

User-scoped memories can participate across projects. Represent their contradiction realm explicitly rather than forcing a fake project path; use a normalized scope/realm field or a reserved, validated user-scope project value consistently across both backends.

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
- multi-valued relations do not create false contradictions
- user-scoped contradictions do not require a fake project identity
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
11. self-declared low risk versus attested high risk
12. forged, expired, replayed, and cross-scope purpose attestations
13. missing, forged, expired, replayed, and cross-scope confirmation receipts
14. reasoning-only memory requested through capable and incapable consumers
15. blocked memory requested through every content-bearing egress surface

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
- purpose-attestation enforcement accuracy
- confirmation-receipt enforcement accuracy
- cross-surface blocked-content leakage rate
- decision reproducibility rate
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
- `influence_unattested_cannot_downgrade_risk`
- `influence_attestation_scope_and_replay`
- `influence_confirmation_scope_and_replay`
- `influence_reasoning_only_consumer_capability`
- `influence_all_egress_surfaces_block_content`
- `influence_decision_reproduction`

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
    "mode": "advisory",
    "require_purpose": false,
    "require_trusted_attestation": false,
    "record_denials": true,
    "fail_closed_on_policy_error": true,
    "confirmation_replay_protection": true,
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

Allowed modes:

- `advisory` — self-declared purpose is accepted and decisions are returned as guidance
- `strict` — full purpose attestation is mandatory and all governed egress fails closed

Setting `mode=strict` implies `require_purpose=true`, `require_trusted_attestation=true`, and `fail_closed_on_policy_error=true`; contradictory overrides are configuration errors. Trusted issuer keys or local verifier material are loaded from protected files, OS key storage, or secret references, never embedded in the public settings document.

Environment overrides:

- `IRONMEM_INFLUENCE_ENABLED`
- `IRONMEM_INFLUENCE_REQUIRE_PURPOSE`
- `IRONMEM_INFLUENCE_MODE`
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

In strict mode these tools accept opaque attestation and confirmation-receipt fields and resolve identity through the transport session or verifier. They never accept a caller-provided `agent_id` as authority. Legacy shared-token MCP and unverified stdio sessions remain advisory until a trusted local/session identity mechanism is configured.

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

`POST /context/evaluate` is the only strict full-purpose context endpoint. Legacy `GET /context` must not accept attestation tokens, confirmations, or sensitive intended-action fields in query parameters. Policy mutations require policy-write capability and expected-version concurrency control.

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

Policy mutations also require policy-write authority and `--expected-version`. In strict mode, non-interactive recall accepts an attestation/confirmation from a protected file descriptor or local verifier integration; do not place bearer tokens or confirmation receipts directly in shell arguments where process listings and history may expose them. Interactive local confirmation mints a scoped, expiring local-operator receipt rather than setting a boolean.

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

It also displays whether the result is advisory or strictly enforced, the verified authority, policy version, evaluator version, and consumer capability. Simulation never mints a confirmation receipt and never represents itself as a real authorized request. Policy editing controls are hidden or disabled without policy-write capability.

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

- backfill any missing `memory_meta` parent rows before dependent tables
- add evidence-root columns and table
- backfill roots and depth
- verify no cycles

### Migration B

- create influence-policy and influence-event tables
- insert no policy rows unless a memory deviates from permissive defaults
- initialize policy versions and preserve append-only event history independently of memory-content deletion

### Migration C

- create contradiction tables

All migrations must:

- support SQLite and Postgres
- remain restart-safe
- record migration version
- avoid rewriting the memory ledger
- produce a deterministic repair report for malformed lineage
- use `memory_meta(memory_id)`, not backend-specific `memories.id`, as the relational parent for live metadata
- run large backfills in bounded, restart-safe batches with a durable cursor

Rollback means disabling enforcement, not deleting new metadata.

---

## 19. Security review

Threats to test:

- caller spoofs `agent_id`
- caller omits purpose to bypass policy
- caller understates task type or action risk
- caller forges, replays, or cross-scopes a purpose attestation
- agent authentication is misrepresented as human confirmation
- caller replays or broadens a confirmation receipt
- malformed task names bypass deny rules
- derived memories reset their depth
- multi-source synthesis hides weak roots
- policy update occurs outside namespace allowlist
- namespace reader relaxes policy without policy-write authority
- concurrent policy writers silently overwrite one another
- policy errors fail open
- contradiction preference hides competing evidence
- source-required decisions inject compressed summaries instead of originals
- denied memory still enters `IRONMEM.md`
- denied memory leaks through search, skim, source expansion, Workbench, SDK, or logs
- reasoning-only memory is mixed into a strict action prompt consumed by a runtime that cannot enforce channel separation
- stale cached policies survive mutation
- raw query or intended-action text turns the influence ledger into a sensitive-data leak

Security decisions:

- authenticated identity overrides supplied identity
- authenticated identity alone does not attest task, risk, or human confirmation
- strict purpose is verified through a trusted-runtime or local-operator authority
- confirmation uses a scoped, expiring, replay-protected receipt
- enforcement mode fails closed on evaluator errors
- strict mode never degrades silently to advisory mode
- policy relaxation requires a distinct policy-write capability and expected version
- policy cache invalidates on ledgered mutation
- all content-bearing egress surfaces use one shared evaluation path
- sensitive purpose/query content is hashed or redacted by default and governed when retained

---

## 20. Performance plan

- batch-load policy rows for candidate memory IDs
- batch-load contradiction memberships
- calculate evidence-root counts in one grouped query
- verify one request-level purpose attestation and confirmation receipt before per-candidate evaluation
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
- cross-backend `memory_meta` parent integrity
- write-path propagation
- lineage output
- deterministic tests

### PR 2: Influence model

- types and policy storage
- versioned CLI/REST/MCP policy CRUD
- policy read/write capabilities and concurrency control
- ledger integration
- tests

### PR 3: Purpose-bound evaluation

- advisory and verified recall-purpose types
- pluggable trusted-runtime/local-operator attestation verifier
- scoped confirmation receipts and replay protection
- evaluator
- one shared egress gate across context, search, skim, source, Workbench, SDK, CLI, MCP, and file injection
- explicit reasoning-only consumer capability contract
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
- strict callers prove purpose through a trusted local or external attestation
- human confirmation is represented by a scoped, expiring receipt rather than a boolean
- influence policy evaluates before every content-bearing memory egress path
- denied memories never enter context
- denied memories never leak through search, skim, source expansion, Workbench, SDKs, logs, or `IRONMEM.md`
- reasoning-only memories are separated, labeled, and audited, and are excluded when a strict consumer cannot enforce channel separation
- source-required memories expand exact evidence before injection
- contradictions remain visible and auditable
- lineage includes allowed and denied influence attempts
- deterministic counterfactual cases gate CI
- influence events record policy, evaluator, configuration, purpose, and confirmation versions sufficient to reproduce a decision
- permissive mode preserves current retrieval scores within 0.5 points
- enforcement remains disabled by default for existing users

---

## 23. Non-goals

- claims of artificial consciousness
- quantum-computing implementation
- replacement of ExecLayer
- dependence on ExecLayer or any cloud service for purpose verification
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

> IronMem can govern which memories leave the store for an attested task, preserves independent evidence lineage, and records every allowed or denied memory-influence decision.

This preserves the narrow adoption wedge while accurately describing the technical moat.

When only advisory mode is enabled, use narrower language:

> IronMem evaluates and records how stored memories should influence a declared task. Strict enforcement is available when a trusted local or external runtime attests the task purpose.
