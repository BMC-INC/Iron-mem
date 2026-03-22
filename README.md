<p align="center">
  <img src="memorylogo.png" alt="IronMem" width="120"/>
  <br/>
  <img src="assets/title.png" alt="IRON-MEM" width="300"/>
</p>

<p align="center">
  <strong>Persistent memory for AI coding assistants.</strong>
</p>

<p align="center">
  Stop re-explaining your project every time you start a new session.
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
  <img src="https://img.shields.io/badge/MCP-Native-6c5ce7?style=for-the-badge" alt="MCP Native"/>
  <img src="https://img.shields.io/badge/SQLite%20%2B%20Postgres-003B57?style=for-the-badge&logo=sqlite&logoColor=white" alt="SQLite + Postgres"/>
  <img src="https://img.shields.io/badge/Docker%20Ready-2496ED?style=for-the-badge&logo=docker&logoColor=white" alt="Docker Ready"/>
  <img src="https://img.shields.io/badge/license-Apache--2.0-brightgreen?style=for-the-badge" alt="License"/>
  <img src="https://img.shields.io/github/stars/BMC-INC/Iron-mem?style=for-the-badge&color=yellow" alt="Stars"/>
  <img src="https://img.shields.io/github/issues/BMC-INC/Iron-mem?style=for-the-badge" alt="Issues"/>
  <a href="https://github.com/BMC-INC/Iron-mem/actions/workflows/rust.yml"><img src="https://github.com/BMC-INC/Iron-mem/actions/workflows/rust.yml/badge.svg" alt="CI"/></a>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/works_with-Claude_Desktop-D97706?style=flat-square&logo=anthropic&logoColor=white" alt="Claude Desktop"/>
  <img src="https://img.shields.io/badge/works_with-Claude_Code-D97706?style=flat-square&logo=anthropic&logoColor=white" alt="Claude Code"/>
  <img src="https://img.shields.io/badge/works_with-Cursor-000000?style=flat-square&logo=cursor&logoColor=white" alt="Cursor"/>
  <img src="https://img.shields.io/badge/works_with-ChatGPT_Desktop-10a37f?style=flat-square&logo=openai&logoColor=white" alt="ChatGPT Desktop"/>
  <img src="https://img.shields.io/badge/works_with-Copilot-2b3137?style=flat-square&logo=github&logoColor=white" alt="Copilot"/>
  <img src="https://img.shields.io/badge/works_with-Windsurf-06B6D4?style=flat-square" alt="Windsurf"/>
  <img src="https://img.shields.io/badge/works_with-Zed-000000?style=flat-square" alt="Zed"/>
</p>

---

<!-- SEO Keywords: AI coding assistant memory, session-aware AI tools, Rust AI tools, context preservation, Claude Code memory, Cursor context -->

## What's New in v0.3.0

> Now multi-provider, cross-platform, with a built-in web UI.

- **Multi-provider compression** — use OpenAI, Google Gemini, or Anthropic as your LLM. Set `"provider": "openai"` in settings.
- **Neovim plugin** — native Lua plugin with auto session lifecycle, `:IronMemSearch`, `:IronMemStatus`
- **Windows support** — `install.ps1`, platform-aware messages, robust home directory detection
- **Web UI** — browse, search, and delete memories at `http://localhost:37778/ui`
- **Still zero telemetry. Still local-first. Your data stays yours.**

<details>
<summary>v0.2.0</summary>

- **10 MCP tools** — session_start, session_end, record_event, compress_session, get_context, get_status, list_memories, search_memories, inject_context, wipe_project
- **Dual database** — SQLite (local, FTS5 full-text search) + Postgres (self-hosted, tsvector) via `DATABASE_URL`
- **Every MCP client** — Claude Desktop, Claude Code, Cursor, Windsurf, ChatGPT Desktop, Zed, and more
- **Docker deployment** — `docker-compose up` for remote/team setups with Postgres
- **`ironmem mcp`** — new subcommand for direct MCP stdio transport (Claude Desktop/Code)
- **REST server still works** — existing hooks and curl-based workflows unaffected
</details>

---

IronMem gives AI coding tools persistent memory across sessions.
It silently records what happened during your session, compresses it into concise memory, and injects that context into your next session automatically.

No copy-pasting.
No rebuilding context from scratch.
No "remember when we refactored auth yesterday?"

**Works with every major AI coding tool** — Claude Code, Claude Desktop, Cursor, Windsurf, ChatGPT Desktop, GitHub Copilot, Zed, VS Code, and any MCP-compatible client.

**Compress with the LLM you already pay for** — Anthropic Claude, OpenAI GPT-4o, or Google Gemini. Switch providers with one config change.

**Free and open source.** Runs locally or on your own infrastructure. No telemetry. No cloud dependency. No subscription. SQLite or Postgres storage. Plain markdown output. Single Rust binary.

<p align="center">
  <img src="assets/demo.gif" alt="IronMem in action" width="640"/>
</p>

## Why this exists

AI coding tools are great inside a session and terrible across sessions.
They help you ship faster, but every fresh session forgets your architecture decisions, debugging trail, and what changed yesterday.

IronMem fixes the handoff.

## Before vs after

Without IronMem:

> "We already changed the auth middleware, switched to JWT, updated the migration, and fixed the failing tests in billing. Let me explain the whole thing again."

With IronMem:

> Open a new session. Your assistant already has the recent project context.

---

## Quick Start

1. **Install IronMem**:
   ```bash
   curl -fsSL https://raw.githubusercontent.com/BMC-INC/Iron-mem/main/install.sh | bash
   ```
2. **Add your API key** (in your `~/.zshrc` or `~/.bashrc`):
   ```bash
   export ANTHROPIC_API_KEY="your-key-here"
   ```
3. **Start coding!** IronMem handles the rest silently in the background.

---

## Table of Contents

- [Quick Start](#quick-start)
- [The Problem](#the-problem)
- [The Fix](#the-fix)
- [Who Should Use This?](#who-should-use-this)
- [How It Works](#how-it-works)
- [Install](#install)
- [CLI](#cli)
- [Multi-Provider Support](#multi-provider-support)
- [MCP Setup](#mcp-setup)
- [Web UI](#web-ui)
- [Configuration](#configuration)
- [Troubleshooting](#troubleshooting)
- [Architecture](#architecture)
- [Why Rust?](#why-rust)
- [Design Principles](#design-principles)
- [Why not just use CLAUDE.md?](#why-not-just-use-claudemd)
- [Roadmap](#roadmap)
- [Contributing](#contributing)
- [Support](#-support)
- [License](#license)

---

## The Problem

Every time you start a new session with Claude Code, Cursor, Copilot, or any AI coding assistant — it starts from zero. It doesn't know what you built yesterday. It doesn't know what broke. It doesn't know what you decided.

**You end up re-explaining context every single session.**

## The Fix

IronMem silently records what happens during your coding session, compresses it into a concise memory using your LLM provider of choice (Anthropic, OpenAI, or Gemini), and injects that context into your next session automatically.

No setup per session. No copy-pasting. No "remember when we..."

<p align="center">
  <img src="assets/demo.gif" alt="IronMem in action" width="640"/>
</p>

> **Without IronMem:** _"Hey Claude, remember yesterday we refactored the auth middleware and switched to JWT? And the database migration for the users table? And..."_
>
> **With IronMem:** You open a new session. It already knows.

---

## Who Should Use This?

IronMem is designed for:
- **Developers frustrated with re-explaining context** to AI tools every single session.
- **Teams working on large, multi-session projects** where context gets easily lost.
- **Developers frequently switching** between multiple AI tools like Copilot, Claude Code, Windsurf, or Cursor.
- **Solo developers** who want to maintain flow and continuity without manual effort.

---

## How It Works

```mermaid
flowchart LR
    A["🟢 Session Start"] -->|inject memories| B["📄 IRONMEM.md"]
    B --> C["🤖 AI reads context"]
    C --> D["🔧 You code"]
    D -->|every tool call| E["🗄️ SQLite"]
    D --> F["🔴 Session End"]
    F -->|compress via LLM| G["🧠 Memory"]
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

**Windows:**

```powershell
git clone https://github.com/BMC-INC/Iron-mem.git
cd Iron-mem
powershell -ExecutionPolicy Bypass -File install.ps1
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
ironmem server              # Start REST + MCP SSE server
ironmem mcp                 # Start MCP stdio server (for Claude Desktop/Code)
ironmem serve               # Start SSE server with bearer token auth
ironmem serve --public      # Same + Cloudflare Tunnel for claude.ai
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

IronMem works as an **MCP server** (native integration) or via **IRONMEM.md** (plain markdown, universal):

| Platform | MCP Native | IRONMEM.md | Setup |
| -------- | :--------: | :--------: | ----- |
| **Claude Code** | **Yes** | Yes | [Setup →](#claude-code-mcp-setup) |
| **Claude Desktop / claude.ai** | **Yes** | Yes | [Setup →](#claude-desktop-mcp-setup) |
| **Cursor** | **Yes** | Yes | [Setup →](#cursor--windsurf-mcp-setup) |
| **Windsurf** | **Yes** | Yes | [Setup →](#cursor--windsurf-mcp-setup) |
| **ChatGPT Desktop** | **Yes** | — | [Setup →](#other-mcp-clients) |
| **Zed** | **Yes** | — | [Setup →](#other-mcp-clients) |
| **VS Code (Copilot/Continue/Cline)** | **Yes** | Yes | [Setup →](#other-mcp-clients) |
| **Any MCP Client** | **Yes** | — | stdio or SSE transport |
| **Any AI Tool** | — | Yes | Read `IRONMEM.md` as project context |

---

## MCP Setup

IronMem supports two MCP transports:

- **stdio** — for local clients that launch the server themselves (Claude Code, Claude Desktop, Cursor)
- **Streamable HTTP** — for remote/cloud clients that connect over HTTP (claude.ai, team deployments). Uses standard request/response — works through any tunnel or proxy.

### Claude Code MCP Setup

Claude Code connects via **stdio** — it launches `ironmem mcp` directly.

**Option A: CLI (recommended)**

```bash
claude mcp add ironmem -- ~/.ironmem/bin/ironmem mcp
```

**Option B: Project `.mcp.json`** (share with your team)

Create `.mcp.json` in your project root:

```json
{
  "mcpServers": {
    "ironmem": {
      "command": "~/.ironmem/bin/ironmem",
      "args": ["mcp"],
      "env": {
        "ANTHROPIC_API_KEY": "your-key-here"
      }
    }
  }
}
```

> **Note:** Claude Code hooks (installed by `install.sh`) and MCP can coexist. The hooks use the REST API for automatic observation recording; MCP gives you direct tool access. You can use both, or just one.

### Claude Desktop MCP Setup

Claude Desktop also connects via **stdio**.

Add to your `claude_desktop_config.json`:

**macOS:** `~/Library/Application Support/Claude/claude_desktop_config.json`
**Windows:** `%APPDATA%\Claude\claude_desktop_config.json`

```json
{
  "mcpServers": {
    "ironmem": {
      "command": "/Users/YOU/.ironmem/bin/ironmem",
      "args": ["mcp"],
      "env": {
        "ANTHROPIC_API_KEY": "your-key-here"
      }
    }
  }
}
```

Replace `/Users/YOU` with your actual home directory path. Restart Claude Desktop after saving.

### claude.ai (Web) MCP Setup

claude.ai runs in the cloud — it **cannot** reach `localhost`. IronMem solves this with one command:

```bash
ironmem serve --public
```

This does three things:
1. Starts the SSE server with **bearer token authentication**
2. Launches a **Cloudflare Tunnel** (free, no account needed) to expose it publicly
3. Prints the public URL and auth token

```
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
  IronMem SSE Server
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
  Local:  http://127.0.0.1:37779/mcp
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
  Public URL: https://xxx-yyy-zzz.trycloudflare.com
  Add to claude.ai as MCP server:
    URL:   https://xxx-yyy-zzz.trycloudflare.com/mcp
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
```

Add the URL to claude.ai under Settings → Integrations → Add MCP Server.

**Everything stays local.** Your data never leaves your machine — the tunnel just forwards authenticated MCP protocol messages.

**Without `--public`:** `ironmem serve` starts the SSE server locally with auth (for LAN or your own reverse proxy).

**Requirements:** Install [cloudflared](https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/downloads/) for best results (`brew install cloudflared` on macOS, `winget install Cloudflare.cloudflared` on Windows). Falls back to `npx cloudflared` if not installed.

### Cursor / Windsurf MCP Setup

Both use stdio. Add to your MCP settings:

**Cursor:** Settings → MCP → Add Server

**Windsurf:** Settings → MCP → Add Server

```json
{
  "ironmem": {
    "command": "~/.ironmem/bin/ironmem",
    "args": ["mcp"]
  }
}
```

### Other MCP Clients

Any MCP client that supports **stdio** transport can use IronMem:

```json
{
  "command": "~/.ironmem/bin/ironmem",
  "args": ["mcp"]
}
```

For clients that support **Streamable HTTP**, start the server and point the client at `http://localhost:37779/mcp`:

```bash
ironmem serve
```

### Neovim Plugin

IronMem includes a native Neovim plugin that communicates via MCP stdio.

**Install with lazy.nvim:**

```lua
{
  "BMC-INC/Iron-mem",
  config = function()
    require("ironmem").setup({
      -- binary = "~/.ironmem/bin/ironmem",  -- default
      -- auto_start = true,   -- session_start on VimEnter
      -- auto_end = true,     -- session_end on VimLeavePre
      -- record_events = true, -- record buffer writes
    })
  end,
}
```

**Commands:**

| Command | Description |
|---------|-------------|
| `:IronMemStart` | Manually start a session |
| `:IronMemEnd` | End session and compress |
| `:IronMemStatus` | Show database stats |
| `:IronMemSearch <query>` | Search memories in a split buffer |

---

## Web UI

When the REST server is running, a built-in memory browser is available at:

```
http://localhost:37778/ui
```

The UI shows sessions, memories, and database stats. You can browse, search, and delete memories directly from the browser.

---

## Configuration

`~/.ironmem/settings.json`:

```json
{
  "port": 37778,
  "provider": "anthropic",
  "model": "claude-sonnet-4-6-20250627",
  "inject_limit": 5,
  "max_observation_bytes": 2048,
  "db_path": "/Users/you/.ironmem/mem.db",
  "database_url": null,
  "mcp_transport": "stdio",
  "mcp_sse_port": 37779,
  "auth_token": null
}
```

All fields optional. Sensible defaults provided.

### Provider

IronMem supports three LLM providers for session compression:

| Provider | `provider` value | Default model | API key env var |
|----------|-----------------|---------------|-----------------|
| **Anthropic** | `"anthropic"` | `claude-sonnet-4-6-20250627` | `ANTHROPIC_API_KEY` |
| **OpenAI** | `"openai"` | `gpt-4o` | `OPENAI_API_KEY` |
| **Google Gemini** | `"google"` | `gemini-2.0-flash` | `GOOGLE_API_KEY` |

To switch providers, set `"provider"` in `~/.ironmem/settings.json` and ensure the corresponding API key is set. The `model` field overrides the provider's default model.

### Environment Variables

| Variable | Default | Description |
|:---------|:--------|:------------|
| `DATABASE_URL` | _(none)_ | Postgres URL. Overrides `db_path` when set. |
| `IRONMEM_MCP_TRANSPORT` | `stdio` | MCP transport: `stdio` or `sse` |
| `ANTHROPIC_API_KEY` | _(none)_ | Required when provider is `anthropic` (default) |
| `OPENAI_API_KEY` | _(none)_ | Required when provider is `openai` |
| `GOOGLE_API_KEY` | _(none)_ | Required when provider is `google` |

### API Key

IronMem needs an LLM API key to compress session observations into memories. Set the environment variable for your chosen provider (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, or `GOOGLE_API_KEY`).

For the default Anthropic provider, IronMem also checks `~/.ironmem/api_key` as a fallback. This file is auto-created by the session-start hook because the IronMem server runs as a background process via `nohup`, and some environments strip environment variables from child processes. The hook persists your API key with `chmod 600` permissions so the server can always access it.

---

## Troubleshooting

**Server not starting:**

```bash
ironmem status                           # Check if server responds
cat ~/.ironmem/server.log                # Check server logs
~/.ironmem/bin/ironmem server            # Run manually to see errors
```

**Observations not being recorded:**

```bash
ironmem status                           # Check observation count
sqlite3 ~/.ironmem/mem.db "SELECT count(*) FROM observations;"
```

If count stays at 0, your hooks may not be installed. Re-run `./install.sh` or check that `~/.claude/hooks/post-tool-use.sh` exists and is executable.

**Compression failing (memories always 0):**

```bash
# Check if the API key is accessible
cat ~/.ironmem/api_key                   # Should contain your key
echo $ANTHROPIC_API_KEY                  # Should be set

# Try manual compression
ironmem compress <session-id>            # Get session ID from server.log
```

**Hooks not firing:**
Check that `~/.claude/settings.json` has the hooks registered under the `"hooks"` key. Re-running `./install.sh` will fix this.

---

## Architecture

```text
~/.ironmem/
├── bin/ironmem          # Single compiled binary
├── mem.db               # SQLite database (FTS5 full-text search)
├── settings.json        # Configuration
├── api_key              # Anthropic API key (auto-persisted, chmod 600)
├── current_session      # Active session ID (ephemeral)
└── server.log           # Worker logs

~/.claude/hooks/         # Auto-installed Claude Code hooks
├── session-start.sh     # Injects memories on session start
├── post-tool-use.sh     # Records every tool call
├── stop.sh              # Triggers compression
└── session-end.sh       # Cleanup
```

**~2,400 lines of Rust.** MCP-native. SQLite or Postgres. One binary. No external runtimes.

---

## Why Rust?

Rust was chosen for IronMem to deliver:
- **Maximum Performance:** Minimal overhead and lightning-fast execution, essential for a tool that hooks into every single CLI command.
- **Zero Dependencies:** Compiles down to a single binary. No need to install Python, Node.js, or complex runtime environments.
- **Memory Safety & Reliability:** Guaranteed safety without a garbage collector ensures the background worker remains rock-solid and leak-free.

---

## Design Principles

- **Zero friction** — hooks run silently, never interrupt your workflow
- **Local-first** — runs on your machine by default, your data stays yours
- **MCP-native** — speaks the protocol every major AI client is adopting
- **Provider-agnostic** — MCP for native integration, plain markdown for everything else
- **Self-hostable** — Docker + Postgres for team deployments, still zero cloud dependencies
- **Fail-safe** — if IronMem crashes, your coding session is unaffected

---

## Who this is for

IronMem is for developers who use AI coding tools heavily and want continuity across sessions.

It is especially useful if you:
- switch between Claude Code, Cursor, Copilot, or Windsurf
- work on projects that span many sessions
- are tired of re-explaining architecture, bugs, and recent changes
- want local-first memory instead of a hosted service

## Who this is not for

IronMem is not trying to be:
- a generic memory platform for every kind of agent
- a cloud sync product
- a team knowledge base
- a dashboard-heavy workflow tool

It solves one narrow problem well: session memory for AI coding workflows.

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

## Docker Deployment

Run IronMem with Postgres for team/remote setups:

```bash
ANTHROPIC_API_KEY=your-key docker-compose up --build
```

This starts IronMem with Streamable HTTP on `http://localhost:37779/mcp` and Postgres 16, plus the REST server on `http://localhost:37778`.

---

## Roadmap

### Shipped in v0.2.0

- [x] MCP-native server (stdio + Streamable HTTP)
- [x] Dual database — SQLite (local, FTS5) + Postgres (self-hosted)
- [x] Docker deployment with Postgres
- [x] Bearer token authentication
- [x] `ironmem serve --public` with Cloudflare Tunnel for claude.ai
- [x] Works with Claude Code, Claude Desktop, Cursor, Windsurf, ChatGPT Desktop, Zed, VS Code

### Shipped in v0.3.0

- [x] Multi-provider compression — OpenAI, Google Gemini, or Anthropic (configurable via `provider` in settings)
- [x] Neovim plugin (`nvim/lua/ironmem/`) — auto session lifecycle, `:IronMemSearch`, `:IronMemStatus`
- [x] Windows native support — `install.ps1`, platform-aware install messages, robust home dir detection
- [x] Web UI memory browser — `http://localhost:37778/ui` when REST server is running

---

## Contributing

Contributions are welcome. Please read [CONTRIBUTING.md](CONTRIBUTING.md) before opening a PR.

**TL;DR:** Open an issue first. Bug fixes and provider compatibility improvements are always welcome. We don't accept changes that add runtime dependencies or complexity.

---

## ⭐ Support

If you find IronMem useful, please consider giving it a star! 🌟
This helps others discover the project and motivates further development.
Contributions, issues, and feature requests are also highly welcome.

---

## License

Apache-2.0 © 2026 ExecLayer Inc

**Maintainer:** [James Benton](https://github.com/BMC-INC)
