# Changelog

All notable changes to IronMem will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
