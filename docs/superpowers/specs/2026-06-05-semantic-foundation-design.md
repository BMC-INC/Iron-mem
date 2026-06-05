# Semantic Foundation — Design Spec (Piece 1 of 6)

> Status: Draft for review
> Date: 2026-06-05
> Component: IronMem hybrid semantic retrieval layer
> Roadmap position: **Piece 1 of an approved 6-piece platform decomposition.** Pieces 2–6 (Structured Memory, Knowledge Graph, Reconciliation, Consolidation, Eval/Provenance) are **sibling specs**, each built to completion in dependency order. This decomposition is a build sequence, not deferral: **within Piece 1 there are no stubs, placeholders, or "later" items** — everything described here is fully implemented.

## 1. Context & Problem

IronMem today captures coding-session tool calls, compresses each session into a single summary memory via an LLM (`provider.rs`), and retrieves memories with **keyword-only** full-text search (SQLite FTS5 / Postgres `tsvector` in `db.rs`). Session-start injection (`hooks.rs`, `ironmem inject`) writes the **most recent N** memories into `IRONMEM.md` regardless of relevance.

Two gaps versus current memory tools (Mem0, Zep, Cognee):
1. **No semantic retrieval** — a search for "login error" misses a memory about "auth bug."
2. **Recency-only injection** — what gets auto-loaded ignores what the developer is actually working on.

This spec adds a **semantic retrieval foundation**: local-first embeddings, real ANN vector indexes, hybrid (keyword + vector) search, and relevance-ranked injection. It is the substrate every later piece (facts, graph, reconciliation, consolidation) builds on.

## 2. Goals

- Embed memories with a **pluggable, local-default, no-egress** embedding layer.
- Store embeddings in a **portable canonical table** plus a **real ANN index** per backend (`sqlite-vec` for SQLite, `pgvector` for Postgres).
- Serve **hybrid search** (FTS + vector, fused via Reciprocal Rank Fusion) from the existing `search_memories` / `search_global` / `get_context` surfaces.
- Serve **relevance-ranked injection** via a blended `relevance + recency + importance` score, with a git-derived query signal at session start.
- **Backfill** existing memories idempotently.
- **Never hard-fail**: with no embedder configured/reachable, every path degrades to today's FTS / recency behavior.
- Preserve the `sqlx::AnyPool` foundation and the existing public CLI/MCP/REST behavior.

## 3. Non-Goals (owned by sibling specs, not deferred work within this piece)

- Atomic fact & entity extraction → **Piece 2**.
- Knowledge graph / relations / cross-session entity linking → **Piece 3**.
- Dedup & conflict resolution → **Piece 4**.
- Decay / forgetting / consolidation → **Piece 5** (this spec stores `importance` and `created_at`, which Piece 5 consumes, but performs no pruning).
- Eval harness & provenance tooling → **Piece 6**.

## 4. Architecture Overview

New modules (each a unit with one responsibility, a clear interface, and independent tests):

| Module | Responsibility | Depends on |
|---|---|---|
| `embedder.rs` | "text → vector": `Embedder` trait + Ollama / ONNX / API impls + resolution chain | `config`, `reqwest` |
| `vectorstore.rs` | "store & ANN-query vectors": `VectorStore` trait + `SqliteVecStore` / `PgVectorStore` / `BruteForceStore` | `db`, `embedder` |
| `retrieval.rs` | "rank memories": RRF hybrid search + blended injection scorer | `db`, `vectorstore`, `embedder` |
| `context.rs` | "what is this session about": git-derived query signal | `std::process` (git) |

Changed modules: `db.rs` (additive tables + accessors), `provider.rs` (importance line), `config.rs` (embedding config), `main.rs` (`embed` subcommand + sqlite-vec registration), `mcp.rs` / `server.rs` (route search/context through `retrieval`), `hooks.rs` (relevance injection).

Data flow:
```
session end ─► compress (summary, tags, importance) ─► insert_memory
                                                   └─► embedder.embed(summary+tags) ─► vectorstore.upsert (canonical + ANN)
search tool ─► retrieval.hybrid_search(query) ─► RRF( FTS(query), vectorstore.knn(embed(query)) )
session start ─► context.git_query() ─► embed ─► retrieval.injection_rank(blended) ─► IRONMEM.md
```

## 5. Data Model & Migrations

All migrations are **additive** (`CREATE TABLE/INDEX IF NOT EXISTS`). The FTS5 `memories` virtual table is **not** altered; new data lives in side tables keyed by `memory_id` (= `rowid` in SQLite, `id` in Postgres — matching the existing `delete_memory` branch in `db.rs`).

### 5.1 Canonical embeddings (portable, source of truth)
Forward-compatible with Pieces 2–3 via `owner_type` (Piece 1 writes only `'memory'`).
```sql
CREATE TABLE IF NOT EXISTS embeddings (
    owner_type  TEXT    NOT NULL,           -- 'memory' (later: 'fact','entity')
    owner_id    BIGINT  NOT NULL,
    model       TEXT    NOT NULL,           -- e.g. 'nomic-embed-text'
    dim         INTEGER NOT NULL,
    embedding   BLOB    NOT NULL,           -- dim * 4 bytes, f32 little-endian, UNIT-NORMALIZED
    created_at  BIGINT  NOT NULL,
    PRIMARY KEY (owner_type, owner_id, model)
);
CREATE INDEX IF NOT EXISTS idx_embeddings_owner ON embeddings(owner_type, owner_id);
```
(Postgres: `BLOB`→`BYTEA`; sqlx `Any` binds `Vec<u8>` for both.) The canonical table is backend-portable and is the **rebuild source** for the ANN indexes, so a model/dim change is a re-index (`ironmem embed --force`), never data loss.

### 5.2 Memory importance
```sql
CREATE TABLE IF NOT EXISTS memory_meta (
    memory_id   BIGINT  NOT NULL PRIMARY KEY,
    importance  REAL    NOT NULL DEFAULT 0.5,   -- 0.0–1.0
    created_at  BIGINT  NOT NULL
);
```
Side tables (`embeddings`, `memory_meta`) key on `memory_id` = the FTS5 `rowid`, which is stable under additive inserts (Piece 1 performs no FTS rebuild). Any future FTS rebuild must re-key these tables — noted for later pieces.

### 5.3 SQLite ANN index — `sqlite-vec`
Loaded once via `unsafe { sqlite3_auto_extension(sqlite_vec::sqlite3_vec_init) }` **before** `AnyPool` creation in `Database::new`, so every SQLite connection has `vec0` available without abandoning `AnyPool`.
```sql
-- created lazily for the active model's dimension
CREATE VIRTUAL TABLE IF NOT EXISTS vec_memories USING vec0(
    memory_id INTEGER PRIMARY KEY,
    embedding float[{DIM}]
);
-- KNN query:
SELECT memory_id, distance FROM vec_memories
WHERE embedding MATCH :query AND k = :k ORDER BY distance;
```
`vec0` requires a fixed dimension per table; `{DIM}` comes from the active embedder. If the configured dim changes, the table is dropped and rebuilt from `embeddings` during backfill.

### 5.4 Postgres ANN index — `pgvector`
```sql
CREATE EXTENSION IF NOT EXISTS vector;
CREATE TABLE IF NOT EXISTS memory_embeddings (
    memory_id BIGINT PRIMARY KEY,
    embedding vector({DIM}) NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_memory_embeddings_hnsw
    ON memory_embeddings USING hnsw (embedding vector_cosine_ops);
-- KNN query:
SELECT memory_id, embedding <=> $1 AS distance FROM memory_embeddings ORDER BY distance LIMIT $2;
```
If the `vector` extension is unavailable on the server, the backend logs once and uses `BruteForceStore` over the canonical `embeddings` table (real fallback, not a stub).

## 6. Embedder Layer (`embedder.rs`)

```rust
#[async_trait]
pub trait Embedder: Send + Sync {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>; // returns UNIT-NORMALIZED vectors
    fn id(&self) -> &str;     // model identity stored with each vector, e.g. "nomic-embed-text"
    fn dim(&self) -> usize;
}
```

Implementations (all return L2-normalized vectors so cosine = dot product):
- **`OllamaEmbedder`** (default, no egress): `POST {ollama_url}/api/embed` with `{"model","input":[...]}` (falls back to `/api/embeddings` with `prompt` for older Ollama). Default model `nomic-embed-text` (dim 768). Reuses `reqwest`.
- **`ApiEmbedder`** (opt-in): OpenAI `POST /v1/embeddings` (`text-embedding-3-small`, dim 1536) and Google `text-embedding-004`. Reuses `reqwest`. Requires an explicit embedding API key. `ApiEmbedder::dim()` returns the dimension actually requested (1536 by default; if a `dimensions` param is ever sent, `dim()` must return that exact value so stored vectors and queries stay consistent).
- **`OnnxEmbedder`** (behind `--features local-onnx`): `fastembed` with `bge-small-en-v1.5` (dim 384), fully in-process, model auto-downloaded once. Gated by a Cargo feature so the **default build adds no native ONNX runtime**.
- **`FakeEmbedder`** (test only, `#[cfg(test)]`): deterministic hash-based vectors, no network.

Resolution — `resolve_embedder(cfg) -> Option<Box<dyn Embedder>>`:
1. If `cfg.embedding.provider == "none"` → `None` (FTS/recency only).
2. Explicit provider in config → that impl (error if its prerequisite, e.g. API key, is missing).
3. `"auto"` (default): Ollama if `{ollama_url}/api/tags` is reachable → else `OnnxEmbedder` if compiled in → else `ApiEmbedder` if a key is configured → else `None`.
Resolution result is logged **once** at startup.

Batching: `embed` accepts a slice; callers batch (backfill chunks of 64). Network/parse errors return `Err`; callers decide fallback (never panic).

## 7. Vector Store Layer (`vectorstore.rs`)

```rust
#[async_trait]
pub trait VectorStore: Send + Sync {
    async fn upsert(&self, db: &Database, owner_id: i64, model: &str, dim: usize, embedding: &[f32]) -> Result<()>;
    async fn knn(&self, db: &Database, project: Option<&str>, query: &[f32], model: &str, k: usize)
        -> Result<Vec<(i64, f32)>>; // (memory_id, similarity 0..1)
    async fn delete(&self, db: &Database, owner_id: i64) -> Result<()>;
}
```
- **`upsert`** always writes the canonical `embeddings` row, then syncs the backend ANN structure (`vec_memories` / `memory_embeddings`).
- **`SqliteVecStore`** / **`PgVectorStore`**: real ANN queries (§5.3/§5.4). `knn` converts distance→similarity (`1 - cosine_distance`). `project` filter applied by joining `memories` on `memory_id`. Because `vec0` pushes `k` into the `MATCH` clause, the project filter runs **after** KNN; to avoid returning fewer than `limit` rows, over-fetch `k = limit * OVERFETCH` (OVERFETCH = 8) before the join, then truncate to `limit`.
- **`BruteForceStore`**: loads canonical embeddings for `(model, project)`, computes dot product in Rust, returns top-k. Used when the ANN extension is unavailable. Correct and complete, just unindexed.
- Selection: `make_vector_store(db)` picks `SqliteVecStore`/`PgVectorStore` when the extension is present, else `BruteForceStore`. Logged once.

Encoding helpers (`embedding_codec`): `encode(&[f32]) -> Vec<u8>` (LE) and `decode(&[u8]) -> Vec<f32>`, with a length/dim assertion.

## 8. Retrieval (`retrieval.rs`)

### 8.1 Hybrid search (on-demand tools)
`hybrid_search(db, embedder, store, project, query, limit) -> Vec<Memory>`:
1. `fts = db::search_memories|search_all_memories(query)` (existing) → ranked list.
2. If embedder+vectors available: `vec = store.knn(embed(query))` → ranked list; else `vec = []`.
3. **Reciprocal Rank Fusion**: `score(m) = Σ_lists 1/(RRF_K + rank_list(m))`, `RRF_K = 60`. Dedup by `memory_id`, sort desc, take `limit`.
4. If `vec` empty → result is exactly today's FTS output (behavior-preserving).

### 8.2 Blended injection (session start)
`injection_rank(db, embedder, store, project, query_vec, limit) -> Vec<Memory>`:
- `relevance(m)` = `store.knn` similarity for `m` (0 if no vector/embedder).
- `recency(m)` = `exp(-age_seconds / (half_life_days * 86400))`.
- `importance(m)` = `memory_meta.importance` (default 0.5).
- `score(m) = w_r*relevance + w_t*recency + w_i*importance` (defaults `w_r=0.5, w_t=0.3, w_i=0.2`, `half_life_days=30`; all in config).
- No embedder / no `query_vec` → `w_r` treated as 0 and ranking falls back to recency+importance (and to pure recency if `memory_meta` is empty) — i.e. today's behavior or better.

### 8.3 Session-start query signal (`context.rs`)
`git_query(project) -> Option<String>`:
- Concatenate `git -C {project} log -n 20 --format=%s` (subjects) + `git -C {project} status --porcelain` paths + `git -C {project} diff --name-only HEAD` paths.
- Returns `None` if not a git repo / git missing / empty → injection falls back to recency+importance.
- Bounded to a sane char budget before embedding.

## 9. Importance Extraction (`provider.rs`)

- `build_prompt` gains one output line: `IMPORTANCE: [1-10 — how durably useful this memory is for future sessions]`.
- `CompressionResult` gains `importance: u8` (1–10). `parse_response` parses the line; on absence/parse failure defaults to `5`.
- `run_compression` writes `memory_meta(memory_id, importance/10.0, created_at)` after `insert_memory`, then best-effort embeds the memory (`summary + " " + tags`) and calls `store.upsert`. **Embedding failure is logged and swallowed** — the memory is already persisted; the vector is filled later by backfill.
- `run_compression` is **duplicated** today in `mcp.rs` (`IronMemServer::run_compression`) and `server.rs` (`run_compression`). The implementation MUST update **both** call sites — or, preferred, extract a single shared `compress::run(db, store, embedder, cfg, session_id)` helper both delegate to — so importance + embedding writes can never diverge between the stdio/HTTP MCP path and the REST path.

## 10. Backfill & Migration CLI (`main.rs`)

New subcommand:
```
ironmem embed [--project <path>] [--all] [--force]
```
- Default: embed memories (and write `memory_meta` if missing, importance `0.5`) that **lack a vector for the active model**. Idempotent.
- `--all`: every project; `--project`: one project (default = resolved cwd project).
- `--force`: re-embed all (used after a model/dim change; drops & rebuilds `vec_memories` for the new dim).
- Processes in batches of 64 with a progress line; continues past individual failures, reports a final count.

`run_compression` also embeds inline at session end (§9), so steady-state needs no manual backfill — `embed` is for the one-time upgrade and model switches.

## 11. Surface Wiring

- `mcp.rs`: `handle_search_memories`, `handle_search_global`, `handle_get_context` call `retrieval::hybrid_search`. New optional tool arg `semantic: bool` (default true) to force FTS-only if desired.
- `server.rs`: REST `get_context` / `api_list_memories` (with `query`) route through `retrieval::hybrid_search`.
- `hooks.rs` + `run_inject`: build `context::git_query` → embed → `retrieval::injection_rank` → write `IRONMEM.md`. No git/embedder → existing recency path.
- `IronMemServer` / REST `AppState` hold a shared `Option<Arc<dyn Embedder>>` and `Arc<dyn VectorStore>`, constructed once at startup.

## 12. Configuration (`config.rs`)

New optional `embedding` block (all fields defaulted; absent block = `auto`):
```jsonc
"embedding": {
  "provider": "auto",            // auto | ollama | openai | google | onnx | none
  "model": "nomic-embed-text",   // overrides per-provider default
  "ollama_url": "http://localhost:11434",
  "weights": { "relevance": 0.5, "recency": 0.3, "importance": 0.2 },
  "recency_half_life_days": 30
}
```
`DATABASE_URL` / existing settings unchanged. `dim` is derived from the active embedder (`Embedder::dim`), not hand-configured, to prevent mismatch.

## 13. Error Handling & Graceful Degradation

| Failure | Behavior |
|---|---|
| No embedder resolved | Log once; hybrid_search = FTS only; injection = recency+importance |
| Embedder reachable but errors mid-call | Caller logs, treats as empty vector list for that op; never panics |
| Embedding fails on insert | Memory still saved; vector backfilled later |
| ANN extension absent | `BruteForceStore` over canonical table |
| Stored vector model/dim ≠ active model | That vector ignored for KNN (only same-model vectors compared); `--force` re-indexes |
| git query fails | Injection falls back to recency+importance |

Invariant: **with `embedding.provider="none"` IronMem behaves exactly as it does today.**

## 14. Testing Strategy

Unit (no network, via `FakeEmbedder` + temp SQLite):
- `embedding_codec` round-trip + dim/length assertion.
- Cosine/dot on normalized vectors; RRF fusion ordering (known ranks → known fused order).
- Blended-score math: recency decay at t=0/half-life/∞; weight blending; importance default.
- `parse_response` importance parsing incl. missing-line default.
- `git_query` returns `None` outside a repo.

Integration (temp SQLite, `FakeEmbedder`):
- Insert memories → `upsert` → `knn` returns nearest by construction.
- `hybrid_search` returns union of FTS + vector hits; with empty vectors equals FTS output (behavior-preserving).
- `ironmem embed` backfill is idempotent; `--force` rebuilds after a simulated dim change.
- `embedding.provider="none"` path == legacy behavior.
- Existing `db.rs` tests remain green (AnyPool untouched).

`sqlite-vec`-specific tests gated so they run when the extension is compiled; `pgvector` tests gated behind a `DATABASE_URL` env (skipped otherwise).

## 15. Dependencies

| Crate | Purpose | Build impact |
|---|---|---|
| `sqlite-vec` | SQLite `vec0` ANN | small; links the vec extension |
| `pgvector` | sqlx `vector` type binding | small; Postgres only |
| `fastembed` | in-process ONNX embeddings | **behind `--features local-onnx`** (default off) |
| `async-trait` | trait objects for `Embedder`/`VectorStore` | negligible |

Ollama + OpenAI/Google reuse the existing `reqwest`. Default `cargo build` adds no native ONNX runtime.

**Linkage note:** `sqlite-vec` must resolve `sqlite3_auto_extension` against the **same** statically-bundled `libsqlite3-sys` that sqlx links (confirmed bundled: `libsqlite3-sys 0.30.1`, sqlx 0.8.6). If `sqlite-vec` pulls a second/system libsqlite3, `vec0` either fails to register or link errors with duplicate symbols. The implementation must pin `sqlite-vec` to the matching `libsqlite3-sys` linkage and gate it with the §17 smoke test before anything else in this piece is built.

## 16. Forward-Compatibility (how Pieces 2–6 attach)

- `embeddings.owner_type` already supports `'fact'`/`'entity'` (Piece 2/3) with no migration.
- `VectorStore` is owner-type-agnostic at the trait level; Piece 2 adds fact/entity upsert call sites.
- `retrieval` RRF + blended scorer extend to fact/graph result lists without redesign.
- `memory_meta.importance` + `created_at` are the inputs Piece 5 (decay/consolidation) consumes.
- `provider.rs` structured-output groundwork (importance line) extends to full JSON fact extraction in Piece 2.

## 17. Risks & Mitigations

- **`sqlite-vec` auto-extension + sqlx bundled sqlite**: must register before pool creation **and** link the same bundled `libsqlite3-sys` symbol sqlx uses — otherwise `vec0` registers against the wrong libsqlite3 (unknown-module at query time) or fails with duplicate-symbol link errors. This is the single highest integration risk and is the **first** task in the plan. Mitigation: an integration test that creates a `vec0` table and runs one KNN on a fresh `AnyPool` connection, gating all downstream work.
- **Dimension lock-in per `vec0` table**: handled by `--force` rebuild from canonical embeddings on model change.
- **Embedding latency at session end**: one batched call on already-async compression path; failures are non-blocking.
- **`pgvector` not installed on a user's Postgres**: detected; `BruteForceStore` fallback keeps correctness.

## 18. Open Questions

1. Default Ollama model — `nomic-embed-text` (768) vs `mxbai-embed-large` (1024)? Spec assumes `nomic-embed-text` for size/speed; configurable.
2. Should `hybrid_search` also rerank with a cross-encoder? Deferred to Piece 6 (eval will tell us if RRF alone suffices); not in Piece 1.
