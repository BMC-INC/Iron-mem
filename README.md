<p align="center">
  <img src="memorylogo.png" alt="IronMem" width="120"/>
  <br/>
  <img src="assets/title.png" alt="IRON-MEM" width="300"/>
</p>

<p align="center">
  <strong>Your AI coding assistant forgets everything. IronMem fixes that.</strong>
</p>

<p align="center">
  <a href="#install">Install</a> &bull;
  <a href="#how-it-works">How It Works</a> &bull;
  <a href="#cli">CLI</a> &bull;
  <a href="#multi-provider-support">Multi-Provider</a> &bull;
  <a href="#contributing">Contributing</a>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/built_with-Rust-dea584?style=flat-square&logo=rust" alt="Built with Rust"/>
  <img src="https://img.shields.io/badge/storage-SQLite-003B57?style=flat-square&logo=sqlite" alt="SQLite"/>
  <img src="https://img.shields.io/badge/license-Apache--2.0-blue?style=flat-square" alt="License"/>
  <img src="https://img.shields.io/github/stars/BMC-INC/Ironmem?style=flat-square" alt="Stars"/>
</p>

---

## The Problem

Every time you start a new session with Claude Code, Cursor, Copilot, or any AI coding assistant — it starts from zero. It doesn't know what you built yesterday. It doesn't know what broke. It doesn't know what you decided.

**You end up re-explaining context every single session.**

## The Fix

IronMem silently records what happens during your coding session, compresses it into a concise memory using Claude's API, and injects that context into your next session automatically.

No setup per session. No copy-pasting. No "remember when we..."

<p align="center">
  <img src="assets/demo-hero.png" alt="IronMem CLI Demo" width="600"/>
</p>

---

## How It Works

```
┌─────────────────────────────────────────────────────────┐
│                    YOUR CODING SESSION                   │
├─────────────────────────────────────────────────────────┤
│                                                         │
│  SessionStart                                           │
│  └─→ Injects previous memories into IRONMEM.md          │
│      └─→ AI assistant reads it automatically            │
│                                                         │
│  Every Tool Call (file edits, searches, etc.)            │
│  └─→ Recorded to SQLite via local HTTP server           │
│                                                         │
│  SessionEnd                                             │
│  └─→ All observations compressed via Claude API         │
│      └─→ Stored as a 3-5 sentence memory with tags     │
│                                                         │
│  Next Session                                           │
│  └─→ Loop repeats. Context never lost.                  │
│                                                         │
└─────────────────────────────────────────────────────────┘
```

Everything runs locally. Your data stays on your machine.

---

## Install

```bash
git clone https://github.com/BMC-INC/Ironmem.git
cd Ironmem
chmod +x install.sh
./install.sh
```

Add to your shell profile (`~/.zshrc` or `~/.bashrc`):

```bash
export PATH="$HOME/.ironmem/bin:$PATH"
export ANTHROPIC_API_KEY="your-key-here"
```

Restart your terminal and Claude Code. That's it.

**Requirements:** Rust/Cargo (the installer will tell you if it's missing)

---

## CLI

```bash
ironmem server              # Start the background worker (auto-started by hooks)
ironmem status              # Health check + DB stats
ironmem list                # Recent memories for current project
ironmem search "auth middleware"  # Full-text search across memories
ironmem inject              # Manually rebuild IRONMEM.md
ironmem compress <id>       # Manually compress a session
ironmem wipe                # Delete all memories for current project
ironmem config              # Print current settings
```

<p align="center">
  <img src="assets/demo-list.png" alt="ironmem list" width="600"/>
</p>
<p align="center">
  <img src="assets/demo-search.png" alt="ironmem search" width="600"/>
</p>

---

## Multi-Provider Support

`IRONMEM.md` is plain markdown. It works everywhere:

| Tool | Setup |
|------|-------|
| **Claude Code** | Automatic — hooks handle everything |
| **Cursor** | Add `@IRONMEM.md` to `.cursorrules` |
| **Windsurf** | Add to `.windsurfrules` |
| **GitHub Copilot** | Add to `.github/copilot-instructions.md` |
| **Any other** | Read `IRONMEM.md` as project context |

---

## Configuration

`~/.ironmem/settings.json`:

```json
{
  "port": 37778,
  "model": "claude-sonnet-4-6-20250627",
  "inject_limit": 5,
  "max_observation_bytes": 2048,
  "db_path": "/Users/you/.ironmem/mem.db"
}
```

All fields optional. Sensible defaults provided.

---

## Architecture

```
~/.ironmem/
├── bin/ironmem          # Single compiled binary
├── mem.db               # SQLite database (FTS5 full-text search)
├── settings.json        # Configuration
├── current_session      # Active session ID (ephemeral)
└── server.log           # Worker logs

~/.claude/hooks/         # Auto-installed Claude Code hooks
├── session-start.sh     # Injects memories on session start
├── post-tool-use.sh     # Records every tool call
├── stop.sh              # Triggers compression
└── session-end.sh       # Cleanup
```

**~1,200 lines of Rust.** No external runtimes. No background daemons you forget about. SQLite for storage. One binary.

---

## Design Principles

- **Zero friction** — hooks run silently, never interrupt your workflow
- **Local-first** — all data on your machine, server binds to `127.0.0.1` only
- **Provider-agnostic** — plain markdown output works with any AI tool
- **No bloat** — no Bun, no Python, no Docker, no cloud accounts
- **Fail-safe** — if IronMem crashes, your coding session is unaffected

---

## Contributing

Contributions are welcome. Please read [CONTRIBUTING.md](CONTRIBUTING.md) before opening a PR.

**TL;DR:** Open an issue first. Bug fixes and provider compatibility improvements are always welcome. We don't accept changes that add runtime dependencies or complexity.

---

## License

Apache-2.0
