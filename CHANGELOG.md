# Changelog

All notable changes to IronMem will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added — Semantic Foundation

- **Hybrid retrieval** — keyword (FTS) and semantic (vector) search are fused with Reciprocal Rank Fusion. `search`, `search-global`, and `get_context` now return semantically relevant memories that pure keyword search would miss. Pass `semantic: false` to force keyword-only.
- **Local-first embeddings** — pluggable `embedding.provider`: `auto` (default, prefers local Ollama → in-process ONNX → keyword-only), `ollama`, `onnx` (build with `--features local-onnx`), `openai`, `google`, or `none`. **No data egress** unless you explicitly select an API provider. A missing/unreachable embedder never hard-fails a command — search degrades to keyword-only.
- **Real ANN indexing** — embeddings are stored and queried via [`sqlite-vec`](https://github.com/asg017/sqlite-vec) (cosine) on SQLite and pgvector on Postgres, with an exact brute-force fallback.
- **Relevance-ranked injection** — session-start injection ranks memories by a blend of **relevance** (semantic match to your current git context), **recency** (true half-life decay), and **importance** (an LLM-assigned 1–10 score per memory). Weights and half-life are configurable under `embedding.weights` / `embedding.recency_half_life_days`.
- **`ironmem embed`** — backfill command to embed pre-existing memories (`--project`, `--all`, `--force`); idempotent and batched.

### Changed

- Session compression now emits an `IMPORTANCE` score and embeds each new memory inline (best-effort). The three compression entry points (MCP, REST, CLI) were unified behind a single `compress::run` so behavior can't drift.

### Fixed

- **MCP stdio transport corrupted by log output** — `tracing` wrote to stdout, but in `ironmem mcp` stdout *is* the JSON-RPC stream, so a startup log line (e.g. `Embedder: none`) made clients reject the connection with `Unexpected token … is not valid JSON`. Logs now go to stderr with ANSI disabled.
- **Crash on multibyte tool output** — observation and prompt truncation sliced strings on raw byte offsets; a multibyte character on the cap boundary panicked, and under the release profile's `panic = "abort"` that took down the entire MCP process mid-session. All truncation now backs up to a UTF-8 char boundary via `strutil::safe_truncate`.
- `insert_memory` now reads the new rowid on the same pooled connection as the INSERT, fixing a latent bug where a 5-connection pool could return a wrong/zero id (previously harmless when the id was only logged; now load-bearing for embeddings + metadata).

### Security

- Patched 7 Dependabot advisories: **rmcp** 1.2 → 1.7 (GHSA-89vp-x53w-74fx, high), **rustls-webpki** 0.103.10 → 0.103.13 (GHSA-82j2-j2ch-gfr8 high, plus GHSA-965h-392x-2mh5 / GHSA-xgp8-3hg3-c2mh), and **rand** → 0.8.6 / 0.9.3 / 0.10.1 (GHSA-cq8v-f236-94qc).

### Added — Lossless Reversible Memory (CCR)

- **Content-addressed blob store** — a new `blobs` table inside the existing SQLite/Postgres DB stores tool outputs and the verbatim pre-LLM session transcript whole, deduplicated by the sha256 of the original bytes, and compressed by **byte-exact reversible codecs**. `load_blob` re-hashes on read and fails loudly on any mismatch, so the original is recoverable exactly or not at all.
- **Per-content-type compression** — content type is detected (json / code / log / diff / text / binary) and compressed with a zstd floor plus lazily-trained, content-addressed per-type **dictionaries** (`ccr_dicts` + `blobs.dict_hash`), so dictionaries can retrain without ever breaking an existing blob's round-trip. The dictionary is kept only when it beats the floor (never worse than the floor).
- **Lossless `record_event`** — when a tool output exceeds the inline preview cap it is preserved whole in the blob store; the inline `output` keeps only a UTF-8-safe preview for search. The full session transcript behind each compressed memory is likewise preserved.
- **`retrieve_original`** — new MCP tool + REST `POST /retrieve_original` to pull back the verbatim original behind any compressed memory, by `observation_id`, `memory_id`, or blob `hash`.
- **Refcount GC + stats** — `ironmem gc` reclaims unreferenced blobs; `get_status` now reports CCR storage stats (blob count, original vs. stored bytes, compression %, dedup factor, bytes saved).

### Added — Memory Model: Scoping, Types, Profile & Corrections

- **Scope + kind** — every memory carries a **scope** (`project` or `user`/cross-project) and a **kind** (`session`, `error_solution`, `preference`, `architecture`, `learned_pattern`, `project_config`, `profile`), stored as additive columns on `memory_meta` with constant defaults so existing rows keep working unchanged. Session-start injection ranks **project ∪ user** memories and applies a per-kind multiplier (configurable via `embedding.weights.kind_boosts`) that lifts durable kinds.
- **`remember`** — new MCP tool + REST `POST /remember` + `ironmem remember` CLI to store an explicit, typed memory (`scope`, `kind`, `text`, `tags`) in one call. User-scope facts surface in every project.
- **User profile** — cross-project (`scope=user`) memories are distilled into a single, always-injected `kind=profile` memory: an LLM summary when a provider is reachable, otherwise a deterministic local rollup (never blocks, never egresses). It auto-refreshes as user memories accumulate. Read/regenerate via `get_profile` / `refresh_profile` (MCP), `GET /profile` / `POST /refresh_profile` (REST), or `ironmem profile [--refresh]` (CLI).
- **Compression classifies kind** — session compression now emits a `KIND:` line that is clamped to the known set (default `session`) and recorded on the memory.
- **Correction miner** — compression scans the session for error→fix loops (a failing command, intervening edits, then the same command passing) and records each as a project-scoped `error_solution` memory; surfaced via `list_corrections` (MCP), `GET /corrections` (REST), and `ironmem corrections` (CLI).

### Changed (cont.)

- **Quick Start now recommends the `~/.ironmem/api_key` file over `export ANTHROPIC_API_KEY`.** Tools that read `ANTHROPIC_API_KEY` from the environment — including Claude Code — will bill against it (pay-as-you-go) instead of your subscription whenever it's set, so the key file keeps IronMem's key out of your shell environment.

### Fixed (cont.)

- **Flaky `mcp_stdio_clean` test on slow CI** — the stdout-is-pure-JSON regression test used a fixed sleep before checking for the `initialize` response, which raced on slow Windows runners. It now waits for the response by content (up to a generous deadline), eliminating the flake without weakening the assertion.

## [0.1.0] - 2026-03-20

### Added

- **Session memory capture** — automatically records tool calls and coding activity via Claude Code hooks (`session-start`, `post-tool-use`, `stop`, `session-end`)
- **AI-powered compression** — summarizes raw session logs into concise memory entries using the Claude API
- **IRONMEM.md injection** — generates a markdown file with recent memories that AI coding assistants read as project context
- **SQLite storage with FTS5** — full-text search across all session memories
- **CLI interface** — `ironmem server`, `status`, `list`, `search`, `inject`, `compress`, `wipe`, `config`
- **Multi-provider support** — output is plain markdown compatible with Claude Code, Cursor, Windsurf, GitHub Copilot, and any tool that reads project context files
- **One-line installer** — `curl | bash` install script that builds from source, installs the binary, and registers Claude Code hooks
- **Local-first architecture** — all data stays on your machine, server binds to `127.0.0.1` only
- **Zero-dependency runtime** — single compiled Rust binary, no external runtimes required

### Known Limitations

- Linux and macOS only (Windows support is planned)
- Requires Rust/Cargo for installation (pre-built binaries coming soon)
- Compression uses the Claude API, which requires an `ANTHROPIC_API_KEY`
- Memory rotation/aging is basic — oldest memories are dropped when the inject limit is reached
