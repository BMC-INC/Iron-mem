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

## What's New in v0.4.0

> IronMem is now a full durable memory stack: reversible originals, typed memories, temporal graph recall, procedural memory, adaptive skim/expand context, and 21 MCP tools.

- **CCR — losslessly reversible memory** (Headroom pattern) — every truncated tool output and the verbatim pre-LLM session transcript is preserved in a content-addressed, deduplicated, byte-exact compressed blob store inside the DB. **`retrieve_original`** pulls the exact original back by `observation_id`, `memory_id`, raw blob `hash`, or the new `chunk_id` expansion handle.
- **Memory scoping & typed memories** (Supermemory patterns) — memories carry a **scope** (`project` vs. `user`/cross-project) and a **kind** (`session`, `fact`, `error_solution`, `preference`, `procedural`, `architecture`, `learned_pattern`, `project_config`, `profile`). Session-start injection ranks **project ∪ user** memories and boosts durable kinds.
- **Dual-output compression** — session compression writes both a narrative memory and separate searchable `kind=fact` memories, so dates, names, quantities, and direct answers survive summarization.
- **Temporal recall + graph recall** — dated facts and `event_time` metadata power timestamp lookup, while `memory_edges` stores structured `source | relation | target` edges with valid-time filters and provenance. Temporal questions route toward date-bearing facts; relationship questions route toward graph edges.
- **Adaptive working-memory skim** — every compressed or explicit memory gets durable `memory_chunks` with density (`high`, `medium`, `low`), kind, title, token estimate, and optional exact transcript offsets. Agents can skim broadly with **`memory_skim`**, then expand exact evidence with **`retrieve_original(chunk_id=...)`**.
- **Governed recall** — memories now carry a durable governance envelope: namespace, source type, trust tier, writer/source provenance, classification, consent state, residency, retention metadata, legal hold, tombstone state, record hash, and append-only ledger hash chain. Reads are namespace-scoped and active-only by default; PHI/PII writes fail closed unless consent is granted.
- **Closed-loop memory quality** — injection events and explicit feedback now reinforce useful memories and decay repeatedly ignored or corrected memories, reducing stale context without deleting provenance.
- **AST-bound Rust anchors** — `ironmem code-relink` uses Tree-sitter to hash Rust symbols and relink memories when code moves across files.
- **Reflection, snapshots, and sync** — dry-run-first consolidation proposals, CCR-backed project brain snapshots, and an idempotent Lamport-clock sync event log support long-lived and multi-agent memory workflows.
- **`remember` tool** — store an explicit, typed memory in one call (`scope`, `kind`, `text`, `tags`). User-scope facts follow you into every project and also enter the skim layer.
- **User profile** — cross-project memories are distilled into a single always-injected profile (LLM summary, or deterministic local rollup when offline). Read/regenerate with **`get_profile`** / **`refresh_profile`**.
- **Correction miner** — error→fix loops in a session (a failing command, edits, then the same command passing) are mined into `error_solution` memories and surfaced via **`list_corrections`**, so past fixes resurface when the work recurs.
- **21 MCP tools** now — including `memory_skim`, `retrieve_original`, `remember`, `get_profile`, `refresh_profile`, `list_corrections`, `memory_graph`, and `reconcile_memory_graph`.
- **Current verification:** `cargo test --bin ironmem` passes **160 tests** with **1 ignored benchmark**, `cargo test --test mcp_stdio_clean` passes, and `cargo clippy --bin ironmem -- -D warnings` is clean.
- **Still zero telemetry. Still local-first. Your data stays yours.**

<details>
<summary>v0.3.0</summary>

- **Multi-provider compression** — use OpenAI, Google Gemini, or Anthropic as your LLM. Set `"provider": "openai"` in settings.
- **Neovim plugin** — native Lua plugin with auto session lifecycle, `:IronMemSearch`, `:IronMemStatus`
- **Windows support** — `install.ps1`, platform-aware messages, robust home directory detection
- **Web UI** — browse, search, and delete memories at `http://localhost:37778/ui`
- **Discovery tools** — list known projects, search across all projects, and inspect per-project session history
- **Still zero telemetry. Still local-first. Your data stays yours.**
</details>

<details>
<summary>v0.2.0</summary>

- **13 MCP tools** — session_start, session_end, record_event, compress_session, get_context, get_status, list_memories, search_memories, search_global, list_projects, list_sessions, inject_context, wipe_project
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
2. **Add your API key** to IronMem's key file:
   ```bash
   echo "your-key-here" > ~/.ironmem/api_key && chmod 600 ~/.ironmem/api_key
   ```
   > **Prefer the key file over `export ANTHROPIC_API_KEY`.** Claude Code (and some other tools) bill against `ANTHROPIC_API_KEY` whenever it's set in your shell — using pay-as-you-go API credit instead of your Claude subscription. The key file keeps IronMem's key out of your environment so it can't change how other tools bill. (IronMem still honors `ANTHROPIC_API_KEY` if you prefer the env var.)
3. **Start coding!** IronMem handles the rest silently in the background.

---

## Table of Contents

- [Quick Start](#quick-start)
- [The Problem](#the-problem)
- [The Fix](#the-fix)
- [Who Should Use This?](#who-should-use-this)
- [How It Works](#how-it-works)
- [Current Memory Stack](#current-memory-stack)
- [Install](#install)
- [CLI](#cli)
- [Multi-Provider Support](#multi-provider-support)
- [MCP Setup](#mcp-setup)
- [MCP Tools](#mcp-tools)
- [Web UI](#web-ui)
- [Governed Memory](#governed-memory)
- [Configuration](#configuration)
- [Testing Status](#testing-status)
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

## Current Memory Stack

IronMem now stores memory in several cooperating layers rather than one flat summary:

| Layer | What it stores | Why it matters |
| ----- | -------------- | -------------- |
| **Session transcript CCR** | Verbatim pre-LLM session transcripts and large/truncated tool outputs in content-addressed compressed blobs | Exact originals are recoverable; summaries are never the only copy |
| **Narrative memories** | Concise session summaries in the `memories` FTS table | Fast project history and ordinary session recall |
| **Typed facts** | Separate `kind=fact` memories extracted from compression | Direct answers, dates, names, quantities, and benchmark-style lookup survive summarization |
| **Procedural memories** | Reusable workflow rules as `kind=procedural` | Future agents can recall “how we work” without mixing procedures into narrative memory |
| **Error solutions** | Mined fail→edit→pass loops as `kind=error_solution` | Past fixes come back when the same failure pattern appears |
| **User profile** | Global `scope=user`, `kind=profile` memory | Stable cross-project facts are always injected |
| **Temporal graph** | `source | relation | target` edges with valid-time fields, confidence, and memory provenance | Relationship questions and Operator OS entity state can use graph traversal instead of only vector similarity |
| **Adaptive skim chunks** | `memory_chunks` rows with `chunk_id`, density, kind, title, summary, token estimate, and optional exact transcript byte offsets | Agents can scan broad compressed history, then expand exact evidence on demand |

Retrieval is routed by query shape:

- **Temporal lookup queries** (`when`, `what date`, `which year`, `before`, `after`, etc.) prioritize date-bearing `kind=fact` memories and suppress graph-only hits that would otherwise promote relationship memories over timestamp answers.
- **Relationship queries** keep graph fusion enabled and rank edges by relation/source/target overlap.
- **General project recall** blends FTS, vectors when available, event-time boosts, graph signals, kind boosts, importance, and recency.
- **Skim/expand workflows** use `memory_skim` or `/skim` first, then `retrieve_original` with a `chunk_id` for exact transcript evidence.

This is intentionally model-agnostic. The durable store is hard-token, structured, and auditable, so Claude, Codex, Operator OS, desktop clients, and remote MCP clients can share the same backing memory.

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

Add IronMem to your `PATH` (in `~/.zshrc` or `~/.bashrc`) and write your API key to IronMem's key file:

```bash
export PATH="$HOME/.ironmem/bin:$PATH"          # in ~/.zshrc / ~/.bashrc
echo "your-key-here" > ~/.ironmem/api_key && chmod 600 ~/.ironmem/api_key
```

> Use the key file, not `export ANTHROPIC_API_KEY` — a global `ANTHROPIC_API_KEY` makes Claude Code (and similar tools) bill against pay-as-you-go API credit instead of your subscription. IronMem reads `~/.ironmem/api_key` automatically.

Restart your terminal and Claude Code. That's it.

**Requirements:** Rust/Cargo (the installer will tell you if it's missing)

---

## CLI

```bash
ironmem server              # Start REST + MCP SSE server
ironmem mcp                 # Start MCP stdio server (for Claude Desktop/Code)
ironmem serve               # Start SSE server with bearer token auth
ironmem serve --public      # Same + Cloudflare Tunnel for remote MCP clients
ironmem serve --public --no-auth  # Authless public tunnel for claude.ai personal use
ironmem status              # Health check + DB stats
ironmem projects            # All projects with stored memories
ironmem list                # Recent memories for current project
ironmem search "auth middleware"  # Hybrid (keyword + semantic) search across memories
ironmem search-global "auth middleware"  # Search across all projects
ironmem sessions            # Session history for current project
ironmem inject              # Manually rebuild IRONMEM.md (relevance-ranked)
ironmem remember "..."      # Store an explicit memory (--scope user, --kind preference, --tags)
ironmem remember "..." --classification pii --consent-state granted --namespace tenant-a
ironmem forget <memory-id> --reason "user requested erasure"
ironmem profile             # Show the user profile (--refresh to regenerate it)
ironmem corrections         # List mined error→fix memories (--all for every project)
ironmem graph "Operator OS" # Query temporal graph edges (--history includes superseded edges, --at filters valid time)
ironmem graph-delete <edge-id> # Mark a bad graph edge user_deleted
ironmem graph-update <edge-id> --source A --relation owns --target B # Human-curate a graph edge
ironmem reconcile --dry-run # Preview duplicate/current-state graph reconciliation
ironmem graph-backfill --limit 50 # Extract graph relations from older memories
ironmem feedback <memory-id> --signal used --weight 1 # Reinforce or decay a memory
ironmem reflect --dry-run # Propose durable-memory consolidation
ironmem code-relink --dry-run # Tree-sitter Rust AST anchoring/relinking
ironmem snapshot create --label before-refactor # CCR-backed project brain snapshot
ironmem sync publish --node ci --op error_solution --payload '{"memory_id":1}' # Multi-agent event log
ironmem eval                # Run deterministic memory-quality evals into docs/evals
ironmem compress <id>       # Manually compress a session
ironmem embed               # Backfill semantic embeddings for existing memories
ironmem gc                  # Reclaim unreferenced CCR blobs (after wipes)
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
| **Claude Desktop** | **Yes** | Yes | [Setup →](#claude-desktop-mcp-setup) |
| **claude.ai** | **Yes** | Yes | [Setup →](#claudeai-web) |
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
- **Streamable HTTP** — for remote/cloud clients that connect over HTTP. Uses standard request/response and bearer-token auth, so it works through tunnels and reverse proxies for clients that support static bearer tokens.

Once connected over MCP, clients can record sessions, retrieve memories, inspect graph state, scan adaptive skims, and expand exact originals directly.

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

### claude.ai (Web)

claude.ai runs in the cloud, so it **cannot** reach `localhost`.

IronMem is a local-first tool. The recommended setup for full MCP access is **Claude Code** or **Claude Desktop** using stdio.

Anthropic's current `claude.ai` custom connector UI supports **authless** and **OAuth-based** remote MCP servers, but not a manual static bearer-token field. For personal use, the honest compatibility path is an **authless ephemeral tunnel**:

```bash
ironmem serve --public --no-auth
```

That command does three things:
1. Starts the SSE server with **no auth**
2. Launches a **Cloudflare Tunnel** (free, no account needed) to expose it publicly
3. Prints the public URL

```
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
  IronMem SSE Server
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
  Local:  http://127.0.0.1:37779/mcp
  Auth:   Disabled (--no-auth)
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
  Public URL: https://xxx-yyy-zzz.trycloudflare.com
  Remote MCP setup:
    URL:   https://xxx-yyy-zzz.trycloudflare.com/mcp
    Auth:  None
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
```

In claude.ai:

1. Open `Settings`
2. Open `Integrations`
3. Choose `Add custom connector`
4. Set `Name` to `IronMem`
5. Paste the printed `https://...trycloudflare.com/mcp` URL into `Remote MCP server URL`
6. Leave the OAuth fields blank

The `trycloudflare.com` URL is ephemeral and changes whenever you restart the public tunnel, so update your connector URL each time you relaunch `ironmem serve --public --no-auth`.

**This is no longer local-only.** The tunnel exposes your MCP endpoint over the internet for as long as it is running.

For a personal local tool, this tradeoff is often acceptable because the URL is short-lived and changes on each restart. Still, use `--no-auth` deliberately and only when you understand that you are trading security for compatibility.

**Without `--no-auth`:** `ironmem serve` and `ironmem serve --public` use bearer-token auth for clients that support static bearer tokens.

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

## MCP Tools

IronMem currently exposes **21 MCP tools**:

| Tool | Purpose |
| ---- | ------- |
| `session_start` | Start a new project session and return a `session_id` |
| `record_event` | Record a tool call observation |
| `session_end` | End a session and trigger compression |
| `compress_session` | Manually compress a session |
| `get_context` | Retrieve project memories; results include expansion chunks with `chunk_id` handles |
| `memory_skim` | Return project or global compressed skim chunks for broad working-memory scan |
| `retrieve_original` | Expand exact original text by `chunk_id`, `observation_id`, `memory_id`, or blob `hash` |
| `get_status` | Return DB stats, CCR stats, graph edge count, and memory chunk count |
| `list_memories` | List recent memories for a project |
| `search_memories` | Hybrid search inside one project |
| `search_global` | Hybrid search across every project |
| `list_projects` | List known projects with stored memories |
| `list_sessions` | List session history for a project |
| `inject_context` | Write `IRONMEM.md` into a project root |
| `remember` | Store an explicit typed/scoped memory |
| `get_profile` | Return the current cross-project user profile |
| `refresh_profile` | Regenerate the user profile |
| `list_corrections` | List mined `error_solution` memories |
| `memory_graph` | Query temporal graph edges for an entity, with optional valid-time filtering |
| `reconcile_memory_graph` | Dry-run or apply duplicate/current-state graph reconciliation |
| `wipe_project` | Delete all memories for one project |

The intended agent loop is:

1. Call `get_context` for focused recall or `memory_skim` for a broader scan.
2. Inspect returned `chunk_id` values.
3. Call `retrieve_original` with the chosen `chunk_id` when exact evidence is needed.

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

### REST API

The REST server runs on `http://localhost:37778` by default. Current high-signal endpoints:

| Endpoint | Purpose |
| -------- | ------- |
| `POST /session/start` | Start a session |
| `POST /event` | Record an observation |
| `POST /session/end` | End and compress a session |
| `POST /compress` | Manually compress a session |
| `GET /context?project=&query=&namespace=&limit=&rerank=` | Retrieve project memories plus expansion chunks in one governance namespace |
| `GET /skim?project=&namespace=&limit=` | Return adaptive project skim chunks in one governance namespace |
| `GET /skim?global=true&namespace=&limit=` | Return adaptive global skim chunks in one governance namespace |
| `POST /retrieve_original` | Expand by `chunk_id`, `observation_id`, `memory_id`, or `hash` |
| `POST /remember` | Store an explicit typed/scoped memory |
| `GET /profile` / `POST /refresh_profile` | Read or regenerate the user profile |
| `GET /corrections` | List mined error-solution memories |
| `GET /graph?entity=&project=&history=&at=&limit=` | Query temporal graph edges |
| `GET /status` | Health, DB stats, CCR stats, graph edge count, and memory chunk count |

---

## Governed Memory

IronMem is still independent of SovereignClaw, but it now has its own governance envelope for durable recall. The default namespace is `local`, so existing single-user installs continue to work without new flags. Multi-tenant or control-plane callers can set `namespace` to isolate reads, search, skim, list, context injection, and explicit memory writes.

Governance fields accepted by CLI, REST `/remember`, and MCP `remember` include:

| Field | Purpose |
| ----- | ------- |
| `namespace` | Tenant/realm boundary. Defaults to `local`. |
| `source_type` | `user_input`, `tool_output`, `agent_generated`, `derived`, `external`, or `sync_peer`. |
| `trust_tier` | `high`, `medium`, `low`, or `untrusted`. |
| `writer_identity` / `source_ref` | Provenance for who/what wrote the memory. |
| `parent_memory_id` | Lineage for derived facts and compression children. |
| `classification` | `public`, `internal`, `confidential`, `restricted`, `pii`, or `phi`. |
| `consent_state` | `required`, `granted`, `denied`, or `withdrawn`; `pii` and `phi` require `granted`. |
| `residency`, `retention_policy_id`, `expires_at` | Policy metadata; `expires_at` is enforced by active recall filters. |
| `legal_hold` | Prevents governed deletion while true. |

Every governed write stores a canonical record hash and appends to `memory_ledger`, linking each entry to the previous namespace hash. Deletion uses `ironmem forget` or the existing project wipe paths, which now call governed deletion per memory: legal holds block deletion, active recall is tombstoned first, the ledger records the forget event, vectors are purged, and CCR blobs are garbage-collected when no references remain.

Example:

```bash
ironmem remember "Customer asked to retain audit exports for 7 years" \
  --namespace tenant-a \
  --classification confidential \
  --consent-state granted \
  --writer ops-agent \
  --residency us \
  --retention-policy-id audit-7y

ironmem search "audit exports" --namespace tenant-a
ironmem forget 42 --actor privacy-admin --reason "retention window expired"
```

---

## Configuration

`~/.ironmem/settings.json`:

```json
{
  "port": 37778,
  "provider": "anthropic",
  "model": "claude-sonnet-4-6",
  "inject_limit": 5,
  "max_observation_bytes": 2048,
  "db_path": "/Users/you/.ironmem/mem.db",
  "database_url": null,
  "mcp_transport": "stdio",
  "mcp_sse_port": 37779,
  "auth_token": null,
  "embedding": {
    "provider": "auto",
    "model": null,
    "ollama_url": "http://localhost:11434",
    "weights": {
      "relevance": 0.5,
      "recency": 0.3,
      "importance": 0.2,
      "kind_boosts": {}
    },
    "recency_half_life_days": 30
  },
  "rerank": {
    "enabled": false,
    "model": "",
    "pool": 20
  }
}
```

All fields optional. Sensible defaults provided. `auth_token` is generated automatically the first time you run `ironmem serve` without `--no-auth`. The `embedding` block is optional — omit it entirely and IronMem behaves exactly as before (keyword-only search, recency injection). The `rerank` block is off by default because it makes one LLM call per reranked query.

### Semantic Search & Embeddings

IronMem can blend **keyword (FTS)** and **semantic (vector)** retrieval using [Reciprocal Rank Fusion](https://en.wikipedia.org/wiki/Reciprocal_rank_fusion), and rank session-start injection by a blend of **relevance + recency + importance**. Embeddings are stored locally in SQLite via [`sqlite-vec`](https://github.com/asg017/sqlite-vec) (or pgvector on Postgres); nothing is sent anywhere unless you explicitly choose an API provider.

**Privacy posture:** this is a governance tool, so the default is **local-first / no data egress**. The `auto` provider prefers a local embedder and silently degrades to keyword-only search if none is available — it never phones home and never hard-fails a command.

| `embedding.provider` | Behavior | Data egress |
|----------------------|----------|-------------|
| `"auto"` *(default)* | Use local Ollama if reachable, else the built-in ONNX model (if compiled in), else keyword-only | **None** (unless only an API key is configured) |
| `"ollama"` | Local [Ollama](https://ollama.com) embeddings | **None** (localhost) |
| `"onnx"` | In-process ONNX model (requires `--features local-onnx` build) | **None** |
| `"openai"` | OpenAI embeddings API | Sends memory text to OpenAI |
| `"google"` | Google embeddings API | Sends memory text to Google |
| `"none"` | Keyword-only FTS + recency injection (legacy behavior) | **None** |

**Recommended local setup (no egress):**

```bash
# Install Ollama, then pull an embedding model:
ollama pull nomic-embed-text
# IronMem's "auto" provider will detect and use it automatically.
```

**Built-in ONNX (no Ollama, no network):** compile with the optional feature so embeddings run fully in-process:

```bash
cargo install --path . --features local-onnx
```

**Blend weights** (`embedding.weights`) control session-start injection ranking: `relevance` (semantic match to your current git context), `recency` (true half-life decay set by `recency_half_life_days`), and `importance` (an LLM-assigned 1–10 score per memory). They need not sum to 1.

**Reranking** (`rerank.enabled`) is optional and disabled by default. When enabled, IronMem pulls a wider candidate pool, asks the configured LLM to rerank compact snippets, then re-anchors results so strong base temporal answers are not lost. Per-request REST callers can override with `?rerank=true` or `?rerank=false`.

**Backfill existing memories** — after enabling embeddings, index memories created before:

```bash
ironmem embed              # embed memories missing a vector (all projects)
ironmem embed --project .  # scope to one project
ironmem embed --force      # rebuild the whole index from scratch
```

### Provider

IronMem supports three LLM providers for session compression:

| Provider | `provider` value | Default model | API key env var |
|----------|-----------------|---------------|-----------------|
| **Anthropic** | `"anthropic"` | `claude-sonnet-4-6` | `ANTHROPIC_API_KEY` |
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

IronMem needs an LLM API key to compress session observations into memories.

**Recommended (Anthropic):** write the key to `~/.ironmem/api_key` (`echo "your-key" > ~/.ironmem/api_key && chmod 600 ~/.ironmem/api_key`). Keeping it in this file rather than a global `ANTHROPIC_API_KEY` export avoids changing how other tools bill — Claude Code, for instance, charges pay-as-you-go API credit whenever `ANTHROPIC_API_KEY` is set in the environment, instead of using your subscription. IronMem reads the file automatically (it also works when the background server is spawned via `nohup`, where some shells strip env vars from child processes).

IronMem still honors the per-provider environment variable if you prefer it — `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, or `GOOGLE_API_KEY`. The file fallback applies to the default Anthropic provider.

---

## Testing Status

Current local verification for this README state:

```bash
CARGO_TARGET_DIR=/tmp/ironmem-codex-target cargo test --bin ironmem -- --nocapture
CARGO_TARGET_DIR=/tmp/ironmem-codex-target cargo clippy --bin ironmem -- -D warnings
```

Result:

- **157 tests passed**
- **1 benchmark intentionally ignored** (`bench_ccr_dict_vs_floor`)
- **0 failed**
- **Clippy clean with `-D warnings`**

Coverage includes CCR round trips and corruption checks, UTF-8-safe truncation, typed/scoped memories, user profile regeneration, correction mining, semantic retrieval, temporal lookup routing, graph reconciliation, chunk skim/expand flows, MCP auth/tools, REST-facing behavior through shared handlers, vector backfill/purge, provider parsing, and the end-to-end semantic pipeline.

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
# Check if the API key is accessible (the key file is the recommended source)
cat ~/.ironmem/api_key                   # Should contain your key
echo $ANTHROPIC_API_KEY                  # Optional env-var fallback (may be empty)

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
├── mem.db               # SQLite DB: FTS memories, metadata, vectors, graph edges, chunks, CCR blobs
├── settings.json        # Configuration
├── api_key              # Anthropic API key (chmod 600; keeps it out of your shell env)
├── current_session      # Active session ID (ephemeral)
└── server.log           # Worker logs

~/.claude/hooks/         # Auto-installed Claude Code hooks
├── session-start.sh     # Injects memories on session start
├── post-tool-use.sh     # Records every tool call
├── stop.sh              # Triggers compression
└── session-end.sh       # Cleanup
```

**~14,000 lines of Rust.** MCP-native. SQLite or Postgres. Lossless, reversible memory. Temporal graph. Adaptive skim/expand chunks. One binary. No external runtimes.

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
- [x] `ironmem serve --public` with Cloudflare Tunnel for remote MCP clients
- [x] `ironmem serve --public --no-auth` for claude.ai personal use
- [x] Works with Claude Code, Claude Desktop, Cursor, Windsurf, ChatGPT Desktop, Zed, VS Code

### Shipped in v0.3.0

- [x] Multi-provider compression — OpenAI, Google Gemini, or Anthropic (configurable via `provider` in settings)
- [x] Neovim plugin (`nvim/lua/ironmem/`) — auto session lifecycle, `:IronMemSearch`, `:IronMemStatus`
- [x] Windows native support — `install.ps1`, platform-aware install messages, robust home dir detection
- [x] Web UI memory browser — `http://localhost:37778/ui` when REST server is running

### Shipped in v0.4.0

- [x] **Semantic foundation** — hybrid FTS + vector + temporal graph (RRF) retrieval, local-first embeddings, and relevance-ranked session-start injection
- [x] **Reliability & security hardening** — stdio MCP stream is no longer corrupted by log output, UTF-8-safe truncation prevents a crash on multibyte tool output, and 7 dependency advisories were patched
- [x] **CCR — losslessly reversible memory** (Headroom pattern): a content-addressed, deduplicated blob store + byte-exact per-content-type compression (zstd + per-type dictionaries) + a `retrieve_original` tool, so the verbatim original behind any compressed memory is always recoverable — no more lossy truncation. Refcount GC (`ironmem gc`) + storage stats in `get_status`. [Design »](docs/superpowers/plans/2026-06-07-ironmem-ccr-supermemory.md)
- [x] **Memory scoping & types** (Supermemory patterns): project vs. user (cross-project) scope, typed memories (`error_solution` / `preference` / `procedural` / `architecture` / `learned_pattern` / …), scope-aware injection with per-kind boosts, and a `remember` tool
- [x] **Dual-output compression** — every compressed session can persist a narrative memory plus separate `kind=fact` memories; facts inherit event-time metadata when available and are indexed for direct retrieval.
- [x] **Always-injected user profile** — cross-project facts distilled into one profile memory (LLM summary or deterministic local rollup); `get_profile` / `refresh_profile`
- [x] **Correction miner** — error→fix loops become `error_solution` memories, surfaced via `list_corrections`
- [x] **Temporal graph lite** — compression now extracts structured `source | relation | target` edges with dates, confidence, memory provenance, and reconciliation. Exact duplicates and superseded current-state edges are marked in history rather than deleted. Query with `ironmem graph`, REST `/graph`, or MCP `memory_graph`; active edges also feed hybrid search as a relation-ranked retrieval signal.
- [x] **Graph operations** — `ironmem reconcile` / MCP `reconcile_memory_graph` repair legacy duplicate/current-state edges with dry-run counts, and `ironmem graph-backfill` extracts graph relations for older memories without mutating summaries.
- [x] **Temporal and procedural recall** — graph queries support valid-time filters (`--at`, `at_time`, REST `at`), compression validates dates, reusable workflow rules are extracted/stored as `kind=procedural`, and temporal lookup queries route toward date-bearing facts instead of graph-only relationship hits.
- [x] **Adaptive working-memory skim** — `memory_chunks` store model-agnostic compressed chunk maps with density, kind, title, token estimate, source offsets, and `chunk_id` expansion handles. Use MCP `memory_skim`, REST `/skim`, or `get_context` expansions, then expand exact evidence with `retrieve_original`.
- [x] **Operator OS adapter + eval harness** — `docs/operator-os-memory-adapter.md` defines tenant/worker/work-item memory mapping, and `ironmem eval` writes repeatable graph/temporal/procedural eval reports with command, model, and commit metadata.
- [x] **Closed-loop quality + curation** — usage feedback and injection events adjust ranking; the Web UI can inspect graph edges and mark hallucinated links as user-deleted.
- [x] **AST-bound memory + reflection + time travel + sync** — Tree-sitter Rust code anchors, dry-run/apply reflection proposals, CCR-backed snapshots, and an idempotent sync event log are implemented.
- [x] **Current verification** — 160 Rust tests pass, 1 benchmark is intentionally ignored, standalone MCP stdio cleanliness passes, and clippy is clean with `-D warnings`.

### Next

- [ ] **Bespoke per-content-type transforms** — invertible log timestamp-delta / diff-token / AST-aware code normalization on top of the dictionary codecs (currently documented-future; the byte-exact contract is the gate)
- [ ] **Observation-blob lifecycle GC** — reclaim CCR blobs behind deleted observations (memory-session blobs are already GC'd)

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
