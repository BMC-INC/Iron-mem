# Semantic Foundation Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a local-first hybrid semantic retrieval layer to IronMem — embeddings, real ANN vector indexes, RRF hybrid search, and blended relevance+recency+importance injection — without breaking the existing keyword-only behavior.

**Architecture:** Four new focused modules (`embedder`, `vectorstore`, `retrieval`, `context`) plus additive side-tables in `db.rs`. The existing `sqlx::AnyPool` foundation is preserved; `sqlite-vec` is loaded via `sqlite3_auto_extension` before pool creation so `vec0` works on every connection. Every new path degrades to today's FTS/recency behavior when no embedder is configured.

**Tech Stack:** Rust, sqlx 0.8 (`Any` over SQLite/Postgres), `sqlite-vec` (`vec0`), pgvector (`vector` + HNSW), `reqwest` (Ollama/OpenAI/Google), `fastembed` (optional, `--features local-onnx`), `async-trait`.

**Spec:** `docs/superpowers/specs/2026-06-05-semantic-foundation-design.md`

**Conventions for the executor:**
- Use `cargo build` (debug) while iterating; `cargo build --release` only for the final install (release build is slow due to LTO).
- Run a single test with `cargo test <name> -- --nocolor`; a module with `cargo test <module>::`.
- After each task's tests pass, **commit** (messages end with the project's `Co-Authored-By` trailer).
- No `TODO`/`unimplemented!()`/stubs land in committed code. Every task leaves the tree compiling and green.
- Branch: `feature/semantic-foundation` (already created).

---

## Execution Progress — RESUME HERE

**Status (2026-06-05):** Chunks 1–4 complete and committed; all 15 tests green. Chunks 5–8 remain.

**Branch:** `feature/semantic-foundation` — commits: chunk1 `fe4e7a5`, chunk2 `8529144`, chunk3 `aa7f758`, chunk4 `c6e7234`.

**CRITICAL build note:** the repo's `target/` contends with the IDE's rust-analyzer on the cargo build lock and will appear to hang for 30+ min. ALWAYS build/test in an isolated target dir, and never pipe a long cargo command through `tail` (it buffers and looks dead):
```
CARGO_TARGET_DIR=/Users/kingjames/.ironmem/target-build CARGO_TERM_PROGRESS_WHEN=never cargo test
```

**Done:**
- ✅ Chunk 1 — deps (`sqlite-vec`, `async-trait`, `libsqlite3-sys` bundled, optional `fastembed`); `embedding_codec`; sqlite-vec loaded via `sqlite3_auto_extension` (vec0 smoke test = the gate, passes).
- ✅ Chunk 2 — `embeddings` + `memory_meta` tables; `ensure_ann`/`drop_ann`; embedding/meta accessors + `memory_ids_missing_embedding` / `all_memory_ids_with_text`.
- ✅ Chunk 3 — `EmbeddingConfig` in config.rs; `Embedder` trait + `Ollama`/`Api`/`Onnx`(feature)/`Fake`; `resolve_embedder` chain with dim probe.
- ✅ Chunk 4 — `VectorStore` trait + `BruteForce`/`SqliteVec`/`PgVector` + `make_vector_store`. **vec0 created with `distance_metric=cosine`** so similarity = `1 - distance`.

**Next — Chunk 5:** create `retrieval.rs` (`rrf_fuse`, `hybrid_search`, `recency_weight` = `0.5^(age/hl)`, `blended_score`, `injection_rank`) and `context.rs` (`git_query`). Then Chunk 6 (provider `IMPORTANCE` + `compress::run`), Chunk 7 (wire mcp/server/hooks, `ironmem embed` backfill, Task 24b delete-cleanup, config already added in Chunk 3), Chunk 8 (e2e test, docs, clippy `-D warnings`, release build).

**Expected (non-error) dead-code warnings** until wired up in Ch5/Ch7: `drop_ann`, `all_memory_ids_with_text`, `make_vector_store`, `pg_literal`.

**Env note:** `claude mcp add ironmem` is blocked by an enterprise MCP policy on this machine (admin allowlist needed); not a code issue.

---

## File Structure

| File | Status | Responsibility |
|---|---|---|
| `Cargo.toml` | modify | add `async-trait`, `sqlite-vec`, `libsqlite3-sys` (bundled, matching sqlx), optional `fastembed` feature |
| `src/embedding_codec.rs` | create | `encode`/`decode`/`normalize` for f32↔bytes; cosine/dot helpers |
| `src/embedder.rs` | create | `Embedder` trait + `Fake`/`Ollama`/`Api`/`Onnx` impls + `resolve_embedder` |
| `src/vectorstore.rs` | create | `VectorStore` trait + `BruteForce`/`SqliteVec`/`PgVector` impls + `make_vector_store` |
| `src/retrieval.rs` | create | `hybrid_search` (RRF) + `injection_rank` (blended score) |
| `src/context.rs` | create | `git_query` session-start query signal |
| `src/compress.rs` | create | shared `run(...)` compression helper (dedupes mcp.rs/server.rs) + embed/meta writes |
| `src/db.rs` | modify | sqlite-vec registration in `new`; additive migrations + embedding/meta accessors |
| `src/provider.rs` | modify | `IMPORTANCE:` output line + `CompressionResult.importance` |
| `src/config.rs` | modify | `embedding` config block + defaults |
| `src/main.rs` | modify | `embed` subcommand; module declarations; wire embedder/store into servers |
| `src/mcp.rs` | modify | route search/context through `retrieval`; hold embedder+store; call `compress::run` |
| `src/server.rs` | modify | route search/context through `retrieval`; hold embedder+store; call `compress::run` |
| `src/hooks.rs` | modify | relevance injection via `context::git_query` + `retrieval::injection_rank` |
| `tests/` (inline `#[cfg(test)]`) | create | unit + integration tests per task |

---

## Chunk 1: Dependencies & sqlite-vec foundation

The single highest risk (spec §17) is `sqlite-vec` linking the *same* bundled libsqlite3 as sqlx. Prove it first; nothing else matters if `vec0` won't load.

### Task 1: Add dependencies

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Add crates** to `[dependencies]`:

```toml
async-trait = "0.1"
sqlite-vec = "0.1"
# Must match the libsqlite3-sys sqlx 0.8 bundles (0.30.x) so there is ONE statically linked sqlite.
libsqlite3-sys = { version = "0.30", features = ["bundled"] }
fastembed = { version = "4", optional = true }
```

- [ ] **Step 2: Add feature flag** under `[features]` (create the section if absent):

```toml
[features]
default = []
local-onnx = ["dep:fastembed"]
```

- [ ] **Step 3: Verify it resolves**

Run: `cargo build`
Expected: compiles (no new code yet). If `libsqlite3-sys` reports a version conflict with sqlx's, pin to the exact version `cargo tree -i libsqlite3-sys` reports.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "build: add embedding/vector deps (sqlite-vec, async-trait, optional fastembed)"
```

### Task 2: Embedding codec

**Files:**
- Create: `src/embedding_codec.rs`
- Modify: `src/main.rs` (add `mod embedding_codec;`)

- [ ] **Step 1: Write the failing test** in `src/embedding_codec.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_roundtrip() {
        let v = vec![0.0_f32, 1.5, -2.25, 3.0];
        let bytes = encode(&v);
        assert_eq!(bytes.len(), v.len() * 4);
        assert_eq!(decode(&bytes), v);
    }

    #[test]
    fn normalize_makes_unit_length() {
        let v = normalize(&[3.0, 4.0]);
        let mag = (v[0] * v[0] + v[1] * v[1]).sqrt();
        assert!((mag - 1.0).abs() < 1e-6);
    }

    #[test]
    fn dot_of_normalized_equals_cosine() {
        let a = normalize(&[1.0, 2.0, 3.0]);
        let b = normalize(&[2.0, 1.0, 0.5]);
        let d = dot(&a, &b);
        assert!(d > -1.0001 && d < 1.0001);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test embedding_codec`
Expected: FAIL — `encode`/`decode`/`normalize`/`dot` not found.

- [ ] **Step 3: Write minimal implementation** at the top of `src/embedding_codec.rs`:

```rust
//! Encode/decode embeddings to bytes and similarity helpers.

/// f32 little-endian byte encoding (length = dim * 4).
pub fn encode(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for f in v {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

pub fn decode(bytes: &[u8]) -> Vec<f32> {
    debug_assert_eq!(bytes.len() % 4, 0, "embedding blob length must be a multiple of 4");
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// L2-normalize; zero vectors are returned unchanged.
pub fn normalize(v: &[f32]) -> Vec<f32> {
    let mag = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if mag == 0.0 {
        return v.to_vec();
    }
    v.iter().map(|x| x / mag).collect()
}

pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}
```

Add `mod embedding_codec;` to `src/main.rs` (after the existing `mod` lines).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test embedding_codec`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add src/embedding_codec.rs src/main.rs
git commit -m "feat: embedding codec (encode/decode/normalize/dot)"
```

### Task 3: Load sqlite-vec + smoke test (gating risk)

**Files:**
- Modify: `src/db.rs` (`Database::new`)

- [ ] **Step 1: Register the extension** at the very start of `Database::new`, BEFORE `AnyPoolOptions::new()`:

```rust
// Load sqlite-vec into every new sqlite connection. Safe to call repeatedly;
// must run before any pool connection is opened.
unsafe {
    libsqlite3_sys::sqlite3_auto_extension(Some(std::mem::transmute(
        sqlite_vec::sqlite3_vec_init as *const (),
    )));
}
```

(If `sqlite_vec` exposes a typed init that `sqlite3_auto_extension` accepts directly, prefer that and drop the transmute. Verify the exact signature from `sqlite-vec` crate docs in this step; adjust the cast so it compiles without warnings.)

- [ ] **Step 2: Write the failing smoke test** in `src/db.rs` `#[cfg(test)] mod tests`:

```rust
#[tokio::test]
async fn sqlite_vec_extension_loads_and_knn_runs() -> Result<()> {
    let (db, path) = test_db().await?;
    sqlx::query("CREATE VIRTUAL TABLE IF NOT EXISTS vt_smoke USING vec0(id INTEGER PRIMARY KEY, embedding float[3])")
        .execute(&db.pool).await?;
    let blob = crate::embedding_codec::encode(&crate::embedding_codec::normalize(&[1.0, 0.0, 0.0]));
    sqlx::query("INSERT INTO vt_smoke(id, embedding) VALUES (1, ?)")
        .bind(blob.clone()).execute(&db.pool).await?;
    let rows: Vec<sqlx::any::AnyRow> = sqlx::query(
        "SELECT id, distance FROM vt_smoke WHERE embedding MATCH ? AND k = 1 ORDER BY distance")
        .bind(blob).fetch_all(&db.pool).await?;
    assert_eq!(rows.len(), 1);
    let _ = std::fs::remove_file(path);
    Ok(())
}
```

- [ ] **Step 3: Run it**

Run: `cargo test sqlite_vec_extension_loads_and_knn_runs -- --nocapture`
Expected: PASS. If it fails with "no such module: vec0", the auto-extension is not linking against sqlx's sqlite — fix the dependency wiring (Task 1, Step 3) before continuing. **Do not proceed past this task until it passes.**

- [ ] **Step 4: Commit**

```bash
git add src/db.rs
git commit -m "feat: load sqlite-vec extension on every connection (+ vec0 smoke test)"
```

---

## Chunk 2: Schema, migrations & db accessors

### Task 4: Additive migrations (canonical tables)

**Files:**
- Modify: `src/db.rs` (`migrate`)

- [ ] **Step 1: Write the failing test**:

```rust
#[tokio::test]
async fn embedding_and_meta_tables_exist_after_migrate() -> Result<()> {
    let (db, path) = test_db().await?;
    sqlx::query("INSERT INTO embeddings(owner_type, owner_id, model, dim, embedding, created_at) VALUES ('memory', 1, 'm', 3, ?, 0)")
        .bind(crate::embedding_codec::encode(&[0.0,0.0,0.0])).execute(&db.pool).await?;
    sqlx::query("INSERT INTO memory_meta(memory_id, importance, created_at) VALUES (1, 0.5, 0)")
        .execute(&db.pool).await?;
    let _ = std::fs::remove_file(path);
    Ok(())
}
```

- [ ] **Step 2: Run to verify fail**

Run: `cargo test embedding_and_meta_tables_exist_after_migrate`
Expected: FAIL — no such table `embeddings`.

- [ ] **Step 3: Add migrations** at the end of `migrate()` (before `Ok(())`). Backend-portable except the BLOB/BYTEA type:

```rust
let blob_type = match self.backend { Backend::Sqlite => "BLOB", Backend::Postgres => "BYTEA" };
sqlx::query(&format!(
    "CREATE TABLE IF NOT EXISTS embeddings (
        owner_type TEXT NOT NULL,
        owner_id   BIGINT NOT NULL,
        model      TEXT NOT NULL,
        dim        INTEGER NOT NULL,
        embedding  {blob_type} NOT NULL,
        created_at BIGINT NOT NULL,
        PRIMARY KEY (owner_type, owner_id, model)
    )")).execute(&self.pool).await?;
sqlx::query("CREATE INDEX IF NOT EXISTS idx_embeddings_owner ON embeddings(owner_type, owner_id)")
    .execute(&self.pool).await?;
sqlx::query(
    "CREATE TABLE IF NOT EXISTS memory_meta (
        memory_id  BIGINT NOT NULL PRIMARY KEY,
        importance REAL NOT NULL DEFAULT 0.5,
        created_at BIGINT NOT NULL
    )").execute(&self.pool).await?;
```

- [ ] **Step 4: Run to verify pass** → `cargo test embedding_and_meta_tables_exist_after_migrate` → PASS.
- [ ] **Step 5: Commit** → `git commit -am "feat: add embeddings + memory_meta tables (additive migration)"`

### Task 5: ANN index creation helpers

**Files:**
- Modify: `src/db.rs`

- [ ] **Step 1:** Add methods that lazily create the per-backend ANN structures for a given `dim`:

```rust
impl Database {
    /// SQLite: create vec0 table sized for `dim` if absent. Postgres: ensure vector ext + table + HNSW.
    pub async fn ensure_ann(&self, dim: usize) -> Result<()> {
        match self.backend {
            Backend::Sqlite => {
                sqlx::query(&format!(
                    "CREATE VIRTUAL TABLE IF NOT EXISTS vec_memories USING vec0(memory_id INTEGER PRIMARY KEY, embedding float[{dim}])"))
                    .execute(&self.pool).await?;
            }
            Backend::Postgres => {
                sqlx::query("CREATE EXTENSION IF NOT EXISTS vector").execute(&self.pool).await.ok();
                sqlx::query(&format!(
                    "CREATE TABLE IF NOT EXISTS memory_embeddings (memory_id BIGINT PRIMARY KEY, embedding vector({dim}))"))
                    .execute(&self.pool).await?;
                sqlx::query("CREATE INDEX IF NOT EXISTS idx_memory_embeddings_hnsw ON memory_embeddings USING hnsw (embedding vector_cosine_ops)")
                    .execute(&self.pool).await.ok();
            }
        }
        Ok(())
    }

    /// Drop ANN structures (used by `embed --force` on dim change).
    pub async fn drop_ann(&self) -> Result<()> {
        let q = match self.backend { Backend::Sqlite => "DROP TABLE IF EXISTS vec_memories", Backend::Postgres => "DROP TABLE IF EXISTS memory_embeddings" };
        sqlx::query(q).execute(&self.pool).await?; Ok(())
    }
}
```

Note: `.ok()` on the Postgres `CREATE EXTENSION`/HNSW so a server lacking pgvector privileges degrades to brute-force rather than failing migration.

- [ ] **Step 2: Test (sqlite path)** — `ensure_ann(3)` then insert into `vec_memories` succeeds. Run `cargo test ensure_ann` → PASS.
- [ ] **Step 3: Commit** → `git commit -am "feat: lazy ANN index creation (vec0 / pgvector)"`

### Task 6: Embedding & meta accessors

**Files:**
- Modify: `src/db.rs`

- [ ] **Step 1: Write failing tests** for: `upsert_embedding` then `get_embedding('memory', id, model)` returns the bytes; `upsert_memory_meta` then `get_memory_meta(id)` returns importance; `list_memory_ids_missing_embedding(model)` returns ids without a vector.
- [ ] **Step 2: Run → FAIL.**
- [ ] **Step 3: Implement** (canonical table only here; ANN sync lives in `vectorstore`):

```rust
pub async fn upsert_embedding(db: &Database, owner_type: &str, owner_id: i64, model: &str, dim: i64, embedding: &[u8]) -> Result<()> {
    let now = chrono::Utc::now().timestamp();
    let sql = match db.backend {
        Backend::Sqlite => "INSERT INTO embeddings(owner_type,owner_id,model,dim,embedding,created_at) VALUES($1,$2,$3,$4,$5,$6)
                            ON CONFLICT(owner_type,owner_id,model) DO UPDATE SET embedding=excluded.embedding, dim=excluded.dim, created_at=excluded.created_at",
        Backend::Postgres => "INSERT INTO embeddings(owner_type,owner_id,model,dim,embedding,created_at) VALUES($1,$2,$3,$4,$5,$6)
                            ON CONFLICT(owner_type,owner_id,model) DO UPDATE SET embedding=excluded.embedding, dim=excluded.dim, created_at=excluded.created_at",
    };
    sqlx::query(sql).bind(owner_type).bind(owner_id).bind(model).bind(dim).bind(embedding.to_vec()).bind(now)
        .execute(&db.pool).await?;
    Ok(())
}

pub async fn get_embedding(db: &Database, owner_type: &str, owner_id: i64, model: &str) -> Result<Option<Vec<u8>>> {
    let row: Option<sqlx::any::AnyRow> = sqlx::query("SELECT embedding FROM embeddings WHERE owner_type=$1 AND owner_id=$2 AND model=$3")
        .bind(owner_type).bind(owner_id).bind(model).fetch_optional(&db.pool).await?;
    Ok(row.map(|r| r.get::<Vec<u8>, _>("embedding")))
}

pub async fn upsert_memory_meta(db: &Database, memory_id: i64, importance: f64) -> Result<()> {
    let now = chrono::Utc::now().timestamp();
    let sql = "INSERT INTO memory_meta(memory_id,importance,created_at) VALUES($1,$2,$3)
               ON CONFLICT(memory_id) DO UPDATE SET importance=excluded.importance";
    sqlx::query(sql).bind(memory_id).bind(importance).bind(now).execute(&db.pool).await?;
    Ok(())
}

pub async fn get_memory_meta(db: &Database, memory_id: i64) -> Result<f64> {
    let row: Option<sqlx::any::AnyRow> = sqlx::query("SELECT importance FROM memory_meta WHERE memory_id=$1")
        .bind(memory_id).fetch_optional(&db.pool).await?;
    Ok(row.map(|r| r.get::<f64,_>("importance")).unwrap_or(0.5))
}

/// memory ids (rowid in sqlite / id in pg) with no embedding row for `model`.
pub async fn memory_ids_missing_embedding(db: &Database, model: &str, project: Option<&str>) -> Result<Vec<(i64, String, Option<String>)>> {
    let id_col = match db.backend { Backend::Sqlite => "rowid", Backend::Postgres => "id" };
    let mut sql = format!(
        "SELECT m.{id_col} AS id, m.summary AS summary, m.tags AS tags FROM memories m
         WHERE NOT EXISTS (SELECT 1 FROM embeddings e WHERE e.owner_type='memory' AND e.owner_id=m.{id_col} AND e.model=$1)");
    if project.is_some() { sql.push_str(" AND m.project=$2"); }
    let mut q = sqlx::query(&sql).bind(model);
    if let Some(p) = project { q = q.bind(p); }
    let rows = q.fetch_all(&db.pool).await?;
    Ok(rows.into_iter().map(|r| (r.get::<i64,_>("id"), r.get::<String,_>("summary"), r.try_get::<Option<String>,_>("tags").ok().flatten())).collect())
}
```

Verify `ON CONFLICT` works under sqlx `Any` for both backends in Step 2; if Postgres needs different upsert syntax, branch like the existing `insert_memory`.

- [ ] **Step 4: Run → PASS.**
- [ ] **Step 5: Commit** → `git commit -am "feat: embedding + memory_meta accessors"`

---

## Chunk 3: Embedder layer

### Task 7: `Embedder` trait + `FakeEmbedder`

**Files:**
- Create: `src/embedder.rs`
- Modify: `src/main.rs` (`mod embedder;`)

- [ ] **Step 1: Write failing test** in `src/embedder.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn fake_embedder_is_deterministic_and_normalized() {
        let e = FakeEmbedder::new(8);
        let a = e.embed(&["hello".into()]).await.unwrap();
        let b = e.embed(&["hello".into()]).await.unwrap();
        assert_eq!(a, b);
        assert_eq!(a[0].len(), 8);
        let mag: f32 = a[0].iter().map(|x| x*x).sum::<f32>().sqrt();
        assert!((mag - 1.0).abs() < 1e-5);
    }
}
```

- [ ] **Step 2: Run → FAIL.**
- [ ] **Step 3: Implement** trait + `FakeEmbedder`:

```rust
use anyhow::Result;
use async_trait::async_trait;
use crate::embedding_codec::normalize;

#[async_trait]
pub trait Embedder: Send + Sync {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>; // unit-normalized
    fn id(&self) -> &str;
    fn dim(&self) -> usize;
}

#[cfg(test)]
pub struct FakeEmbedder { dim: usize }
#[cfg(test)]
impl FakeEmbedder { pub fn new(dim: usize) -> Self { Self { dim } } }
#[cfg(test)]
#[async_trait]
impl Embedder for FakeEmbedder {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| {
            let mut v = vec![0.0f32; self.dim];
            for (i, b) in t.bytes().enumerate() { v[i % self.dim] += b as f32; }
            normalize(&v)
        }).collect())
    }
    fn id(&self) -> &str { "fake" }
    fn dim(&self) -> usize { self.dim }
}
```

- [ ] **Step 4: Run → PASS.** **Step 5: Commit** → `"feat: Embedder trait + test FakeEmbedder"`

### Task 8: `OllamaEmbedder`

**Files:** Modify `src/embedder.rs`

- [ ] **Step 1: Implement** (no live test in CI; add a `#[ignore]` integration test that hits a local Ollama):

```rust
pub struct OllamaEmbedder { client: reqwest::Client, base: String, model: String, dim: usize }

impl OllamaEmbedder {
    pub fn new(base: String, model: String, dim: usize) -> Self {
        Self { client: reqwest::Client::new(), base, model, dim }
    }
    pub async fn reachable(base: &str) -> bool {
        reqwest::Client::new().get(format!("{base}/api/tags")).send().await.map(|r| r.status().is_success()).unwrap_or(false)
    }
}

#[async_trait]
impl Embedder for OllamaEmbedder {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        #[derive(serde::Serialize)] struct Req<'a> { model: &'a str, input: &'a [String] }
        #[derive(serde::Deserialize)] struct Resp { embeddings: Vec<Vec<f32>> }
        let resp = self.client.post(format!("{}/api/embed", self.base))
            .json(&Req { model: &self.model, input: texts }).send().await?;
        if !resp.status().is_success() {
            return Err(anyhow::anyhow!("Ollama embed error {}: {}", resp.status(), resp.text().await.unwrap_or_default()));
        }
        let data: Resp = resp.json().await?;
        Ok(data.embeddings.iter().map(|v| crate::embedding_codec::normalize(v)).collect())
    }
    fn id(&self) -> &str { &self.model }
    fn dim(&self) -> usize { self.dim }
}
```

- [ ] **Step 2:** `cargo build` → compiles. **Step 3: Commit** → `"feat: OllamaEmbedder (local, no-egress default)"`

### Task 9: `ApiEmbedder` (OpenAI + Google)

**Files:** Modify `src/embedder.rs`

- [ ] **Step 1: Implement** an enum-backed `ApiEmbedder` reusing the provider key resolution in `provider.rs::resolve_api_key`. OpenAI: `POST {OPENAI}/v1/embeddings {model, input:[...]}` → `data[].embedding` (dim 1536 for `text-embedding-3-small`). Google: `text-embedding-004` batch endpoint. Normalize outputs. `dim()` returns the requested dimension (1536 default; if a `dimensions` param is sent, return that exact value).
- [ ] **Step 2:** `cargo build` → compiles. **Step 3: Commit** → `"feat: ApiEmbedder (OpenAI/Google, opt-in)"`

### Task 10: `OnnxEmbedder` (feature-gated)

**Files:** Modify `src/embedder.rs`

- [ ] **Step 1: Implement** behind `#[cfg(feature = "local-onnx")]` using `fastembed` (`bge-small-en-v1.5`, dim 384); normalize outputs; `id()` = model name.
- [ ] **Step 2:** `cargo build` (default, feature OFF) compiles unchanged; `cargo build --features local-onnx` compiles with the impl. **Step 3: Commit** → `"feat: OnnxEmbedder behind local-onnx feature"`

### Task 11: `resolve_embedder` chain

**Files:** Modify `src/embedder.rs`

- [ ] **Step 1: Write failing test:** with a config of `provider="none"`, `resolve_embedder` returns `None`.
- [ ] **Step 2: Run → FAIL.**
- [ ] **Step 3: Implement** `pub async fn resolve_embedder(cfg: &Config) -> Option<Box<dyn Embedder>>` implementing the §6 order: `none` → explicit provider → `auto` (Ollama if reachable → onnx if compiled → api if key present → None). Log the chosen embedder once via `tracing::info!`.
- [ ] **Step 4: Run → PASS.** **Step 5: Commit** → `"feat: embedder resolution chain (local-default, graceful)"`

---

## Chunk 4: VectorStore layer

### Task 12: `VectorStore` trait + `BruteForceStore`

**Files:**
- Create: `src/vectorstore.rs`
- Modify: `src/main.rs` (`mod vectorstore;`)

- [ ] **Step 1: Write failing test** (temp sqlite): upsert 3 known vectors via `BruteForceStore`, `knn` for a query nearest to vector #2 returns #2 first.
- [ ] **Step 2: Run → FAIL.**
- [ ] **Step 3: Implement** trait + brute-force impl:

```rust
use anyhow::Result;
use async_trait::async_trait;
use crate::db::{Database, Backend};
use crate::embedding_codec::{decode, dot};

#[async_trait]
pub trait VectorStore: Send + Sync {
    async fn upsert(&self, db: &Database, owner_id: i64, model: &str, dim: usize, embedding: &[f32]) -> Result<()>;
    async fn knn(&self, db: &Database, project: Option<&str>, query: &[f32], model: &str, k: usize) -> Result<Vec<(i64, f32)>>;
    /// Remove all vectors (canonical + ANN) for a memory id. Used by memory deletion (spec §7).
    async fn delete(&self, db: &Database, owner_id: i64) -> Result<()>;
}

pub struct BruteForceStore;

#[async_trait]
impl VectorStore for BruteForceStore {
    async fn upsert(&self, db: &Database, owner_id: i64, model: &str, dim: usize, embedding: &[f32]) -> Result<()> {
        crate::db::upsert_embedding(db, "memory", owner_id, model, dim as i64, &crate::embedding_codec::encode(embedding)).await
    }
    async fn delete(&self, db: &Database, owner_id: i64) -> Result<()> {
        sqlx::query("DELETE FROM embeddings WHERE owner_type='memory' AND owner_id=$1")
            .bind(owner_id).execute(&db.pool).await?;
        Ok(())
    }
    async fn knn(&self, db: &Database, project: Option<&str>, query: &[f32], model: &str, k: usize) -> Result<Vec<(i64, f32)>> {
        // join memories for the project filter
        let id_col = match db.backend { Backend::Sqlite => "m.rowid", Backend::Postgres => "m.id" };
        let mut sql = format!(
            "SELECT e.owner_id AS id, e.embedding AS embedding FROM embeddings e
             JOIN memories m ON {id_col} = e.owner_id
             WHERE e.owner_type='memory' AND e.model=$1");
        if project.is_some() { sql.push_str(" AND m.project=$2"); }
        let mut q = sqlx::query(&sql).bind(model);
        if let Some(p) = project { q = q.bind(p); }
        let rows = q.fetch_all(&db.pool).await?;
        let mut scored: Vec<(i64, f32)> = rows.into_iter().map(|r| {
            let id = r.get::<i64,_>("id");
            let v = decode(&r.get::<Vec<u8>,_>("embedding"));
            (id, dot(query, &v))
        }).collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(k);
        Ok(scored)
    }
}
```

- [ ] **Step 4: Run → PASS.** **Step 5: Commit** → `"feat: VectorStore trait + BruteForceStore"`

### Task 13: `SqliteVecStore`

**Files:** Modify `src/vectorstore.rs`

- [ ] **Step 1: Write failing test** (temp sqlite): `ensure_ann(dim)`, upsert via `SqliteVecStore` (writes canonical + `vec_memories`), `knn` returns nearest. Over-fetch then project-join.
- [ ] **Step 2: Run → FAIL.**
- [ ] **Step 3: Implement.** `upsert` writes canonical `embeddings` AND `INSERT OR REPLACE INTO vec_memories(memory_id, embedding) VALUES(?, ?)` (bind the f32 blob; confirm sqlite-vec accepts a raw f32 blob in `vec0` insert/MATCH — if it requires `vec_f32(json)`, bind JSON text instead). `knn` over-fetches `k*8` from `vec_memories MATCH`, then filters by `project` via a join on `memories.rowid`, truncates to `k`, converts `distance`→similarity `1.0 - distance`. `delete` removes both the canonical `embeddings` row and the `vec_memories` row.
- [ ] **Step 4: Run → PASS.** **Step 5: Commit** → `"feat: SqliteVecStore (vec0 ANN)"`

### Task 14: `PgVectorStore` (gated test)

**Files:** Modify `src/vectorstore.rs`

- [ ] **Step 1: Implement.** `upsert` writes canonical + `INSERT INTO memory_embeddings(memory_id, embedding) VALUES($1, $2::vector) ON CONFLICT(memory_id) DO UPDATE ...`, binding the embedding as a pgvector **text literal** `"[f1,f2,...]"` (avoids needing the pgvector crate under `Any`). `knn`: `SELECT memory_id, embedding <=> $1::vector AS distance FROM memory_embeddings [JOIN memories for project] ORDER BY distance LIMIT $k`; similarity = `1.0 - distance`. `delete` removes the canonical row and the `memory_embeddings` row. (Assert in the gated test that the `text::vector` cast resolves — pgvector can be strict about whitespace in the `[..]` literal.)
- [ ] **Step 2: Test** behind `#[cfg_attr(not(env DATABASE_URL), ignore)]` style gate — only runs when a Postgres `DATABASE_URL` is set; otherwise `cargo build` confirms compilation.
- [ ] **Step 3: Commit** → `"feat: PgVectorStore (pgvector text-literal binding)"`

### Task 15: `make_vector_store` selection

**Files:** Modify `src/vectorstore.rs`

- [ ] **Step 1: Implement** `pub async fn make_vector_store(db: &Database, dim: usize) -> Box<dyn VectorStore>`: call `db.ensure_ann(dim)`; if it succeeds use `SqliteVecStore`/`PgVectorStore` by backend; on error (e.g. pgvector missing) log once and return `BruteForceStore`. **Step 2:** `cargo test vectorstore::` → PASS. **Step 3: Commit** → `"feat: vector store selection w/ brute-force fallback"`

---

## Chunk 5: Retrieval & context

### Task 16: RRF hybrid search

**Files:**
- Create: `src/retrieval.rs`
- Modify: `src/main.rs` (`mod retrieval;`)

- [ ] **Step 1: Write failing test** for the pure-function fuser: given two ranked id-lists, `rrf_fuse` returns ids ordered by `Σ 1/(60+rank)`.

```rust
#[test]
fn rrf_fuses_by_reciprocal_rank() {
    let fts = vec![1_i64, 2, 3];
    let vec = vec![3_i64, 1, 4];
    let fused = rrf_fuse(&[fts, vec], 60);
    assert_eq!(fused[0], 1); // appears high in both
    assert!(fused.contains(&4));
}
```

- [ ] **Step 2: Run → FAIL.**
- [ ] **Step 3: Implement** `rrf_fuse(lists, k) -> Vec<i64>` (HashMap accumulate `1.0/(k+rank) as f64`, sort desc), then `hybrid_search(db, embedder, store, project, query, limit)` that: runs existing `db::search_memories`/`search_all_memories` → id list; if `embedder` present, embeds query and runs `store.knn` → id list; fuses; loads the `Memory` rows by id preserving fused order; falls back to pure FTS output when the vec list is empty.
- [ ] **Step 4: Run → PASS.** **Step 5: Commit** → `"feat: RRF hybrid search"`

### Task 17: Blended injection score

**Files:** Modify `src/retrieval.rs`

- [ ] **Step 1: Write failing tests** for `recency_weight(age_secs, half_life_days)` (==1.0 at age 0; ==0.5 at one half-life; →0 at large age) and `blended_score(rel, rec, imp, weights)` (linear combo).
- [ ] **Step 2: Run → FAIL.**
- [ ] **Step 3: Implement** the helpers and `injection_rank(db, embedder, store, project, query_vec: Option<&[f32]>, weights, half_life_days, limit) -> Vec<Memory>`. Use the **true half-life** form so it matches the Step 1 test:

```rust
pub fn recency_weight(age_secs: f64, half_life_days: f64) -> f64 {
    0.5_f64.powf(age_secs / (half_life_days * 86_400.0)) // 1.0 at age 0, 0.5 at one half-life
}
pub fn blended_score(rel: f64, rec: f64, imp: f64, w: &Weights) -> f64 {
    w.relevance * rel + w.recency * rec + w.importance * imp
}
```

Then `injection_rank`: fetch candidate memories (recent window), compute relevance from `store.knn` similarities (0 when no `query_vec`), recency via `recency_weight`, importance from `get_memory_meta`; sort by `blended_score`; take `limit`. With no embedder/query_vec, `w_r` collapses to 0 and ranking == recency+importance.
- [ ] **Step 4: Run → PASS.** **Step 5: Commit** → `"feat: blended relevance+recency+importance injection ranking"`

### Task 18: `context.rs` git query signal

**Files:**
- Create: `src/context.rs`
- Modify: `src/main.rs` (`mod context;`)

- [ ] **Step 1: Write failing test:** `git_query("/nonexistent/path")` returns `None`.
- [ ] **Step 2: Run → FAIL.**
- [ ] **Step 3: Implement** `pub fn git_query(project: &str) -> Option<String>`: run `git -C <project> log -n 20 --format=%s`, `git -C <project> status --porcelain`, `git -C <project> diff --name-only HEAD` via `std::process::Command`; return `None` if not a git repo / git missing / all empty; otherwise concatenate (cap to ~2000 chars).
- [ ] **Step 4: Run → PASS.** **Step 5: Commit** → `"feat: git-derived session-start query signal"`

---

## Chunk 6: Importance extraction & shared compression

### Task 19: `IMPORTANCE` line in compression

**Files:** Modify `src/provider.rs`

- [ ] **Step 1: Write failing tests** for `parse_response`: input containing `IMPORTANCE: 8` yields `importance == 8`; missing line yields default `5`; out-of-range clamps to 1..=10.
- [ ] **Step 2: Run → FAIL** (field doesn't exist).
- [ ] **Step 3: Implement:** add `pub importance: u8` to `CompressionResult`; append the `IMPORTANCE: [1-10 ...]` instruction in `build_prompt`; parse the line in `parse_response` (default 5, clamp 1..=10).
- [ ] **Step 4: Run → PASS.** **Step 5: Commit** → `"feat: LLM importance score in compression output"`

### Task 20: Shared `compress::run` helper (+ embed + meta)

**Files:**
- Create: `src/compress.rs`
- Modify: `src/main.rs` (`mod compress;`), `src/mcp.rs` (`IronMemServer::run_compression` delegates), `src/server.rs` (`run_compression` delegates)

- [ ] **Step 1: Write failing test** (temp sqlite, `FakeEmbedder`, fake observations): `compress::run` inserts a memory, writes `memory_meta`, and upserts an embedding for it. (Stub the provider call by testing the post-LLM persistence path, or factor the LLM call behind a small trait so the test injects a canned `CompressionResult`.)
- [ ] **Step 2: Run → FAIL.**
- [ ] **Step 3: Implement** `pub async fn run(db, embedder: Option<&dyn Embedder>, store: &dyn VectorStore, cfg, session_id) -> Result<i64>`: replicate today's `run_compression` body (get session/observations, `provider::compress`, `insert_memory`, `mark_compressed`), then `upsert_memory_meta(id, importance/10.0)`, then **best-effort** embed `summary + " " + tags` and `store.upsert(id, ...)` (log+swallow on error). Replace BOTH `mcp.rs` and `server.rs` `run_compression` bodies with a call to `compress::run` (per spec §9 — fix both, no divergence).
- [ ] **Step 4: Run → PASS;** also `cargo test` whole suite green.
- [ ] **Step 5: Commit** → `"refactor: shared compress::run with importance + inline embedding"`

---

## Chunk 7: Config, surface wiring & backfill CLI

### Task 21: `embedding` config block

**Files:** Modify `src/config.rs`

- [ ] **Step 1: Write failing test:** deserializing settings JSON without an `embedding` key yields `EmbeddingConfig::default()` (`provider="auto"`, weights 0.5/0.3/0.2, half-life 30); a settings JSON with `provider="none"` round-trips.
- [ ] **Step 2: Run → FAIL.**
- [ ] **Step 3: Implement** `EmbeddingConfig` struct (with `#[serde(default)]` fields + `weights` sub-struct), add `#[serde(default)] pub embedding: EmbeddingConfig` to `Config`, defaults per §12. Keep all existing fields/behavior.
- [ ] **Step 4: Run → PASS.** **Step 5: Commit** → `"feat: embedding configuration block"`

### Task 22: Construct embedder + store at startup

**Files:** Modify `src/main.rs`, `src/mcp.rs`, `src/server.rs`

- [ ] **Step 1:** In `run_mcp`, `run_serve`, `run_server`: after building `db`, call `resolve_embedder(&cfg).await`; if `Some`, `make_vector_store(&db, embedder.dim()).await` and `db.ensure_ann(dim)`. Store `Option<Arc<dyn Embedder>>` + `Arc<dyn VectorStore>` in `IronMemServer` and REST `AppState` (add fields; brute-force default store when no embedder).
- [ ] **Step 2:** `cargo build` compiles; existing tests green. **Step 3: Commit** → `"feat: wire embedder + vector store into servers"`

### Task 23: Route search/context through `retrieval`

**Files:** Modify `src/mcp.rs`, `src/server.rs`

- [ ] **Step 1: Write failing test** (mcp-level, `FakeEmbedder`): seed memories + embeddings; `handle_search_memories` returns a semantically-near memory that pure FTS would miss. (If handler testing is heavy, test `retrieval::hybrid_search` directly with the server's wiring.)
- [ ] **Step 2: Run → FAIL.**
- [ ] **Step 3: Implement:** `handle_search_memories`/`handle_search_global`/`handle_get_context` (and REST `get_context`, `api_list_memories` with `query`) call `retrieval::hybrid_search`. Add optional `semantic: bool` (default true) tool arg to force FTS-only. No embedder → identical to today.
- [ ] **Step 4: Run → PASS.** **Step 5: Commit** → `"feat: hybrid search behind MCP/REST search + context"`

### Task 24: Relevance injection

**Files:** Modify `src/hooks.rs`, `src/main.rs` (`run_inject`), `src/mcp.rs` (`handle_inject_context`)

- [ ] **Step 1: Write failing test:** with embeddings + a query vector, injection order is by blended score, not raw recency.
- [ ] **Step 2: Run → FAIL.**
- [ ] **Step 3: Implement:** in the inject path, compute `context::git_query(project)` → embed (if embedder) → `retrieval::injection_rank(...)`; write `IRONMEM.md` from the ranked list. Pass `cfg.embedding.weights` and `cfg.embedding.recency_half_life_days` into `injection_rank` (do not hardcode). No git/embedder → existing `get_recent_memories` recency path.
- [ ] **Step 4: Run → PASS.** **Step 5: Commit** → `"feat: relevance-ranked session-start injection"`

### Task 24b: Wire vector cleanup into memory deletion

**Files:**
- Modify: `src/db.rs` (`delete_memory`, `delete_memories_for_project`)
- Modify: `src/mcp.rs` (`handle_wipe_project`), `src/server.rs` (`api_delete_memory`)

- [ ] **Step 1: Write failing test** (temp sqlite, `FakeEmbedder`): insert a memory + embedding (+ `memory_meta`); delete the memory through the deletion path; assert no `embeddings` / `vec_memories` / `memory_meta` row remains for that id.
- [ ] **Step 2: Run → FAIL** (orphaned vector remains).
- [ ] **Step 3: Implement:** after a memory row is deleted, call `store.delete(db, memory_id)` and delete its `memory_meta` row. For `delete_memories_for_project` / `wipe_project`, collect the affected memory ids first, delete the memory rows, then `store.delete` each id and clear their `memory_meta`. The `store` is already held by the servers (Task 22); thread it into these call sites.
- [ ] **Step 4: Run → PASS.**
- [ ] **Step 5: Commit** → `"feat: clean up vectors + meta on memory deletion"`

### Task 25: `ironmem embed` backfill subcommand

**Files:** Modify `src/main.rs`

- [ ] **Step 1: Write failing test** (temp sqlite, `FakeEmbedder`): insert 2 memories without vectors; run the backfill function; both now have embeddings; running again is a no-op (idempotent); `--force` re-embeds.
- [ ] **Step 2: Run → FAIL.**
- [ ] **Step 3: Implement** `Commands::Embed { project, all, force }` and `run_embed(cfg, project, all, force)`: resolve embedder (error clearly if none), `db.ensure_ann(dim)`; if `force`, `db.drop_ann()` + re-`ensure_ann` and clear `embeddings` for the model; gather ids via `memory_ids_missing_embedding` (or all if force), embed in batches of 64, `store.upsert` each, write `memory_meta` default if missing, print progress + final count, continue past per-item errors.
- [ ] **Step 4: Run → PASS.** **Step 5: Commit** → `"feat: ironmem embed backfill command"`

---

## Chunk 8: Integration, docs & release

### Task 26: End-to-end integration test

**Files:** Create `tests/semantic_e2e.rs` (or inline integration module)

- [ ] **Step 1: Write the test** (temp sqlite, `FakeEmbedder` exposed via a test-only constructor or a `#[cfg(test)]` injection seam): create session → observations → `compress::run` → assert memory + meta + embedding exist → `hybrid_search` finds it → `injection_rank` orders it → `provider="none"` path equals legacy FTS/recency.
- [ ] **Step 2: Run → PASS** (`cargo test --test semantic_e2e`).
- [ ] **Step 3: Commit** → `"test: end-to-end semantic retrieval"`

### Task 27: Documentation

**Files:** Modify `README.md`, `CHANGELOG.md`

- [ ] **Step 1:** Document the `embedding` config block, the local-default/no-egress posture, Ollama setup (`ollama pull nomic-embed-text`), the `--features local-onnx` build option, and `ironmem embed` backfill. Add a CHANGELOG entry. **Do not** commit secrets or machine-specific paths (use `~`/placeholders) per repo policy.
- [ ] **Step 2: Commit** → `"docs: document semantic retrieval + embedding config"`

### Task 28: Full verification & release build

- [ ] **Step 1:** `cargo test` (all green, default features) and `cargo test --features local-onnx`.
- [ ] **Step 2:** `cargo clippy --all-targets -- -D warnings` (matches the repo's clippy-clean history).
- [ ] **Step 3:** `cargo build --release` once to confirm the release profile links sqlite-vec/fastembed cleanly; install to `~/.ironmem/bin/` only for final manual verification.
- [ ] **Step 4: Final commit** if clippy/docs touched → `"chore: clippy clean + release build verification"`

---

## Verification Checklist (Definition of Done)

- [ ] `vec0` smoke test passes (Task 3) — sqlite-vec links correctly.
- [ ] All unit + integration tests green on default features; `--features local-onnx` builds and tests.
- [ ] `clippy -D warnings` clean.
- [ ] `embedding.provider="none"` reproduces today's exact behavior (FTS search, recency injection).
- [ ] No embedder running (Ollama down, no key) never hard-fails any command.
- [ ] Both `run_compression` call sites (mcp.rs, server.rs) go through `compress::run` — no divergence.
- [ ] `ironmem embed` backfills existing memories idempotently; `--force` re-indexes after a model/dim change.
- [ ] Deleting a memory (single delete or wipe-project) removes its vectors + `memory_meta` — no orphaned rows.
- [ ] No `TODO`/`unimplemented!()`/placeholder in committed code.

## Notes / locked decisions
- Default embedding model: `nomic-embed-text` (768d) via Ollama (spec §18 Q1). Configurable.
- Cross-encoder reranking: out of scope here, lives in Piece 6 (spec §18 Q2).
- pgvector bound as a text literal (`'[...]'::vector`) to work under `sqlx::Any` without a PgPool-specific type.
