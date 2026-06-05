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

- `insert_memory` now reads the new rowid on the same pooled connection as the INSERT, fixing a latent bug where a 5-connection pool could return a wrong/zero id (previously harmless when the id was only logged; now load-bearing for embeddings + metadata).

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
