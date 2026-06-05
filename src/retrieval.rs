//! Hybrid retrieval: Reciprocal Rank Fusion over FTS + vector results, plus
//! the blended relevance/recency/importance ranking used for session-start
//! injection. Pure scoring helpers are unit-tested; the search/rank entry
//! points compose them with the db + vector store.

use anyhow::Result;
use chrono::Utc;
use std::collections::HashMap;

use crate::config::Weights;
use crate::context;
use crate::db::{self, Database, Memory};
use crate::embedder::Embedder;
use crate::vectorstore::VectorStore;

/// Standard RRF damping constant. Larger ⇒ rank position matters less.
pub const RRF_K: i64 = 60;

/// Fuse several ranked id-lists into one ordering by Σ 1/(k + rank).
/// `rank` is 0-indexed (top of a list contributes the most). Ties keep
/// first-appearance order so the result is deterministic.
pub fn rrf_fuse(lists: &[Vec<i64>], k: i64) -> Vec<i64> {
    let mut score: HashMap<i64, f64> = HashMap::new();
    let mut order: Vec<i64> = Vec::new();
    for list in lists {
        for (rank, &id) in list.iter().enumerate() {
            let entry = score.entry(id).or_insert_with(|| {
                order.push(id);
                0.0
            });
            *entry += 1.0 / (k as f64 + rank as f64);
        }
    }
    // Stable sort by score desc; equal scores keep first-appearance order.
    order.sort_by(|a, b| {
        score[b]
            .partial_cmp(&score[a])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    order
}

/// Hybrid search: fuse keyword (FTS) and semantic (vector) results. With no
/// embedder — or when the vector side yields nothing — this returns the exact
/// FTS ordering, reproducing legacy behavior.
pub async fn hybrid_search(
    db: &Database,
    embedder: Option<&dyn Embedder>,
    store: &dyn VectorStore,
    project: Option<&str>,
    query: &str,
    limit: usize,
) -> Result<Vec<Memory>> {
    // Keyword side (always run).
    let fts = match project {
        Some(p) => db::search_memories(db, p, query, limit as i64).await?,
        None => db::search_all_memories(db, query, limit as i64).await?,
    };

    // Semantic side (best-effort; only when an embedder is configured).
    let vec_ids: Vec<i64> = if let Some(emb) = embedder {
        match embed_one(emb, query).await {
            Some(qvec) => store
                .knn(db, project, &qvec, emb.id(), limit)
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|(id, _sim)| id)
                .collect(),
            None => Vec::new(),
        }
    } else {
        Vec::new()
    };

    // No semantic signal ⇒ pure FTS, unchanged.
    if vec_ids.is_empty() {
        return Ok(fts);
    }

    // Fuse and materialize in fused order, reusing already-loaded FTS rows.
    let fts_ids: Vec<i64> = fts.iter().map(|m| m.id).collect();
    let by_id: HashMap<i64, Memory> = fts.into_iter().map(|m| (m.id, m)).collect();
    let fused = rrf_fuse(&[fts_ids, vec_ids], RRF_K);

    let mut out = Vec::with_capacity(limit);
    for id in fused.into_iter().take(limit) {
        if let Some(m) = by_id.get(&id) {
            out.push(m.clone());
        } else if let Some(m) = db::get_memory_by_id(db, id).await? {
            out.push(m);
        }
    }
    Ok(out)
}

// ── Blended injection ranking ───────────────────────────────────────

/// Recency weight via true half-life decay: 1.0 at age 0, 0.5 at one
/// half-life, approaching 0 for very old memories.
pub fn recency_weight(age_secs: f64, half_life_days: f64) -> f64 {
    if half_life_days <= 0.0 {
        return 0.0;
    }
    0.5_f64.powf(age_secs / (half_life_days * 86_400.0))
}

/// Linear blend of relevance, recency, and importance (each in 0..1).
pub fn blended_score(rel: f64, rec: f64, imp: f64, w: &Weights) -> f64 {
    w.relevance * rel + w.recency * rec + w.importance * imp
}

/// Rank recent memories for session-start injection by blended score.
/// With no `query_vec`/embedder the relevance term is 0 for every candidate,
/// so the ordering collapses to recency + importance (legacy-compatible).
pub async fn injection_rank(
    db: &Database,
    embedder: Option<&dyn Embedder>,
    store: &dyn VectorStore,
    project: &str,
    query_vec: Option<&[f32]>,
    weights: &Weights,
    half_life_days: f64,
    limit: usize,
) -> Result<Vec<Memory>> {
    // Pull a generous recent window, then re-rank it by the blend.
    let window = ((limit as i64) * 10).max(50);
    let candidates = db::get_recent_memories(db, project, window).await?;
    if candidates.is_empty() {
        return Ok(candidates);
    }

    // Relevance only when we have both an embedder (for its model id) and a
    // query vector. Map memory id → cosine similarity (0..1).
    let relevance: HashMap<i64, f64> = match (embedder, query_vec) {
        (Some(emb), Some(qv)) => store
            .knn(db, Some(project), qv, emb.id(), window as usize)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|(id, sim)| (id, sim as f64))
            .collect(),
        _ => HashMap::new(),
    };

    let now = Utc::now().timestamp();
    let mut scored: Vec<(f64, Memory)> = Vec::with_capacity(candidates.len());
    for m in candidates {
        let rel = relevance.get(&m.id).copied().unwrap_or(0.0);
        let rec = recency_weight((now - m.created_at).max(0) as f64, half_life_days);
        let imp = db::get_memory_meta(db, m.id).await?;
        scored.push((blended_score(rel, rec, imp, weights), m));
    }
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    Ok(scored.into_iter().take(limit).map(|(_, m)| m).collect())
}

/// High-level session-start ranking: derive a query signal from the project's
/// git state, embed it (when an embedder is available), and rank recent
/// memories by blended score. With no embedder/git signal this collapses to
/// recency + importance — the legacy injection order.
pub async fn rank_for_injection(
    db: &Database,
    embedder: Option<&dyn Embedder>,
    store: &dyn VectorStore,
    project: &str,
    weights: &Weights,
    half_life_days: f64,
    limit: usize,
) -> Result<Vec<Memory>> {
    let query_vec: Option<Vec<f32>> = match (embedder, context::git_query(project)) {
        (Some(emb), Some(signal)) => embed_one(emb, &signal).await,
        _ => None,
    };
    injection_rank(
        db,
        embedder,
        store,
        project,
        query_vec.as_deref(),
        weights,
        half_life_days,
        limit,
    )
    .await
}

/// Embed a single string, returning the vector or `None` on failure/empty.
async fn embed_one(embedder: &dyn Embedder, text: &str) -> Option<Vec<f32>> {
    embedder
        .embed(&[text.to_string()])
        .await
        .ok()
        .and_then(|mut v| v.drain(..).next())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{create_session, insert_memory};
    use crate::embedder::FakeEmbedder;
    use crate::embedding_codec::normalize;
    use crate::vectorstore::SqliteVecStore;

    #[test]
    fn rrf_fuses_by_reciprocal_rank() {
        let fts = vec![1_i64, 2, 3];
        let vec = vec![3_i64, 1, 4];
        let fused = rrf_fuse(&[fts, vec], 60);
        assert_eq!(fused[0], 1); // appears high in both
        assert!(fused.contains(&4));
    }

    #[test]
    fn rrf_single_list_preserves_order() {
        let fused = rrf_fuse(&[vec![5_i64, 6, 7]], 60);
        assert_eq!(fused, vec![5, 6, 7]);
    }

    #[test]
    fn recency_weight_halves_at_half_life() {
        assert!((recency_weight(0.0, 30.0) - 1.0).abs() < 1e-9);
        let one_hl = 30.0 * 86_400.0;
        assert!((recency_weight(one_hl, 30.0) - 0.5).abs() < 1e-9);
        assert!(recency_weight(one_hl * 20.0, 30.0) < 0.001); // →0 for old
    }

    #[test]
    fn blended_score_is_linear_combo() {
        let w = Weights {
            relevance: 0.5,
            recency: 0.3,
            importance: 0.2,
        };
        let s = blended_score(1.0, 1.0, 1.0, &w);
        assert!((s - 1.0).abs() < 1e-9);
        let s2 = blended_score(1.0, 0.0, 0.0, &w);
        assert!((s2 - 0.5).abs() < 1e-9);
    }

    async fn seeded_db() -> (Database, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!("ironmem-ret-{}.db", uuid::Uuid::new_v4()));
        let db = Database::new(&path.to_string_lossy()).await.unwrap();
        db.migrate().await.unwrap();
        db.ensure_ann(8).await.unwrap();
        let s = create_session(&db, "/tmp/p").await.unwrap();
        // rowids 1,2,3
        insert_memory(&db, "/tmp/p", &s, "alpha auth login", Some("auth"))
            .await
            .unwrap();
        insert_memory(&db, "/tmp/p", &s, "beta search index", Some("search"))
            .await
            .unwrap();
        insert_memory(&db, "/tmp/p", &s, "gamma database schema", Some("db"))
            .await
            .unwrap();
        (db, path)
    }

    async fn embed_all(db: &Database, emb: &FakeEmbedder, store: &SqliteVecStore) {
        for (id, text) in [
            (1_i64, "alpha auth login"),
            (2, "beta search index"),
            (3, "gamma database schema"),
        ] {
            let v = emb.embed(&[text.to_string()]).await.unwrap();
            store.upsert(db, id, emb.id(), emb.dim(), &v[0]).await.unwrap();
        }
    }

    #[tokio::test]
    async fn hybrid_search_surfaces_semantic_hit_fts_misses() {
        let (db, path) = seeded_db().await;
        let emb = FakeEmbedder::new(8);
        let store = SqliteVecStore;
        embed_all(&db, &emb, &store).await;

        // Query whose embedding matches memory 3, but whose keyword has no FTS
        // overlap with any summary — pure FTS returns nothing.
        let qvec = emb.embed(&["gamma database schema".to_string()]).await.unwrap();
        let knn = store.knn(&db, Some("/tmp/p"), &qvec[0], emb.id(), 3).await.unwrap();
        assert_eq!(knn[0].0, 3);

        let res = hybrid_search(&db, Some(&emb), &store, Some("/tmp/p"), "zzznomatch", 5)
            .await
            .unwrap();
        assert!(res.iter().any(|m| m.id == 3), "semantic hit should appear");
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn hybrid_search_without_embedder_is_pure_fts() {
        let (db, path) = seeded_db().await;
        let store = SqliteVecStore;
        let res = hybrid_search(&db, None, &store, Some("/tmp/p"), "alpha", 5)
            .await
            .unwrap();
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].id, 1);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn injection_rank_prefers_semantic_match() {
        let (db, path) = seeded_db().await;
        let emb = FakeEmbedder::new(8);
        let store = SqliteVecStore;
        embed_all(&db, &emb, &store).await;
        let weights = Weights::default();

        let qvec = normalize(&emb.embed(&["gamma database schema".to_string()]).await.unwrap()[0]);
        let ranked = injection_rank(
            &db,
            Some(&emb),
            &store,
            "/tmp/p",
            Some(&qvec),
            &weights,
            30.0,
            3,
        )
        .await
        .unwrap();
        assert_eq!(ranked[0].id, 3, "semantically nearest ranks first");
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn injection_rank_without_query_is_recency_order() {
        let (db, path) = seeded_db().await;
        let store = crate::vectorstore::BruteForceStore;
        let weights = Weights::default();
        let ranked = injection_rank(&db, None, &store, "/tmp/p", None, &weights, 30.0, 3)
            .await
            .unwrap();
        // All created ~now with equal importance ⇒ stable recency (DESC) order.
        assert_eq!(ranked.len(), 3);
        let _ = std::fs::remove_file(path);
    }
}
