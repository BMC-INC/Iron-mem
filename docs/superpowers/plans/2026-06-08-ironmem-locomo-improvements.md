# IronMem LoCoMo Score Improvements — Implementation Plan

> **For implementation:** Execute inline in the main session (per project preference: no subagents — see [[feedback_no_subagents]]). Use superpowers:executing-plans for checkpoint discipline. Steps use checkbox (`- [ ]`) syntax for tracking. Work in a dedicated worktree or branch (`feat/locomo-retrieval`).

**Goal:** Make IronMem preserve atomic facts (dates, proper nouns, quantities) through compression and surface them at retrieval, so LoCoMo session-only accuracy climbs from 31.6% toward 65%+ and the fact-augmented path toward 80%+ — without regressing coding-session memory quality.

**Architecture:** Move the "explicit fact extraction" that the benchmark's *hybrid* strategy does as a separate pass **into the write path**: compression emits a narrative summary AND a structured fact list in one LLM call, stored as searchable `kind=fact` memories. Add temporal tags and an entity inverted-index so date- and name-anchored questions resolve by direct lookup, not just semantic ranking. Then fuse keyword + vector + entity signals at retrieval.

**Tech Stack:** Rust, SQLite (FTS5 + sqlite-vec), `fastembed` (local-onnx, BGE-small-en-v1.5), Anthropic provider for compression. Benchmark harness: Python 3.11 (`uv`) in the separate `ironmem-locomo-benchmark` repo.

---

## Evidence motivating this plan (conv-26 dry-run, 199 Qs, Claude-judged)

| Category | session (compression) | hybrid (+ explicit facts) | Δ |
|---|---|---|---|
| single-hop | 34.3% | 54.3% | +20.0 |
| multi-hop | **46.9%** | 37.5% | −9.4 |
| open-domain | 53.8% | 61.5% | +7.7 |
| temporal | 5.4% | **86.5%** | **+81.1** |
| overall (1–4) | 31.6% | 59.2% | +27.6 |

**Read:** temporal information *survives the conversation* (hybrid 86.5%) but *not the compression step* (session 5.4%). Per-question logs show the compressor turning "Caroline joined the LGBTQ group on 7 May 2023" into "attended LGBTQ+ events." Multi-hop *regresses* under explicit facts (compression's narrative preserves cross-turn links), so the goal is **both** signals available, not facts replacing narrative.

**Root causes (all confirmed in source):**
1. Compression prompt is coding-tuned and lossy for specifics — `src/provider.rs:54,77` ("tool calls from a coding session", "what was built, changed, or decided. Include specific file names…"). No instruction to keep dates/proper nouns/quantities.
2. No structured facts at write time — `parse_response` (`src/provider.rs:94`) only extracts SUMMARY/TAGS/IMPORTANCE/KIND.
3. No temporal tag on memories — `memory_meta` (`src/db.rs:227`) has scope/kind/session_blob but no event time; observations stamp wall-clock "now".
4. No entity index — retrieval is FTS + vector only (`src/retrieval.rs:46 hybrid_search`); a name must rank in top-k to be seen.
5. Shallow depth — `config.inject_limit = 5` (`src/config.rs:138`); benchmark `retrieve_limit = 10`.

## Success gate (do NOT run the full 10-conversation benchmark until met)

Re-run `--dry-run` on conv-26 after each tier. **Gate to full run:** session-only ≥ 70% **and** fact-augmented ≥ 80% (overall, cats 1–4). Then run all 10 conversations with the **GPT-4o judge** (`OPENAI_API_KEY` in `.env`) — that is the publishable result. Record per-tier deltas in `results/` so each change's impact is attributable.

---

## File Structure

**IronMem repo (`/Users/kingjames/Desktop/Iron-mem`):**
- `src/provider.rs` — compression prompt + response parser. Tier 1.1 (prompt), 1.2 (FACTS section + parse), 1.3 (WHEN field).
- `src/compress.rs` — `run`/`persist`. Tier 1.2 (store facts), 1.3 (store event_time), 1.4 (store entities).
- `src/db.rs` — schema + queries. Tier 1.3 (`memory_meta.event_time`), 1.4 (`memory_entities` table + lookup), 2.1 (entity fusion), 3.1 (provenance).
- `src/retrieval.rs` — `hybrid_search`. Tier 1.4/2.1 (entity signal + RRF), 2.3 (depth), 3.2 (rerank).
- `src/config.rs` — `inject_limit`. Tier 2.3.

**Benchmark repo (`/Users/kingjames/Desktop/ironmem-locomo-benchmark`):**
- `benchmark/config.py`, `benchmark/run.py` — `retrieve_limit`, re-measurement only. No new logic; the dual-output change makes `session` ≈ today's `hybrid`, so after Tier 1.2 the `hybrid` strategy becomes an upper-bound check, not the headline.

---

## Phase 0: Baseline + cheap wins (do first, ~30 min)

### Task 0.1: Capture current compression output as evidence
**Files:** none (investigation). 
- [ ] Query 3 temporal-failing memories: `curl "http://127.0.0.1:37778/context?project=/benchmark/locomo/conv-26__session&query=LGBTQ&limit=5"` and save the `summary` text into `docs/superpowers/plans/notes/conv26-compression-before.md`. This is the before-image for the prompt rewrite.

### Task 0.2: Retrieval-depth experiment (cheapest possible signal)
**Files:** `src/config.rs:138`, benchmark `benchmark/config.py` (`retrieve_limit`).
- [ ] Bump `inject_limit: 5` → `10` in `src/config.rs:138`; rebuild release `--features local-onnx`, deploy, re-kick launchd.
- [ ] Bump `retrieve_limit: int = 10` → `20` in `benchmark/config.py`.
- [ ] Re-run `uv run python -m benchmark.run --dry-run --skip-ingest --concurrency 6`.
- [ ] Record delta. **Expected:** small lift on single/multi-hop if relevant memories were ranking 11–20; ~0 on temporal (the date isn't in any memory yet — that's Tier 1). Keep the bump only if it helps.

---

## Chunk / Tier 1 — Highest impact (build first)

### Task 1.1: Domain-agnostic, fact-preserving compression prompt

**Files:**
- Modify: `src/provider.rs:52-92` (`build_prompt`)
- Test: `src/provider.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1 — Failing test:** assert the prompt instructs preservation of specifics and is not coding-only.
```rust
#[test]
fn prompt_preserves_specifics_and_is_domain_agnostic() {
    let p = build_prompt(&[]);
    assert!(p.contains("dates"), "must ask to keep dates");
    assert!(p.contains("proper nouns") || p.contains("names"));
    assert!(!p.contains("coding session"), "must not assume coding");
}
```
- [ ] **Step 2 — Run, expect FAIL:** `cargo test --bin ironmem prompt_preserves_specifics`
- [ ] **Step 3 — Rewrite the prompt.** Replace the system line (`:54`) and SUMMARY line (`:77`):
```rust
// system line
"You are a memory system. Analyze this session — which may be a coding session, a conversation, or any activity — and produce a faithful, compact memory entry.".to_string(),
// SUMMARY line
"SUMMARY: [3-6 sentences. PRESERVE every specific: exact dates and times, proper nouns (people, places, organizations, events), quantities, file names, and key quoted statements. Keep causal relationships (X because Y). Do not generalize specifics away — \"attended an LGBTQ support group on 7 May 2023\", never \"attended social events\".]".to_string(),
```
- [ ] **Step 4 — Run, expect PASS.**
- [ ] **Step 5 — Commit:** `fix(provider): compression prompt preserves dates, names, quantities (domain-agnostic)`

### Task 1.2: Dual-output compression — structured FACTS baked into the write path

This is the single highest-impact change: it turns the benchmark's separate "hybrid" extraction into an automatic part of compression, so `session` ≈ today's `hybrid` (59%).

**Files:**
- Modify: `src/provider.rs` — add `facts` to `CompressionResult`; emit `FACTS:` block in `build_prompt`; parse it in `parse_response`.
- Modify: `src/compress.rs:92` (`persist`) — after writing the narrative memory, store each fact as a `kind=fact` memory (reuses existing `remember`/fact infra + embedding + FTS).
- Test: `src/provider.rs` tests, `src/compress.rs` tests (`persist_*` pattern at `:197`).

- [ ] **Step 1 — Failing parser test:**
```rust
#[test]
fn parse_response_extracts_facts_block() {
    let r = parse_response("SUMMARY: s\nFACTS:\n- Caroline joined the LGBTQ group on 7 May 2023\n- Melanie painted a sunrise in 2022\nTAGS: a b\nIMPORTANCE: 6");
    assert_eq!(r.facts.len(), 2);
    assert!(r.facts[0].contains("7 May 2023"));
}
```
- [ ] **Step 2 — Run, expect FAIL** (no `facts` field).
- [ ] **Step 3 — Implement:**
  - Add `pub facts: Vec<String>` to `CompressionResult` (near `:44`).
  - Add to `build_prompt` after the SUMMARY line:
```rust
lines.push("FACTS: [then one atomic fact per line, each starting with \"- \". Each fact must be self-contained and carry its own entity + any date/quantity, e.g. \"- Caroline joined the LGBTQ support group on 7 May 2023\". Extract every concrete fact; omit chit-chat.]".to_string());
```
  - In `parse_response`, collect lines after a `FACTS:` marker that start with `- ` into `facts` (stop at the next `TAGS:`/`IMPORTANCE:`/`KIND:` marker). Keep existing fields working.
- [ ] **Step 4 — Run parser test, expect PASS.**
- [ ] **Step 5 — Failing persist test:** extend the `persist_*` tests to assert that facts become retrievable `kind=fact` memories.
```rust
#[tokio::test]
async fn persist_stores_facts_as_searchable_memories() {
    // build a CompressionResult with facts, persist, then search_memories for a fact term
    // assert a kind=fact memory exists containing the date string.
}
```
- [ ] **Step 6 — Run, expect FAIL.**
- [ ] **Step 7 — Implement in `persist`:** after the narrative memory is written, for each `result.facts` entry call the existing fact-storage path (mirror `compress::remember` → `insert_memory` + `set_memory_scope_kind(kind="fact")` + inline embed). Tag them so they're attributable (e.g. `tags="fact session:<id>"`).
- [ ] **Step 8 — Run, expect PASS; full suite `cargo test --bin ironmem`.**
- [ ] **Step 9 — Commit:** `feat(compress): dual-output compression — emit + store structured facts at write time`
- [ ] **Step 10 — Measure:** rebuild/deploy, re-run `--dry-run --wipe`. **Expected:** session-only overall jumps toward ~55–60% (temporal especially), approaching today's hybrid — now with no separate extraction pass.

### Task 1.3: Temporal tagging + time-aware retrieval

**Files:**
- Modify: `src/db.rs` — `ALTER TABLE memory_meta ADD COLUMN event_time TEXT` (follow the existing duplicate-column-ignored migration pattern at `:285-321`); add `event_time` to `MemoryMetaInfo`/setters.
- Modify: `src/provider.rs` — emit `WHEN:` (a date or range if the session states one); parse into `CompressionResult.event_time: Option<String>`.
- Modify: `src/compress.rs:persist` — store `event_time` on `memory_meta`.
- Modify: `src/retrieval.rs:hybrid_search` — optional time-window boost/filter when the query implies a date.
- Test: `src/db.rs` tests (migration + round-trip), `src/provider.rs` (parse WHEN).

- [ ] **Step 1 — Failing test:** `memory_meta` round-trips `event_time`.
- [ ] **Step 2 — Run, expect FAIL.**
- [ ] **Step 3 — Migration + setter/getter** following the `scope`/`kind` ALTER pattern (`src/db.rs:299-321`).
- [ ] **Step 4 — Parse `WHEN:`** in `provider.rs`; store in `persist`.
- [ ] **Step 5 — Run tests, expect PASS.**
- [ ] **Step 6 — Time boost in retrieval:** in `hybrid_search`, when the query contains a parseable month/year, add a recency-independent boost to memories whose `event_time` falls in-window (cheap string match on year/month is sufficient for LoCoMo). Keep it additive — never filter out non-dated memories.
- [ ] **Step 7 — Commit:** `feat: temporal tagging on memories + time-aware retrieval boost`
- [ ] **Step 8 — Measure** (temporal category specifically).

### Task 1.4: Entity inverted index

**Files:**
- Modify: `src/db.rs` — `CREATE TABLE IF NOT EXISTS memory_entities (memory_id INTEGER, entity TEXT)` + index on `lower(entity)`; insert + `memories_for_entity(entity) -> Vec<i64>`.
- Modify: `src/provider.rs` — emit `ENTITIES:` (proper nouns); parse to `Vec<String>`.
- Modify: `src/compress.rs:persist` — populate `memory_entities` for the memory + its facts.
- Modify: `src/retrieval.rs:hybrid_search` — extract candidate entities from the query, look up memory_ids, add as a third RRF signal.
- Test: `src/db.rs` (insert + lookup), `src/retrieval.rs` (entity hit surfaces a memory FTS/vector miss).

- [ ] **Step 1 — Failing test:** insert entity rows, `memories_for_entity("Caroline")` returns the id.
- [ ] **Step 2 — Run, expect FAIL.**
- [ ] **Step 3 — Table + queries** (mirror existing `db.rs` CREATE/insert patterns).
- [ ] **Step 4 — Emit/parse `ENTITIES:`; populate in `persist`.**
- [ ] **Step 5 — Run, expect PASS.**
- [ ] **Step 6 — Wire into `hybrid_search`** as a third id list into `rrf_fuse` (the fusion helper already exists at `src/retrieval.rs`).
- [ ] **Step 7 — Commit:** `feat: entity inverted index + entity-aware retrieval`
- [ ] **Step 8 — Measure** (single-hop especially).

**Tier 1 checkpoint:** re-run `--dry-run --wipe`. Target session-only ≥ ~60%. If temporal is still low, inspect whether the date made it into a fact memory (compression) vs. failed to retrieve (ranking) — fix the failing stage before moving on.

---

## Chunk / Tier 2 — Meaningful gains (after Tier 1)

### Task 2.1: Multi-signal retrieval fusion
Already partially present (`hybrid_search` does FTS + vector RRF). Extend to fuse **three** signals: FTS + vector + entity-index (Task 1.4), dedup by memory_id, then return fused order. Acceptance: a question whose answer memory misses FTS *and* ranks low semantically but matches an entity is retrieved. **File:** `src/retrieval.rs`. TDD via a constructed case.

### Task 2.2: Compression prompt hardening
Folded into Task 1.1; if Tier 1 measurement shows specific loss patterns (e.g. quantities, relative dates like "the Sunday before"), add targeted instructions and a regression note in the prompt test. **File:** `src/provider.rs`.

### Task 2.3: Retrieval depth (already trialed in 0.2)
Lock in the depth that measured best. **Files:** `src/config.rs`, `benchmark/config.py`.

**Tier 2 checkpoint:** re-run `--dry-run`. Target session-only ≥ 70%, fact-augmented ≥ 80% → **gate to full run met.**

---

## Chunk / Tier 3 — Competitive edge (for the full benchmark)

### Task 3.1: Fact conflict resolution by provenance
When a newer `kind=fact` memory contradicts an older one (same entity+attribute), prefer the newer. Track `event_time` (Task 1.3) as provenance; at retrieval, when two facts share an entity and conflict, keep the most recent. **Files:** `src/db.rs`, `src/retrieval.rs`. Scope carefully — only needed if the full run shows contradiction-driven judge failures.

### Task 3.2: Cross-encoder / LLM reranking
After initial retrieval, a second pass scores each candidate against the query (one LLM call ranking the top ~20 → keep top ~10). Expensive; gate behind a config flag (`rerank: bool`). **Files:** `src/retrieval.rs`, `src/config.rs`. This is the 75%→85% lever.

### Task 3.3: Graph edges between memories
Link memories sharing an entity (`memory_edges(a, b, entity)`); multi-hop questions traverse from one entity's memories to a related entity's. IronMem already wins multi-hop with narrative alone (46.9% vs 37.5%), so this is upside, not a fix. **Files:** `src/db.rs`, `src/retrieval.rs`.

---

## Re-measurement protocol (run after every task that touches compression or retrieval)

```bash
# IronMem: rebuild + deploy + re-kick (warm onnx target ⇒ ~3 min)
cd /Users/kingjames/Desktop/Iron-mem
cargo build --release --features local-onnx
cp target/release/ironmem ~/.ironmem/bin/ironmem.new && chmod 755 ~/.ironmem/bin/ironmem.new && mv -f ~/.ironmem/bin/ironmem.new ~/.ironmem/bin/ironmem
launchctl kickstart -k gui/$(id -u)/com.execlayer.ironmem   # embedder loads from ~/.ironmem/fastembed_cache

# Benchmark: clean re-ingest + score on conv-26
cd /Users/kingjames/Desktop/ironmem-locomo-benchmark
uv run python -m benchmark.run --dry-run --wipe --concurrency 6
```

Keep each tier's `results/*.json` committed so the per-tier deltas are auditable.

## Notes / risks
- **Don't regress coding memory.** The prompt rewrite (1.1) generalizes; verify on a real coding session that SUMMARY still captures file names/decisions (it explicitly still asks for them).
- **Cost.** Dual-output adds tokens per compression but removes the separate hybrid extraction pass — net neutral, and it's local-first (Anthropic provider already in use).
- **Once 1.2 lands, the benchmark's `hybrid` strategy is redundant** (facts are baked into `session`). Repurpose `--strategy hybrid` as an upper-bound sanity check, not the headline.
- **Judge:** all gating measurements use the Claude judge for speed; the FINAL full-run number must use GPT-4o for comparability to published Mem0/Zep.

Relates to [[project_ccr_supermemory_plan]], [[project_ironmem]], [[feedback_no_subagents]], [[feedback_completeness]], [[feedback_build_speed]].
