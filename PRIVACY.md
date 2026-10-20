# Privacy

IronMem is a local memory store for coding assistants. This note describes what the current source stores, where it lives, and which actions send data off the machine. It is not a hosted-service privacy policy.

## What is stored

By default, files live under `.ironmem` in the home directory (`dirs::home_dir()`):

- `mem.db`: SQLite database of sessions, observations, memories, embeddings, the governance ledger, and local access counters (recall and expansion counts). Those counters are not sent anywhere.
- `settings.json`: configuration, including an optional `auth_token` for the HTTP MCP listener.
- `api_key`: only if you create this file. It is read for Anthropic when cloud enrichment needs a key.
- Upgrade backups created by the installer when an existing install changes.
- `server.log`: written when the Claude Code hook launcher starts `ironmem server` and redirects its output. The binary itself does not open this file.
- `fastembed_cache`: only after a binary built with the `local-onnx` feature loads the ONNX embedder. The first load downloads model weights. Memory text is not part of that download.

Project files:

- `IRONMEM.md` in a project directory when context injection writes it. `wipe_project` tries to delete that file.

If `database_url` or `DATABASE_URL` is set, the database is wherever that URL points (for example a Postgres server you operate). That server may be on another machine.

Optional `storage.vector_backend` and `storage.graph_backend` settings can point at Qdrant or Neo4j. The default backend is `native` (inside the local database). If you set a remote URL, vectors or graph rows are stored on that server.

## What stays on the machine by default

- Compression mode defaults to `local`. Session text is not sent to an LLM provider.
- The embedding provider defaults to `auto`. Auto uses a local Ollama server if one answers, then an in-process ONNX model if this binary was built with `local-onnx`. Otherwise it uses keyword search.
- Auto does not call OpenAI or Google unless you opt in (see below). An API key in the environment is not enough.
- `ironmem server` and `ironmem serve` listen on `127.0.0.1` unless you change the SSE bind.
- This repository does not contain a product-analytics client. The word "telemetry" in code and benchmark docs means local counters and in-process timings, not a remote collector. Outbound HTTP call sites in `src/` are provider APIs, embedding APIs, optional Qdrant or Neo4j, local status checks, and the Cloudflare tunnel helper.

## When data leaves the machine

- Cloud enrichment. Setting `compression.mode` to `cloud_with_local_fallback` sends session text to the configured provider (Anthropic, OpenAI, Google Gemini, or Vertex AI).
- Explicit embedding providers `openai` and `google` send memory text to that vendor's embeddings API. IronMem logs a warning when it selects one of these.
- Opt-in auto fallback. If no local embedder is available, `embedding.allow_cloud_fallback` is true or `IRONMEM_EMBED_ALLOW_CLOUD=1`, and `OPENAI_API_KEY` is set, memory text is sent to OpenAI. The default is off. Google is not an automatic fallback. Set `embedding.provider` to `google` to use Google embeddings.
- `ironmem serve --public` starts a Cloudflare tunnel. The MCP endpoint is reachable on the internet while the tunnel runs. `--no-auth` removes the bearer token.
- A non-loopback `--mcp-sse-bind` or `IRONMEM_MCP_SSE_BIND` exposes the SSE listener on that interface.
- A remote `DATABASE_URL`, Qdrant URL, or Neo4j URL sends the corresponding data to that server.
- An ONNX model download, only for `local-onnx` builds, the first time the weights are not already in `fastembed_cache`.

API keys are read from provider environment variables. `~/.ironmem/api_key` is used for Anthropic only.

## What this document does not claim

- It does not claim your coding assistant keeps prompts local. Traffic between Cursor, Claude, or another client and that client's model provider is outside IronMem.
- It does not claim disk deletion is a forensic wipe. `forget` and `wipe_project` remove application rows. Snapshots, installer backups, and SQLite WAL files can retain copies until you delete them.
