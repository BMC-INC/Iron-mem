# Privacy

IronMem is a local memory store for coding assistants. This note describes what the current source stores, where it lives, and which actions send data off the machine. It is not a hosted-service privacy policy.

## What is stored

By default, files live under `.ironmem` in the home directory (`dirs::home_dir()`):

- `mem.db`: SQLite database of sessions, observations, memories, embeddings, the governance ledger, and local access counters (recall and expansion counts). Those counters are not sent anywhere.
- `settings.json`: configuration, including an optional `auth_token` for the HTTP MCP listener.
- `api_key`: only if you create this file. It is read for Anthropic when an LLM feature needs a key.
- Upgrade backups created by the installer when an existing install changes.
- `server.log`: written when the Claude Code hook launcher starts `ironmem server` and redirects its output. The binary itself does not open this file.
- `fastembed_cache`: only after a binary built with the `local-onnx` feature loads the ONNX embedder. The first load downloads model weights. Memory text is not part of that download.

Project files:

- `IRONMEM.md` in a project directory when context injection writes it. `wipe_project` tries to delete that file.

If `database_url` or `DATABASE_URL` is set, the database is wherever that URL points (for example a Postgres server you operate). That server may be on another machine.

Optional `storage.vector_backend` and `storage.graph_backend` settings can point at Qdrant or Neo4j. The default backend is `native` (inside the local database). If you set a remote URL, vectors or graph rows are stored on that server.

## What stays on the machine by default

- Compression mode defaults to `local`. The session-compression step runs on device. Other LLM features have the independent triggers listed below.
- The embedding provider defaults to `auto`. Auto uses a local Ollama server if one answers, then an in-process ONNX model if this binary was built with `local-onnx`. Otherwise it uses keyword search.
- Auto embeddings do not call OpenAI or Google unless you opt in (see below). An API key in the environment is not enough to enable cloud embeddings.
- `ironmem server` and `ironmem serve` listen on `127.0.0.1` unless you change the SSE bind.
- This repository does not contain a product-analytics client. The word "telemetry" in code and benchmark docs means local counters and in-process timings, not a remote collector. Outbound HTTP call sites in `src/` are provider APIs, embedding APIs, optional Qdrant or Neo4j, local status checks, and the Cloudflare tunnel helper.

## When data leaves the machine

`compression.mode` controls session compression, and `embedding.allow_cloud_fallback` controls auto embeddings. Neither is a global network switch. The LLM features below can send text to the configured provider (Anthropic, OpenAI, Google Gemini, or Vertex AI) even when compression is `local` and embeddings are `none`. They use that provider's credentials, including the Anthropic key file or Vertex Application Default Credentials where applicable.

- Cloud enrichment. Setting `compression.mode` to `cloud_with_local_fallback` sends session text to the configured provider for extraction and its fact-coverage pass.
- Explicit profile refresh. `ironmem profile --refresh`, MCP `refresh_profile`, and REST `POST /refresh_profile` send user-memory summaries to the configured provider to build a profile. Automatic profile refresh after user-memory writes uses a deterministic local rollup.
- LLM reranking and multi-hop retrieval. Enabling `rerank.enabled` or requesting reranking on a REST context request sends the query and candidate evidence to the configured provider. The `cross_encoder` backend can fall back to the LLM when its local model is unavailable, or escalate when a configured score margin requires it. Enabled multi-hop retrieval can send the question and retrieved facts for follow-up queries.
- Observer extraction. Setting `observer.enabled` to true sends the session transcript to the configured provider during compression, including when the compression mode is `local`. The default is off.
- Fact synthesis. REST `POST /synthesize` sends groups of fact memories to the configured provider, including previews with `apply: false`. Setting `auto_dream.enabled` to true enables an idle-project watcher that also invokes synthesis. The watcher defaults to off. MCP `dream_memory` uses the separate deterministic consolidation path; its embedding writes follow the selected embedding provider.
- Scored LongMemEval benchmark runs. Runs without `--dry-run` send the selected question and its context (retrieved memories, or full session history in full-context mode) to the answer model, then the question, reference answer, and generated answer to the judge model. Both use the configured provider. A dry run skips these answer and judge calls; other enabled features retain their own settings.
- Explicit embedding providers `openai` and `google` send memory text to that vendor's embeddings API. IronMem logs a warning when it selects one of these.
- Opt-in auto fallback. If no local embedder is available, `embedding.allow_cloud_fallback` is true or `IRONMEM_EMBED_ALLOW_CLOUD=1`, and `OPENAI_API_KEY` is set, memory text is sent to OpenAI. The default is off. Google is not an automatic fallback. Set `embedding.provider` to `google` to use Google embeddings.
- A custom `embedding.ollama_url` sends embedding inputs to that URL. The default is `http://localhost:11434`; choosing a remote server sends those inputs off the machine independently of the cloud fallback flag.
- `ironmem serve --public` starts a Cloudflare tunnel. The MCP endpoint is reachable on the internet while the tunnel runs. `--no-auth` removes the bearer token.
- A non-loopback `--mcp-sse-bind` or `IRONMEM_MCP_SSE_BIND` exposes the SSE listener on that interface.
- A remote `DATABASE_URL`, Qdrant URL, or Neo4j URL sends the corresponding data to that server.
- An ONNX model download, only for `local-onnx` builds, the first time the weights are not already in `fastembed_cache`.

API keys are read from provider environment variables. `~/.ironmem/api_key` is used for Anthropic only.

## What this document does not claim

- It does not claim your coding assistant keeps prompts local. Traffic between Cursor, Claude, or another client and that client's model provider is outside IronMem.
- It does not claim disk deletion is a forensic wipe. `forget` and `wipe_project` remove application rows. Snapshots, installer backups, and SQLite WAL files can retain copies until you delete them.
