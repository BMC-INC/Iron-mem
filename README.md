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
  <img src="https://img.shields.io/badge/built_with-Rust-F74C00?style=for-the-badge&logo=rust&logoColor=white" alt="Built with Rust"/>
  <img src="https://img.shields.io/badge/storage-SQLite-003B57?style=for-the-badge&logo=sqlite&logoColor=white" alt="SQLite"/>
  <img src="https://img.shields.io/badge/license-Apache--2.0-brightgreen?style=for-the-badge" alt="License"/>
  <img src="https://img.shields.io/github/stars/BMC-INC/Ironmem?style=for-the-badge&color=yellow" alt="Stars"/>
  <a href="https://github.com/BMC-INC/Iron-mem/actions/workflows/rust.yml"><img src="https://github.com/BMC-INC/Iron-mem/actions/workflows/rust.yml/badge.svg" alt="CI"/></a>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/works_with-Claude_Code-D97706?style=flat-square&logo=anthropic&logoColor=white" alt="Claude Code"/>
  <img src="https://img.shields.io/badge/works_with-Cursor-000000?style=flat-square&logo=cursor&logoColor=white" alt="Cursor"/>
  <img src="https://img.shields.io/badge/works_with-Copilot-2b3137?style=flat-square&logo=github&logoColor=white" alt="Copilot"/>
  <img src="https://img.shields.io/badge/works_with-Windsurf-06B6D4?style=flat-square" alt="Windsurf"/>
</p>

---

## The Problem

Every time you start a new session with Claude Code, Cursor, Copilot, or any AI coding assistant — it starts from zero. It doesn't know what you built yesterday. It doesn't know what broke. It doesn't know what you decided.

**You end up re-explaining context every single session.**

## The Fix

IronMem silently records what happens during your coding session, compresses it into a concise memory using Claude's API, and injects that context into your next session automatically.

No setup per session. No copy-pasting. No "remember when we..."

<p align="center">
  <img src="assets/demo.gif" alt="IronMem in action" width="640"/>
</p>

> **Without IronMem:** _"Hey Claude, remember yesterday we refactored the auth middleware and switched to JWT? And the database migration for the users table? And..."_
>
> **With IronMem:** You open a new session. It already knows.

---

## How It Works

```mermaid
flowchart LR
    A["🟢 Session Start"] -->|inject memories| B["📄 IRONMEM.md"]
    B --> C["🤖 AI reads context"]
    C --> D["🔧 You code"]
    D -->|every tool call| E["🗄️ SQLite"]
    D --> F["🔴 Session End"]
    F -->|compress via Claude API| G["🧠 Memory"]
    G -->|next session| A

    style A fill:#22c55e,color:#fff,stroke:none
    style F fill:#ef4444,color:#fff,stroke:none
    style G fill:#8b5cf6,color:#fff,stroke:none
    style E fill:#0ea5e9,color:#fff,stroke:none
```

Everything runs locally. Your data stays on your machine.

---

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/BMC-INC/Iron-mem/main/install.sh | bash
```

Or clone and build manually:

```bash
git clone https://github.com/BMC-INC/Iron-mem.git
cd Iron-mem
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
| ---- | ----- |
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

```text
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

## Why not just use CLAUDE.md?

`CLAUDE.md` is great for static project context — things like "use tabs not spaces" or "we use Axum for routing." But it's manual. You write it, you maintain it, and it doesn't know what happened last session.

IronMem is **automatic and session-aware:**

|   | CLAUDE.md | IronMem |
| - | --------- | ------- |
| **Updates** | You write it manually | Auto-generated from session activity |
| **Scope** | Static project rules | Dynamic session history |
| **Rotation** | You manage it | Old memories age out automatically |
| **Search** | Ctrl+F | Full-text search across all sessions |
| **Effort** | High | Zero — hooks handle everything |

They work together. `CLAUDE.md` holds your project rules. IronMem holds what happened.

---

## Roadmap

- [ ] Windows native support
- [ ] Neovim plugin
- [ ] VSCode extension
- [ ] OpenAI / Gemini provider support (for compression)
- [ ] Web UI for memory browser

---

## Contributing

Contributions are welcome. Please read [CONTRIBUTING.md](CONTRIBUTING.md) before opening a PR.

**TL;DR:** Open an issue first. Bug fixes and provider compatibility improvements are always welcome. We don't accept changes that add runtime dependencies or complexity.

---

## License

Apache-2.0
