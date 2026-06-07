# IronMem CCR + Supermemory Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking. The user has explicitly authorized subagent-driven-development for execution.

**Goal:** Make every memory IronMem stores **losslessly reversible** (Headroom/CCR pattern — compress, cache, retrieve the verbatim original on demand) and extend the memory model with **user/project scoping, typed memories, and an always-injected user profile** (Supermemory patterns) — all native Rust, local-first, zero data egress.

**Architecture:** Add a content-addressed blob store (`ccr`) inside the existing SQLite/Postgres DB. Tool outputs and pre-LLM session blobs are stored whole, deduplicated by content hash, and compressed through per-content-type **byte-exact reversible** codecs (zstd + type-specific dictionaries / invertible preprocessing). The lossy LLM summary stays as the retrieval/ranking surface; a new `retrieve_original` MCP tool pulls the exact bytes back. Separately, memory scope (`project|user`) and `kind` (typed) move into `memory_meta` (no FTS5 rebuild), enabling a global user profile that is regenerated from user-scoped memories and injected at every session start.

**Tech Stack:** Rust 2021 · sqlx (SQLite + Postgres) · rmcp (MCP) · **`zstd`** (new dep — vendored libzstd, dictionary-capable) · **`sha2`** (already transitive — content addressing) · existing embedder/vectorstore/retrieval layers.

---

## Why this plan (gap → fix)

| Gap in current IronMem | Fix | Phase |
|---|---|---|
| `insert_observation` slices `&o[..max_bytes]` on a raw byte offset → UTF‑8 boundary panic → with `panic="abort"` the **whole MCP process dies**. Same in `provider.rs:60,67`. | `safe_truncate` char-boundary helper, used everywhere. | 0 |
| Installed binary (`~/.ironmem/bin/ironmem`) is **Apr 1**; source is Jun 5. Claude runs stale code. | Rebuild + reinstall current `main`. | 0 |
| `src/*\ 2.rs`, `docs/superpowers 2/` = iCloud sync-conflict duplicates (untracked noise). | Delete + `.gitignore` guard. | 0 |
| Observations **truncated at 2048 B, original lost forever**. LLM summary is lossy with **no path back to the original**. | CCR: store full original, content-addressed + compressed + reversible; `retrieve_original` tool. | 1–3 |
| `mem.db` is 16.8 MB and grows lossily; no dedup. | Content-addressed dedup + per-type compression. | 1–3 |
| Memories are **project-scoped only**; one **type** (session summary); **no user profile**. | `scope` + `kind` in `memory_meta`; `remember` tool; user-profile memory. | 4–6 |

**Two parallel tracks after Phase 0:** CCR (Phases 1→2→3) and Memory-Model (Phases 4→5→6) are independent and can be executed concurrently by separate subagents.

---

## File structure (decomposition locked here)

```
src/
  ccr/
    mod.rs          # public API: store_blob, load_blob, BlobRef, ccr_stats, gc
    codec.rs        # Codec trait + registry + dispatch by ContentType
    detect.rs       # ContentType detection (json|code|log|diff|text|binary)
    zstd_codec.rs   # universal byte-exact zstd codec (the reversible floor)
    dict.rs         # per-type zstd dictionary load/train (Phase 2)
    json_codec.rs   # Phase 2 — dictionary-assisted, byte-exact
    log_codec.rs    # Phase 2 — invertible line-template transform + zstd
    diff_codec.rs   # Phase 2 — diff-token dictionary + zstd
    code_codec.rs   # Phase 2 — language-dictionary zstd (byte-exact); AST = documented future
    text_codec.rs   # Phase 2 — plain zstd alias (fallback)
  strutil.rs        # safe_truncate (Phase 0) + shared string helpers
  profile.rs        # Phase 5 — user profile extraction/regeneration
  db.rs             # +blobs table, +observations.output_blob, +memories↔blob link, +memory_meta.scope/kind
  mcp.rs            # +retrieve_original, +remember tools; wire CCR into record_event
  server.rs         # REST parity for retrieve_original / remember
  compress.rs       # store pre-LLM session blob in CCR; classify kind
  config.rs         # +CcrConfig (codec toggles, gc thresholds)
assets/
  dicts/            # Phase 2 — checked-in seed dictionaries per type (optional; lazy-train fallback)
```

**Convention note:** existing `src/` is flat. CCR is a 7-file cohesive subsystem, so it gets its own `src/ccr/` module dir — a deliberate, contained grouping (files that change together live together). Everything else follows existing flat-module patterns.

---

## Data model (additive, backward-compatible, both backends)

All new DDL is `CREATE TABLE IF NOT EXISTS` / additive columns, branched for SQLite (`BLOB`) vs Postgres (`BYTEA`) exactly like the existing `embeddings` table.

```sql
-- Content-addressed blob store (the "Cache" in CCR)
CREATE TABLE IF NOT EXISTS blobs (
    hash         TEXT PRIMARY KEY,    -- hex sha256 of the ORIGINAL bytes
    content_type TEXT NOT NULL,       -- json|code|log|diff|text|binary
    codec        TEXT NOT NULL,       -- zstd | json+zstd | log+zstd | ...
    orig_len     BIGINT NOT NULL,
    comp_len     BIGINT NOT NULL,
    data         BLOB NOT NULL,       -- BYTEA on Postgres; compressed bytes
    refcount     BIGINT NOT NULL DEFAULT 0,
    created_at   BIGINT NOT NULL
);

-- Observations gain a lossless pointer; inline `output` keeps a short FTS preview.
ALTER TABLE observations ADD COLUMN output_blob TEXT;   -- hash into blobs, nullable

-- Memories gain a pointer to the verbatim pre-LLM session blob behind the summary.
-- (SQLite memories is FTS5; store the link in memory_meta to avoid recreating the vtable.)
-- memory_meta gains scope + kind + optional session_blob.
ALTER TABLE memory_meta ADD COLUMN scope        TEXT NOT NULL DEFAULT 'project'; -- project|user
ALTER TABLE memory_meta ADD COLUMN kind         TEXT NOT NULL DEFAULT 'session'; -- session|error_solution|preference|architecture|learned_pattern|project_config|profile
ALTER TABLE memory_meta ADD COLUMN session_blob TEXT;                            -- hash into blobs, nullable
```

**Reversibility contract (every codec):** `decompress(compress(x)) == x` byte-for-byte, and `load_blob` re-hashes the decompressed bytes and **fails loudly** if the hash ≠ key. No lossy codec is ever registered. This is the property test that gates every codec.

---

## Key decisions (rationale)

1. **Native, not the Python package.** `pip install headroom-ai` adds a Python runtime, a proxy/egress surface, and breaks the local-first/no-egress hard constraint. We implement the *pattern* in Rust.
2. **Storage lives in the existing DB**, not loose files. `~/Desktop` is under iCloud sync (that's where the `* 2` duplicates came from); loose blob files would be sync-corruption bait. One `mem.db` keeps atomicity and matches the current single-file design.
3. **sha256 (`sha2`) for addressing** — already in the dep graph; no new hash crate. **zstd** is the single new direct dep; its first-class dictionary training is what makes per-type codecs genuinely beat the zstd floor.
4. **Honest codec depth.** JSON/log/diff/code codecs are **byte-exact reversible** and win via zstd dictionaries + (for logs) an invertible template transform. True **AST-aware** code normalization is byte-lossy and research-grade, so the code codec v1 is dictionary-assisted zstd (byte-exact); AST normalization is a **documented future enhancement, not a silent placeholder** (respects the no-placeholders rule).
5. **Supermemory is studied, not integrated.** We adopt three native patterns (scoping, typed memories, user profile) and explicitly do **not** add the hosted service.

---

## Chunk 0: Stabilize & ship (the "make it working" fix)

**Independently shippable. No new deps. This is the answer to "first make sure IronMem is working."**

### Task 0.1: UTF‑8-safe truncation helper

**Files:**
- Create: `src/strutil.rs`
- Modify: `src/main.rs` (add `mod strutil;`)
- Test: inline `#[cfg(test)]` in `src/strutil.rs`

- [ ] **Step 1 — Write the failing test**
```rust
// src/strutil.rs
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn truncates_on_char_boundary_never_panics() {
        let s = "héllo wörld ✓ multibyte"; // multibyte at various offsets
        for n in 0..s.len() + 4 {
            let out = safe_truncate(s, n);          // must never panic
            assert!(s.starts_with(out.trim_end_matches("… [truncated]")) || out == s || out.ends_with("[truncated]"));
        }
    }
    #[test]
    fn short_input_is_returned_unchanged() {
        assert_eq!(safe_truncate("abc", 100), "abc");
    }
    #[test]
    fn boundary_in_middle_of_multibyte_backs_up() {
        let s = "a✓b"; // '✓' is 3 bytes at offset 1..4
        let out = safe_truncate(s, 2);   // 2 is mid-'✓'
        assert!(out.starts_with('a'));
        assert!(out.ends_with("… [truncated]"));
    }
}
```
- [ ] **Step 2 — Run, verify it fails** — `cargo test --lib strutil` → FAIL (`safe_truncate` undefined)
- [ ] **Step 3 — Implement**
```rust
//! Shared string helpers. `safe_truncate` is the single choke point for
//! length-capping untrusted text so a multibyte char on the cap boundary can
//! never panic (which, under `panic="abort"`, would kill the whole process).

/// Truncate `s` to at most `max_bytes`, backing up to the nearest char
/// boundary, and append an ellipsis marker when truncation occurred.
pub fn safe_truncate(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… [truncated]", &s[..end])
}
```
- [ ] **Step 4 — Run, verify pass** — `cargo test --lib strutil` → PASS
- [ ] **Step 5 — Commit** — `git commit -m "fix: char-boundary-safe truncation helper (prevents panic→abort)"`

### Task 0.2: Route all truncation through `safe_truncate`

**Files:**
- Modify: `src/db.rs:355-361` (`insert_observation`)
- Modify: `src/provider.rs:59-72` (`build_prompt`)
- Test: `src/db.rs` add a regression test recording a multibyte output > `max_bytes`

- [ ] **Step 1 — Failing regression test** in `db.rs`: `insert_observation` with a 3000-byte multibyte string and `max_bytes=2048` returns `Ok` (today: panics).
- [ ] **Step 2 — Run, verify it fails (panics/aborts).**
- [ ] **Step 3 — Replace** the three ad-hoc `&x[..n]` slices with `crate::strutil::safe_truncate(x, n)`.
- [ ] **Step 4 — Run, verify pass** — `cargo test --lib db::`.
- [ ] **Step 5 — Commit** — `git commit -m "fix: route observation/prompt truncation through safe_truncate"`

### Task 0.3: Remove iCloud sync-conflict duplicates + guard

**Files:** delete `src/*\ 2.rs` (7 files), `docs/superpowers 2/`; modify `.gitignore`.

- [ ] **Step 1** — `git status` confirms all `* 2.*` are untracked.
- [ ] **Step 2** — Delete: ``rm src/*" 2.rs"`` and ``rm -rf "docs/superpowers 2"``.
- [ ] **Step 3** — Append to `.gitignore`: `*\ 2.*` and `* 2/` (block future sync dupes).
- [ ] **Step 4** — `cargo build` still succeeds (these were never `mod`-declared).
- [ ] **Step 5 — Commit** — `git commit -m "chore: drop iCloud sync-conflict duplicates + gitignore guard"`

### Task 0.4: Rebuild + reinstall so Claude runs current `main`

> Run when the editor's rust-analyzer is idle (it holds the cargo build-dir lock; that lock contention — not a code fault — is what wedged the verification build during planning).

- [ ] **Step 1** — `cargo build --release` (final-install build per build-speed preference).
- [ ] **Step 2** — `cp target/release/ironmem ~/.ironmem/bin/ironmem` (or re-run `install.sh`).
- [ ] **Step 3** — Verify: `~/.ironmem/bin/ironmem --version` and an MCP `initialize`+`tools/list` handshake returns the current tool set.
- [ ] **Step 4** — Restart Claude Desktop; confirm `mcp-server-ironmem.log` shows a clean init with the new binary mtime.
- [ ] **Step 5 — @superpowers:verification-before-completion** before claiming the MCP is fixed.

---

## Chunk 1: CCR core — content-addressed cache + zstd floor + retrieve

**Delivers a working, lossless `record_event` and a `retrieve_original` tool, using the universal zstd codec only. Per-type codecs come in Chunk 2.**

### Task 1.1: Add deps + `ContentType` + detection

**Files:** `Cargo.toml` (+`zstd = "0.13"`, promote `sha2` to direct dep); create `src/ccr/mod.rs`, `src/ccr/detect.rs`; `src/main.rs` (+`mod ccr;`).

- [ ] **Step 1 — Failing tests** in `detect.rs`: `detect(b"{\"a\":1}") == Json`; `detect(b"@@ -1 +1 @@\n-x\n+y")==Diff`; `detect(b"2026-06-07T00:00:00Z INFO foo\n...")==Log`; `detect(b"fn main(){}")` with hint `Some("rs")`==Code; random bytes==Binary; plain prose==Text.
- [ ] **Step 2 — Run, fail.**
- [ ] **Step 3 — Implement** `enum ContentType { Json, Code, Log, Diff, Text, Binary }` and `pub fn detect(bytes: &[u8], path_hint: Option<&str>) -> ContentType` (JSON = successful `serde_json` parse; Diff = leading `@@`/`+++ `/`--- ` / unified-diff line ratio; Log = ratio of lines matching a timestamp/level regex; Code = `path_hint` extension in a known set OR brace/semicolon density; Binary = NUL byte / invalid UTF‑8; else Text).
- [ ] **Step 4 — Pass.**
- [ ] **Step 5 — Commit** — `feat: ccr content-type detection + deps`.

### Task 1.2: `Codec` trait + universal zstd codec + round-trip property test

**Files:** `src/ccr/codec.rs`, `src/ccr/zstd_codec.rs`.

- [ ] **Step 1 — Failing tests:** `ZstdCodec` round-trips empty, ASCII, 1 MB random, and multibyte inputs byte-for-byte; `codec_for(ContentType)` returns a codec whose `id()` is stable.
- [ ] **Step 2 — Run, fail.**
- [ ] **Step 3 — Implement**
```rust
pub trait Codec: Send + Sync {
    fn id(&self) -> &'static str;
    fn compress(&self, input: &[u8]) -> anyhow::Result<Vec<u8>>;
    fn decompress(&self, input: &[u8]) -> anyhow::Result<Vec<u8>>;
}
pub struct ZstdCodec { pub level: i32 }
impl Codec for ZstdCodec {
    fn id(&self) -> &'static str { "zstd" }
    fn compress(&self, input: &[u8]) -> anyhow::Result<Vec<u8>> { Ok(zstd::encode_all(input, self.level)?) }
    fn decompress(&self, input: &[u8]) -> anyhow::Result<Vec<u8>> { Ok(zstd::decode_all(input)?) }
}
// Chunk 1: every ContentType maps to ZstdCodec. Chunk 2 swaps in per-type codecs.
pub fn codec_for(_ct: ContentType) -> Box<dyn Codec> { Box::new(ZstdCodec { level: 19 }) }
```
- [ ] **Step 4 — Pass.**
- [ ] **Step 5 — Commit** — `feat: ccr Codec trait + zstd floor codec`.

### Task 1.3: `blobs` table + accessors (both backends)

**Files:** `src/db.rs` (DDL in `migrate()` branched BLOB/BYTEA; `insert_blob`, `get_blob`, `incref`/`decref`).

- [ ] **Step 1 — Failing tests:** `insert_blob` is idempotent by hash (second insert bumps refcount, doesn't duplicate); `get_blob` returns stored row; `decref` to 0 leaves row for GC.
- [ ] **Step 2 — Fail.**
- [ ] **Step 3 — Implement** DDL + accessors following the existing `embeddings` BLOB/BYTEA branch pattern (`src/db.rs:185-207`).
- [ ] **Step 4 — Pass.**
- [ ] **Step 5 — Commit** — `feat: ccr blobs table + accessors`.

### Task 1.4: `store_blob` / `load_blob` public API (hash-verified)

**Files:** `src/ccr/mod.rs`.

- [ ] **Step 1 — Failing tests:** `store_blob(db, bytes, hint)` returns a `BlobRef{hash,content_type,...}`; storing identical bytes twice yields the same hash and a single row; `load_blob(db, &hash)` returns the exact original; a tampered `data` row makes `load_blob` **error** (hash mismatch).
- [ ] **Step 2 — Fail.**
- [ ] **Step 3 — Implement:** sha256 the original → detect type → `codec_for(ct).compress` → `insert_blob`; `load_blob` = `get_blob` → `decompress` → re-hash → assert equals key (else `bail!`).
- [ ] **Step 4 — Pass.**
- [ ] **Step 5 — Commit** — `feat: ccr store_blob/load_blob with hash verification`.

### Task 1.5: Lossless `record_event` (wire CCR into `insert_observation`)

**Files:** `src/db.rs` (`insert_observation` stores full output as a blob, keeps `safe_truncate` preview inline, writes `output_blob`); `src/mcp.rs:290` + `src/server.rs:156` unchanged signatures.

- [ ] **Step 1 — Failing test:** record a 50 KB output; assert inline `output` is the short preview AND `output_blob` resolves via `load_blob` to the full 50 KB original.
- [ ] **Step 2 — Fail.**
- [ ] **Step 3 — Implement:** when `output` exceeds a preview cap, `store_blob` the full bytes, set `output_blob=hash`, keep `safe_truncate` preview for FTS. Below the cap, skip the blob (no overhead).
- [ ] **Step 4 — Pass.**
- [ ] **Step 5 — Commit** — `feat: lossless record_event via CCR blob backing`.

### Task 1.6: `retrieve_original` MCP + REST tool

**Files:** `src/mcp.rs` (tool def in `build_tool_list` + `handle_retrieve_original` + dispatch arm); `src/server.rs` (`/retrieve_original`).

- [ ] **Step 1 — Failing test:** `tools/list` includes `retrieve_original`; calling it with `{observation_id}` returns the full original; with `{hash}` returns the blob; unknown id → graceful error result.
- [ ] **Step 2 — Fail.**
- [ ] **Step 3 — Implement** tool (accepts `observation_id` OR `hash` OR `memory_id`), description: "Retrieve the verbatim original behind a compressed memory/observation."
- [ ] **Step 4 — Pass + manual MCP handshake check.**
- [ ] **Step 5 — Commit** — `feat: retrieve_original tool (CCR retrieve surface)`.

### Task 1.7: Chunk-1 verification

- [ ] `cargo test` green; `cargo clippy -- -D warnings` clean; e2e: record large multibyte output → end session → `retrieve_original` returns exact bytes. @superpowers:verification-before-completion. Commit.

---

## Chunk 2: Per-content-type reversible codecs (the "complete" CCR)

**Every codec byte-exact; gated by the round-trip + hash-verify property test. Swaps `codec_for` from the zstd floor to real per-type codecs.**

### Task 2.1: Dictionary infrastructure (`dict.rs`)
- Load a per-type zstd dictionary from `assets/dicts/<type>.dict` if present; else **lazy-train** from the user's own accumulated blobs of that type once ≥ N samples exist, persist to `~/.ironmem/dicts/`. Tests: compress-with-dict round-trips byte-exact; absent dict falls back to the zstd floor (still reversible).

### Task 2.2: JSON codec (`json_codec.rs`)
- Byte-exact: zstd + JSON dictionary. Round-trip property test over a JSON corpus (objects, arrays, unicode, floats, nesting). Records `codec="json+zstd"`. (Optional: also persist a canonicalized view for search — never replaces the exact original.)

### Task 2.3: Log codec (`log_codec.rs`)
- **Invertible** line-template transform: extract recurring line prefixes (timestamp/level) into a per-blob table, replace with short tokens, then zstd. `decompress` reverses the substitution exactly. Property test asserts byte-exact on real multi-line logs. Records `codec="log+zstd"`.

### Task 2.4: Diff codec (`diff_codec.rs`)
- zstd + diff-token dictionary (`@@`, `+++ `, `--- `, hunk headers). Byte-exact round-trip test over `git diff` samples. `codec="diff+zstd"`.

### Task 2.5: Code codec (`code_codec.rs`)
- Byte-exact: per-language zstd dictionary keyed off the detected extension. Round-trip tests for rs/ts/py/go. `codec="code+zstd"`.
- **Documented boundary:** AST-normalization (whitespace/comment canonicalization) is byte-lossy and out of scope for v1; recorded in the module doc + this plan as a future research enhancement. No placeholder code is shipped.

### Task 2.6: Wire `codec_for` + ratio benchmark
- `codec_for(ct)` returns the matching codec. Add a `cargo test --release benchmarks` (ignored by default) reporting compression ratio per type vs the zstd floor, so gains are measured, not assumed. `log()` any type where the per-type codec underperforms the floor (then it falls back). Commit.

---

## Chunk 3: CCR over memories + GC + stats

### Task 3.1: Store the pre-LLM session blob
- In `compress::run`, before the LLM call, concatenate the full observation transcript, `store_blob` it, and write the hash to `memory_meta.session_blob`. The verbatim session behind any lossy summary becomes retrievable.

### Task 3.2: Extend `retrieve_original` for `memory_id`
- `retrieve_original({memory_id})` → resolves `memory_meta.session_blob` → full session transcript. Test.

### Task 3.3: Refcount GC
- `incref` on store / link, `decref` on `wipe_project` + session cleanup. New `ironmem gc` CLI command + `gc` internals that delete `refcount=0` blobs. Tests: wiping a project frees its blobs; shared (deduped) blobs survive until the last referrer is gone.

### Task 3.4: CCR stats in `get_status`
- Extend `get_status` to report: blob count, total original vs compressed bytes, dedup ratio, bytes saved. Test + commit.

---

## Chunk 4: Supermemory pattern — scoping + typed memories

### Task 4.1: `scope` + `kind` columns in `memory_meta`
- Additive `ALTER TABLE` (defaults `project`/`session`); accessors `set_memory_scope_kind`, `get_recent_memories_scoped`. Existing rows keep working. Tests for default backfill + round-trip.

### Task 4.2: `remember` MCP/REST tool
- `remember({scope, kind, text, tags})` writes an explicit typed memory (the Supermemory "single-API add memory" pattern) — inserts into `memories` + `memory_meta`, embeds inline like `compress::persist`. Tests: a `user`-scope `preference` memory is retrievable across projects.

### Task 4.3: Scope-aware injection
- Session-start injection (`rank_for_injection`) pulls **top project memories UNION top user memories**, with `kind` priors (e.g. boost `error_solution`/`preference`). Extend `blended_score` weights with an optional per-kind multiplier in `Weights`. Tests assert a user-scope preference is injected into a fresh project.

### Task 4.4: Compression classifies `kind`
- Add a `KIND:` line to the compression prompt (`provider.rs build_prompt`/`parse_response`) with the typed enum; default `session`. Tests for parse + clamp to the known set.

---

## Chunk 5: User profile extraction

### Task 5.1: `profile.rs` — regenerate profile
- From user-scoped memories, produce one `kind=profile, scope=user` memory = **stable facts + recent activity** (Supermemory's profile model; mirrors how Claude Code's `MEMORY.md` separates durable user facts from project facts). Regenerate on a threshold (every K user-memory writes) — single LLM call, or deterministic rollup when no API key/credits (degrade gracefully, never block).

### Task 5.2: Always-inject the profile
- Session-start injection prepends the current profile memory (cheap, single row). Tests: profile present → injected first; absent → no change (legacy order).

### Task 5.3: `get_profile` / `refresh_profile` tools
- Read + force-regenerate surfaces. Tests + commit.

---

## Chunk 6 (stretch): Feedback loop — correction miner

> Optional. Build only after 0–5 are shipped and validated.

### Task 6.1: Mine corrective sessions
- Detect sessions containing error→fix signals (failed command followed by a passing retry, or explicit user correction) and extract a `kind=error_solution` memory. Tests over synthetic transcripts.

### Task 6.2: Surface corrections
- Inject relevant `error_solution` memories on matching project/query; optionally append a digest to `IRONMEM.md` (the Headroom "writes corrections to CLAUDE.md" idea, kept local + reversible). Tests + commit.

---

## Sequencing & handoff

```
Chunk 0  (stabilize + ship)           ── do first, independently shippable
   ├── CCR track:    Chunk 1 → 2 → 3
   └── Memory track: Chunk 4 → 5 → 6   (independent of CCR; parallelizable)
```

- **Execution:** subagent-driven-development — one fresh subagent per Task, two-stage review per task, CCR and Memory tracks dispatched in parallel after Chunk 0.
- **Per-task discipline:** TDD (red→green→commit), `cargo clippy -- -D warnings`, both-backend parity for any DDL, and the reversibility property test as the gate for every codec.
- **Verification:** @superpowers:verification-before-completion at each Chunk boundary; never claim "working" without a real handshake/round-trip.

## Risks & mitigations
- **zstd build weight / static link** → vendored libzstd via `zstd` crate, matches the existing all-static posture (`libsqlite3-sys` bundled); verify release binary size delta in Chunk 1.
- **Postgres BYTEA parity** → mirror the `embeddings` branch; both-backend tests in Task 1.3.
- **iCloud sync on a live DB** → out of scope for code, but flag to user: move the repo/DB off `~/Desktop` iCloud or exclude it; the `* 2` duplicates prove sync is active.
- **No API key / low credits** (seen in `server.log`) → every LLM-dependent step (compression, kind classification, profile) degrades to a deterministic local fallback and never blocks CCR or storage.
- **Honest scope** → code codec ships byte-exact dictionary zstd; AST normalization is documented-future, not a stub.
