# Reliability and coding evidence

Status: all five implementation areas complete; integrated local validation passed. Local model canaries did not establish a coding-accuracy advantage, and no competitor system was scored. See `docs/benchmarks/reliability/VALIDATION.md`. Release status is tracked by the associated pull request; this document records implementation and evaluation scope. Public synthetic fixtures only; no installed service or personal database changes.

## Acceptance

1. A paired coding-task runner isolates each arm, uses identical task/model/settings/budgets, executes hidden deterministic checks, and publishes per-task failures, latency, bytes, and provenance. Include no-memory and full-history baselines. A deterministic adapter validates the harness; its scores must never be presented as LLM or competitive results. External model/competitor adapters use an explicit local command protocol, without implicit paid calls.
2. Isolated release-build scale and recovery runs verify interrupted transactions, SQLite page exhaustion, corruption refusal, concurrent writes, full/delta recovery and repeated cycles. Report tested sizes/durations, not an unbounded reliability claim.
3. Shared CLI/REST/MCP diagnostics explain actual lexical candidate rank, shared policy decisions, source availability, telemetry and generated-context freshness. Do not fabricate hybrid ranking explanations. Explicit administrative capability and namespace checks precede inspection; content passes the existing gate.
4. An offline native SQLite retention inventory distinguishes active records, immutable history, shared sources and snapshots. Physical whole-store purge requires a content-bound plan, refuses legal holds and live writers, and reports external backups/files and storage-device erasure limits. Selective history rewriting is not safe for hash chains; keep normal governed forget for selective logical deletion.
5. Isolated configuration, safe/idempotent hook installation, backup-before-upgrade and a newcomer rehearsal cover installation, MCP initialization, save/search, export/import, and recovery. Preserve existing settings/hooks. No live install during development.

## Sequence

- Shared configuration isolation and diagnostics.
- Coding benchmark and release recovery/scale runner.
- Explicit offline retention workflow and installer/rehearsal improvements.
- Focused checks per component, one integrated checkpoint, public README/results, PR and green merge using the previously authorized cadence.

## Evidence boundaries

Paid model calls and personal corpora are not needed for implementation or deterministic validation. Competitive superiority requires measured paired model runs; missing scores remain unmeasured. Optional features remain opt-in. Retain only small public reports and one shared build tree; clean task-owned fixtures and temporary databases.
