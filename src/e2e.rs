//! End-to-end semantic pipeline test. Drives the real persistence + retrieval
//! path with the in-crate `FakeEmbedder` (deterministic, no network): compress
//! → persist (memory + importance meta + embedding) → hybrid search → blended
//! injection ranking, and confirms the no-embedder path stays pure-FTS.
//!
//! Lives inline (not under `tests/`) because ironmem is a binary crate without
//! a library target, and `FakeEmbedder` is `#[cfg(test)]`-only.

#![cfg(test)]

use crate::compress;
use crate::config::Weights;
use crate::db::{self, create_session, Database};
use crate::embedder::{Embedder, FakeEmbedder};
use crate::provider::CompressionResult;
use crate::retrieval;
use crate::vectorstore::{BruteForceStore, SqliteVecStore};

async fn tmp_db() -> (Database, std::path::PathBuf) {
    let path = std::env::temp_dir().join(format!("ironmem-e2e-{}.db", uuid::Uuid::new_v4()));
    let db = Database::new(&path.to_string_lossy()).await.unwrap();
    db.migrate().await.unwrap();
    db.ensure_ann(8).await.unwrap();
    (db, path)
}

#[tokio::test]
async fn semantic_pipeline_end_to_end() {
    let (db, path) = tmp_db().await;
    let project = "/tmp/e2e-proj";
    let session = create_session(&db, project).await.unwrap();

    let emb = FakeEmbedder::new(8);
    let store = SqliteVecStore;

    // Compress two sessions into memories via the network-free persist seam.
    let target = compress::persist(
        &db,
        Some(&emb),
        &store,
        project,
        &session,
        CompressionResult {
            summary: "implemented sqlite-vec ANN search".into(),
            tags: "rust sqlite vector ann".into(),
            importance: 9,
        },
    )
    .await
    .unwrap();

    let distractor = compress::persist(
        &db,
        Some(&emb),
        &store,
        project,
        &session,
        CompressionResult {
            summary: "tweaked frontend css layout".into(),
            tags: "css frontend layout".into(),
            importance: 2,
        },
    )
    .await
    .unwrap();

    // 1. Persistence: memory + importance meta + embedding all written.
    assert!(db::get_memory_by_id(&db, target).await.unwrap().is_some());
    assert!((db::get_memory_meta(&db, target).await.unwrap() - 0.9).abs() < 1e-9);
    assert!((db::get_memory_meta(&db, distractor).await.unwrap() - 0.2).abs() < 1e-9);
    assert!(db::get_embedding(&db, "memory", target, emb.id())
        .await
        .unwrap()
        .is_some());

    // 2. Hybrid search surfaces the target.
    let hits = retrieval::hybrid_search(&db, Some(&emb), &store, Some(project), "sqlite", 5)
        .await
        .unwrap();
    assert!(hits.iter().any(|m| m.id == target));

    // 3. Injection ranking puts the semantically-near, high-importance memory first.
    let qv = emb.embed(&["rust sqlite vector ann".into()]).await.unwrap();
    let ranked = retrieval::injection_rank(
        &db,
        Some(&emb),
        &store,
        project,
        Some(&qv[0]),
        &Weights::default(),
        30.0,
        5,
    )
    .await
    .unwrap();
    assert_eq!(ranked[0].id, target);

    // 4. No-embedder path == legacy FTS: still finds the keyword match, never errors.
    let fts_only =
        retrieval::hybrid_search(&db, None, &BruteForceStore, Some(project), "sqlite", 5)
            .await
            .unwrap();
    assert!(fts_only.iter().any(|m| m.id == target));
    assert!(!fts_only.iter().any(|m| m.id == distractor));

    let _ = std::fs::remove_file(path);
}
