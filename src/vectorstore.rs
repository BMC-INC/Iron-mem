//! Vector storage + ANN query. The canonical embedding always lives in the
//! `embeddings` table; each backend also syncs a real ANN index (vec0 /
//! pgvector). `BruteForceStore` is the genuine no-extension fallback.

use anyhow::Result;
use async_trait::async_trait;
use sqlx::Row;
use std::sync::Arc;

use crate::config::Config;
use crate::db::{self, Backend, Database};
use crate::embedder::{resolve_embedder, Embedder};
use crate::embedding_codec::{decode, dot, encode};

/// How many ANN candidates to pull before applying the project filter.
const OVERFETCH: i64 = 8;

#[async_trait]
pub trait VectorStore: Send + Sync {
    async fn upsert(
        &self,
        db: &Database,
        owner_id: i64,
        model: &str,
        dim: usize,
        embedding: &[f32],
    ) -> Result<()>;

    /// Returns (memory_id, similarity in 0..1) for the top `k` nearest.
    async fn knn(
        &self,
        db: &Database,
        project: Option<&str>,
        query: &[f32],
        model: &str,
        k: usize,
    ) -> Result<Vec<(i64, f32)>>;

    /// Remove all vectors (canonical + ANN) for a memory id.
    async fn delete(&self, db: &Database, owner_id: i64) -> Result<()>;
}

// ── Brute force (no extension; exact cosine in Rust) ────────────────

pub struct BruteForceStore;

#[async_trait]
impl VectorStore for BruteForceStore {
    async fn upsert(
        &self,
        db: &Database,
        owner_id: i64,
        model: &str,
        dim: usize,
        embedding: &[f32],
    ) -> Result<()> {
        db::upsert_embedding(db, "memory", owner_id, model, dim as i64, &encode(embedding)).await
    }

    async fn knn(
        &self,
        db: &Database,
        project: Option<&str>,
        query: &[f32],
        model: &str,
        k: usize,
    ) -> Result<Vec<(i64, f32)>> {
        let id_col = match db.backend {
            Backend::Sqlite => "m.rowid",
            Backend::Postgres => "m.id",
        };
        let mut sql = format!(
            "SELECT e.owner_id AS id, e.embedding AS embedding
             FROM embeddings e JOIN memories m ON {id_col} = e.owner_id
             WHERE e.owner_type = 'memory' AND e.model = $1"
        );
        if project.is_some() {
            sql.push_str(" AND m.project = $2");
        }
        let mut q = sqlx::query(&sql).bind(model);
        if let Some(p) = project {
            q = q.bind(p);
        }
        let rows = q.fetch_all(&db.pool).await?;
        let mut scored: Vec<(i64, f32)> = rows
            .into_iter()
            .map(|r| {
                let id = r.get::<i64, _>("id");
                let v = decode(&r.get::<Vec<u8>, _>("embedding"));
                (id, dot(query, &v))
            })
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(k);
        Ok(scored)
    }

    async fn delete(&self, db: &Database, owner_id: i64) -> Result<()> {
        db::delete_embedding(db, "memory", owner_id).await
    }
}

// ── SQLite: sqlite-vec vec0 ANN ─────────────────────────────────────

pub struct SqliteVecStore;

#[async_trait]
impl VectorStore for SqliteVecStore {
    async fn upsert(
        &self,
        db: &Database,
        owner_id: i64,
        model: &str,
        dim: usize,
        embedding: &[f32],
    ) -> Result<()> {
        db::upsert_embedding(db, "memory", owner_id, model, dim as i64, &encode(embedding)).await?;
        sqlx::query("INSERT OR REPLACE INTO vec_memories(memory_id, embedding) VALUES ($1, $2)")
            .bind(owner_id)
            .bind(encode(embedding))
            .execute(&db.pool)
            .await?;
        Ok(())
    }

    async fn knn(
        &self,
        db: &Database,
        project: Option<&str>,
        query: &[f32],
        _model: &str,
        k: usize,
    ) -> Result<Vec<(i64, f32)>> {
        let qblob = encode(query);
        let rows: Vec<sqlx::any::AnyRow> = if let Some(p) = project {
            sqlx::query(
                "SELECT v.memory_id AS id, v.distance AS distance
                 FROM (SELECT memory_id, distance FROM vec_memories
                       WHERE embedding MATCH $1 AND k = $2 ORDER BY distance) v
                 JOIN memories m ON m.rowid = v.memory_id
                 WHERE m.project = $3
                 ORDER BY v.distance LIMIT $4",
            )
            .bind(qblob)
            .bind(k as i64 * OVERFETCH)
            .bind(p)
            .bind(k as i64)
            .fetch_all(&db.pool)
            .await?
        } else {
            sqlx::query(
                "SELECT memory_id AS id, distance FROM vec_memories
                 WHERE embedding MATCH $1 AND k = $2 ORDER BY distance",
            )
            .bind(qblob)
            .bind(k as i64)
            .fetch_all(&db.pool)
            .await?
        };
        Ok(rows
            .into_iter()
            .map(|r| {
                let id = r.get::<i64, _>("id");
                let dist = r.get::<f64, _>("distance") as f32;
                (id, 1.0 - dist) // cosine metric ⇒ similarity = 1 - distance
            })
            .collect())
    }

    async fn delete(&self, db: &Database, owner_id: i64) -> Result<()> {
        db::delete_embedding(db, "memory", owner_id).await?;
        sqlx::query("DELETE FROM vec_memories WHERE memory_id = $1")
            .bind(owner_id)
            .execute(&db.pool)
            .await?;
        Ok(())
    }
}

// ── Postgres: pgvector ANN ──────────────────────────────────────────

pub struct PgVectorStore;

fn pg_literal(v: &[f32]) -> String {
    let mut s = String::with_capacity(v.len() * 8);
    s.push('[');
    for (i, x) in v.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&x.to_string());
    }
    s.push(']');
    s
}

#[async_trait]
impl VectorStore for PgVectorStore {
    async fn upsert(
        &self,
        db: &Database,
        owner_id: i64,
        model: &str,
        dim: usize,
        embedding: &[f32],
    ) -> Result<()> {
        db::upsert_embedding(db, "memory", owner_id, model, dim as i64, &encode(embedding)).await?;
        sqlx::query(
            "INSERT INTO memory_embeddings(memory_id, embedding) VALUES ($1, $2::vector)
             ON CONFLICT(memory_id) DO UPDATE SET embedding = excluded.embedding",
        )
        .bind(owner_id)
        .bind(pg_literal(embedding))
        .execute(&db.pool)
        .await?;
        Ok(())
    }

    async fn knn(
        &self,
        db: &Database,
        project: Option<&str>,
        query: &[f32],
        _model: &str,
        k: usize,
    ) -> Result<Vec<(i64, f32)>> {
        let lit = pg_literal(query);
        let rows: Vec<sqlx::any::AnyRow> = if let Some(p) = project {
            sqlx::query(
                "SELECT me.memory_id AS id, (me.embedding <=> $1::vector) AS distance
                 FROM memory_embeddings me JOIN memories m ON m.id = me.memory_id
                 WHERE m.project = $2
                 ORDER BY distance LIMIT $3",
            )
            .bind(lit)
            .bind(p)
            .bind(k as i64)
            .fetch_all(&db.pool)
            .await?
        } else {
            sqlx::query(
                "SELECT memory_id AS id, (embedding <=> $1::vector) AS distance
                 FROM memory_embeddings ORDER BY distance LIMIT $2",
            )
            .bind(lit)
            .bind(k as i64)
            .fetch_all(&db.pool)
            .await?
        };
        Ok(rows
            .into_iter()
            .map(|r| {
                let id = r.get::<i64, _>("id");
                let dist = r.get::<f64, _>("distance") as f32;
                (id, 1.0 - dist)
            })
            .collect())
    }

    async fn delete(&self, db: &Database, owner_id: i64) -> Result<()> {
        db::delete_embedding(db, "memory", owner_id).await?;
        sqlx::query("DELETE FROM memory_embeddings WHERE memory_id = $1")
            .bind(owner_id)
            .execute(&db.pool)
            .await?;
        Ok(())
    }
}

/// How many texts to embed per provider call during backfill.
const BACKFILL_BATCH: usize = 64;

/// Backfill embeddings for memories lacking them (or all of them when `force`).
/// Returns the number of memories embedded. Per-item failures are logged and
/// skipped so one bad row never aborts the pass.
pub async fn backfill(
    db: &Database,
    embedder: &dyn Embedder,
    store: &dyn VectorStore,
    project: Option<&str>,
    force: bool,
) -> Result<usize> {
    let model = embedder.id().to_string();
    let dim = embedder.dim();

    if force {
        db.drop_ann().await.ok();
        db.ensure_ann(dim).await.ok();
        db::clear_embeddings_for_model(db, &model).await?;
    } else {
        db.ensure_ann(dim).await.ok();
    }

    let targets = if force {
        db::all_memory_ids_with_text(db, project).await?
    } else {
        db::memory_ids_missing_embedding(db, &model, project).await?
    };

    let mut embedded = 0usize;
    for chunk in targets.chunks(BACKFILL_BATCH) {
        let texts: Vec<String> = chunk
            .iter()
            .map(|(_, summary, tags)| match tags {
                Some(t) => format!("{summary} {t}"),
                None => summary.clone(),
            })
            .collect();
        let vectors = match embedder.embed(&texts).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("embed batch failed ({} items): {e}", chunk.len());
                continue;
            }
        };
        for ((id, _, _), vec) in chunk.iter().zip(vectors) {
            if let Err(e) = store.upsert(db, *id, &model, dim, &vec).await {
                tracing::warn!("store upsert failed (memory {id}): {e}");
                continue;
            }
            let _ = db::ensure_memory_meta(db, *id, 0.5).await;
            embedded += 1;
        }
    }
    Ok(embedded)
}

/// Remove a memory's vectors (canonical + ANN) and its metadata row. Call
/// after the `memories` row itself has been deleted so nothing dangles.
pub async fn purge_memory(db: &Database, store: &dyn VectorStore, memory_id: i64) -> Result<()> {
    store.delete(db, memory_id).await?;
    db::delete_memory_meta(db, memory_id).await?;
    db::delete_memory_entities(db, memory_id).await?;
    db::delete_memory_edges(db, memory_id).await?;
    db::delete_memory_chunks(db, memory_id).await?;
    Ok(())
}

/// Resolve the configured embedder (if any) and the matching vector store in
/// one step. With no embedder (provider="none" or nothing reachable) the store
/// is brute-force and search degrades to FTS — never an error.
pub async fn build_semantic(
    db: &Database,
    cfg: &Config,
) -> (Option<Arc<dyn Embedder>>, Arc<dyn VectorStore>) {
    match resolve_embedder(cfg).await {
        Some(embedder) => {
            let store = make_vector_store(db, embedder.dim()).await;
            (Some(embedder), store)
        }
        None => (None, Arc::new(BruteForceStore)),
    }
}

/// Pick the best vector store for the backend, ensuring the ANN index exists.
/// Falls back to brute force if the ANN structure can't be created.
pub async fn make_vector_store(db: &Database, dim: usize) -> Arc<dyn VectorStore> {
    match db.ensure_ann(dim).await {
        Ok(()) => match db.backend {
            Backend::Sqlite => Arc::new(SqliteVecStore),
            Backend::Postgres => Arc::new(PgVectorStore),
        },
        Err(e) => {
            tracing::warn!("ANN index unavailable ({}); using brute-force vector search", e);
            Arc::new(BruteForceStore)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{create_session, insert_memory};
    use crate::embedding_codec::normalize;

    async fn seed(db: &Database) {
        let s = create_session(db, "/tmp/p").await.unwrap();
        // rowids 1,2,3
        insert_memory(db, "/tmp/p", &s, "alpha auth", Some("a")).await.unwrap();
        insert_memory(db, "/tmp/p", &s, "beta search", Some("b")).await.unwrap();
        insert_memory(db, "/tmp/p", &s, "gamma db", Some("c")).await.unwrap();
    }

    fn vecs() -> [Vec<f32>; 3] {
        [
            normalize(&[1.0, 0.0, 0.0, 0.0]),
            normalize(&[0.0, 1.0, 0.0, 0.0]),
            normalize(&[0.0, 0.0, 1.0, 0.0]),
        ]
    }

    #[tokio::test]
    async fn brute_force_knn_orders_by_cosine() {
        let path = std::env::temp_dir().join(format!("ironmem-bf-{}.db", uuid::Uuid::new_v4()));
        let db = Database::new(&path.to_string_lossy()).await.unwrap();
        db.migrate().await.unwrap();
        seed(&db).await;
        let store = BruteForceStore;
        for (i, v) in vecs().iter().enumerate() {
            store.upsert(&db, (i + 1) as i64, "m", 4, v).await.unwrap();
        }
        let q = normalize(&[0.9, 0.1, 0.0, 0.0]);
        let res = store.knn(&db, None, &q, "m", 2).await.unwrap();
        assert_eq!(res[0].0, 1); // nearest is memory 1
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn backfill_embeds_missing_then_is_idempotent() {
        let path = std::env::temp_dir().join(format!("ironmem-bf2-{}.db", uuid::Uuid::new_v4()));
        let db = Database::new(&path.to_string_lossy()).await.unwrap();
        db.migrate().await.unwrap();
        let s = create_session(&db, "/tmp/p").await.unwrap();
        insert_memory(&db, "/tmp/p", &s, "alpha auth", Some("a")).await.unwrap();
        insert_memory(&db, "/tmp/p", &s, "beta search", Some("b")).await.unwrap();

        let emb = crate::embedder::FakeEmbedder::new(8);
        let store = make_vector_store(&db, 8).await;

        // First pass embeds both.
        let n = backfill(&db, &emb, store.as_ref(), Some("/tmp/p"), false).await.unwrap();
        assert_eq!(n, 2);
        assert!(db::get_embedding(&db, "memory", 1, emb.id()).await.unwrap().is_some());
        assert!(db::get_embedding(&db, "memory", 2, emb.id()).await.unwrap().is_some());

        // Second pass is a no-op (nothing missing).
        let n2 = backfill(&db, &emb, store.as_ref(), Some("/tmp/p"), false).await.unwrap();
        assert_eq!(n2, 0);

        // Force re-embeds everything.
        let n3 = backfill(&db, &emb, store.as_ref(), Some("/tmp/p"), true).await.unwrap();
        assert_eq!(n3, 2);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn purge_memory_removes_vectors_and_meta() {
        let path = std::env::temp_dir().join(format!("ironmem-purge-{}.db", uuid::Uuid::new_v4()));
        let db = Database::new(&path.to_string_lossy()).await.unwrap();
        db.migrate().await.unwrap();
        db.ensure_ann(4).await.unwrap();
        seed(&db).await;
        let store = SqliteVecStore;
        store.upsert(&db, 1, "m", 4, &vecs()[0]).await.unwrap();
        db::upsert_memory_meta(&db, 1, 0.7).await.unwrap();
        db::insert_memory_entity(&db, 1, "Caroline").await.unwrap();

        // Delete the memory row, then purge its vectors + metadata + entity rows.
        assert!(db::delete_memory(&db, 1).await.unwrap());
        purge_memory(&db, &store, 1).await.unwrap();

        assert!(db::get_embedding(&db, "memory", 1, "m").await.unwrap().is_none());
        // Entity-index rows for the memory are physically gone (checked directly,
        // not via the memories JOIN which already excludes the deleted row).
        let ent_rows: i64 =
            sqlx::query("SELECT COUNT(*) AS c FROM memory_entities WHERE memory_id = 1")
                .fetch_one(&db.pool)
                .await
                .unwrap()
                .get("c");
        assert_eq!(ent_rows, 0, "purge must remove entity-index rows");
        // meta back to default (no row) ⇒ 0.5
        assert!((db::get_memory_meta(&db, 1).await.unwrap() - 0.5).abs() < 1e-9);
        // ANN row gone too.
        let remaining: i64 =
            sqlx::query("SELECT COUNT(*) AS c FROM vec_memories WHERE memory_id = 1")
                .fetch_one(&db.pool)
                .await
                .unwrap()
                .get("c");
        assert_eq!(remaining, 0);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn sqlitevec_knn_orders_by_cosine_and_deletes() {
        let path = std::env::temp_dir().join(format!("ironmem-sv-{}.db", uuid::Uuid::new_v4()));
        let db = Database::new(&path.to_string_lossy()).await.unwrap();
        db.migrate().await.unwrap();
        db.ensure_ann(4).await.unwrap();
        seed(&db).await;
        let store = SqliteVecStore;
        for (i, v) in vecs().iter().enumerate() {
            store.upsert(&db, (i + 1) as i64, "m", 4, v).await.unwrap();
        }
        let q = normalize(&[0.0, 0.0, 0.95, 0.05]);
        let res = store.knn(&db, Some("/tmp/p"), &q, "m", 2).await.unwrap();
        assert_eq!(res[0].0, 3); // nearest is memory 3
        assert!(res[0].1 > 0.8); // cosine similarity high

        store.delete(&db, 3).await.unwrap();
        let res2 = store.knn(&db, Some("/tmp/p"), &q, "m", 3).await.unwrap();
        assert!(!res2.iter().any(|(id, _)| *id == 3));
        let _ = std::fs::remove_file(path);
    }
}
