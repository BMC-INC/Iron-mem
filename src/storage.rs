//! Pluggable storage substrate (paper addition #4, HYBRID direction).
//!
//! IronMem composes ranking (RRF, rerank, narrative reserve) and governance
//! (ledger, tombstones, consent, trust tiers, namespace authority) ON TOP of
//! this trait. A `StorageBackend` is responsible only for durable store + raw
//! recall of content, embeddings, dated facts, and graph edges.
//!
//! The trait speaks the native `i64` memory id rather than an opaque
//! `BackendId` on purpose: every governance-bearing structure in IronMem (the
//! ledger hash-chain, `memory_meta`, trust tiers, edges, tombstones) keys on
//! that i64. Governance layered above the trait must correlate against the same
//! identity space, so an external adapter is responsible for mapping its own id
//! space onto i64 internally — IronMem keeps the identity, the backend keeps the
//! bytes. This is what makes "governance over a store you don't own" tractable
//! (spec §4): the audit trail never leaves IronMem.
//!
//! `NativeBackend` is the default adapter: it borrows the request-scoped
//! `Database` + vector store and delegates every method to the existing `db::`
//! / `VectorStore` functions, so behavior is byte-identical to the pre-trait
//! direct-SQL path. Dogfooding the native engine through the trait is how the
//! abstraction is proven before any external backend (spec §3).

use anyhow::Result;
use async_trait::async_trait;
use reqwest::Client;
use serde_json::{json, Value};

use crate::db::{self, DatedMemory, Database, Memory, MemoryEdge};
use crate::vectorstore::VectorStore;

/// What recall signals a backend can serve. The fusion layer gates optional
/// signals on these flags so an adapter that lacks (say) a graph engine simply
/// contributes nothing rather than erroring.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BackendCaps {
    pub fulltext: bool,
    pub vector: bool,
    pub temporal: bool,
    pub entity: bool,
    pub graph: bool,
    /// True if the backend isolates namespaces natively (collections/indexes);
    /// false means IronMem must enforce namespace authority above the trait.
    pub native_namespacing: bool,
}

/// One recall candidate from a single storage signal. `memory` is populated by
/// signals that already materialized the row (native full-text) so the fusion
/// layer can reuse it without a re-fetch; id-only signals (vector) leave it
/// `None` and the fusion layer materializes on demand via [`StorageBackend::get_memory`].
#[derive(Debug, Clone)]
pub struct Candidate {
    pub id: i64,
    // Ranking-aware backends populate this; the current id-based RRF fusion
    // ignores it. Kept on the contract for adapters that fuse by score (P3).
    #[allow(dead_code)]
    pub score: f32,
    pub memory: Option<Memory>,
}

impl Candidate {
    pub fn id_only(id: i64, score: f32) -> Self {
        Self {
            id,
            score,
            memory: None,
        }
    }
    pub fn with_memory(m: Memory, score: f32) -> Self {
        Self {
            id: m.id,
            score,
            memory: Some(m),
        }
    }
}

/// A content embedding to persist alongside a memory. A vector backend stores
/// this in its index; the native backend writes it via the [`VectorStore`].
#[derive(Debug, Clone)]
pub struct Embedding {
    pub model: String,
    pub dim: usize,
    pub vector: Vec<f32>,
}

/// Substrate write record. Governance metadata (consent, trust, classification,
/// ledger) is attached above the trait by the governed-write path, not here.
/// `embedding` is written when present so the same call seeds both the row and
/// the vector index — the seam a vector adapter (P3) implements.
// Consumed by the P2 conformance suite and the P3/P4 external adapters; the
// retrieval hot path is read-only, so the binary itself does not construct it.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct BackendMemory {
    pub project: String,
    pub session_id: String,
    pub summary: String,
    pub tags: Option<String>,
    pub embedding: Option<Embedding>,
}

/// A graph edge to persist. Governance/identity stays keyed on `memory_id`; an
/// external graph adapter (P4) maps the entity/relation onto its own store.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct BackendEdge {
    pub project: String,
    pub memory_id: i64,
    pub source: String,
    pub relation: String,
    pub target: String,
    pub confidence: f64,
}

/// A pluggable memory substrate. See the module docs for the identity/governance
/// contract. Methods that a backend does not support return an empty `Vec`
/// (never an error) so the fusion layer degrades gracefully.
///
/// `capabilities`/`put_memory`/`hide`/`purge` are the write+lifecycle surface
/// exercised by the P2 conformance suite (cfg(test)) and the P3/P4 external
/// adapters; the retrieval hot path only reads, so the binary does not call them
/// yet. The allow keeps a no-test release build clean without dropping the
/// contract those phases depend on.
#[allow(dead_code)]
#[async_trait]
pub trait StorageBackend: Send + Sync {
    fn capabilities(&self) -> BackendCaps;

    // ── Content ──────────────────────────────────────────────────────
    async fn put_memory(&self, rec: &BackendMemory) -> Result<i64>;
    async fn get_memory(&self, namespace: &str, id: i64) -> Result<Option<Memory>>;

    // ── Recall signals (ranked candidates; empty Vec when unsupported) ─
    async fn fulltext_search(
        &self,
        namespace: &str,
        project: Option<&str>,
        query: &str,
        k: usize,
    ) -> Result<Vec<Candidate>>;
    async fn vector_search(
        &self,
        project: Option<&str>,
        qvec: &[f32],
        model: &str,
        k: usize,
    ) -> Result<Vec<Candidate>>;
    async fn memories_by_event_time(
        &self,
        project: Option<&str>,
        needle: &str,
        k: usize,
    ) -> Result<Vec<i64>>;
    async fn memories_for_entity(
        &self,
        project: Option<&str>,
        entity: &str,
        k: usize,
    ) -> Result<Vec<i64>>;
    async fn dated_memories(&self, project: Option<&str>, k: usize) -> Result<Vec<DatedMemory>>;

    // ── Graph ────────────────────────────────────────────────────────
    async fn put_edge(&self, edge: &BackendEdge) -> Result<i64>;
    async fn edges_for_entity(
        &self,
        project: Option<&str>,
        entity: &str,
        include_superseded: bool,
        k: usize,
    ) -> Result<Vec<MemoryEdge>>;

    // ── Governed lifecycle (driven by the governance layer above) ─────
    /// Governed delete: record the forget in the ledger and make the memory
    /// un-retrievable. On the native engine this removes the row (no residual
    /// bytes). A backend without soft-delete must filter at query time AND
    /// document that bytes persist until [`purge`](StorageBackend::purge).
    async fn hide(&self, id: i64, actor: Option<&str>, reason: Option<&str>) -> Result<bool>;
    /// Hard delete. On the native engine this is identical to `hide` (the
    /// governed delete already removes bytes); kept distinct so external
    /// adapters can expose a separate hard-purge for residual bytes.
    async fn purge(&self, id: i64) -> Result<bool>;
}

/// The native SQLite/Postgres engine as the default [`StorageBackend`]. Holds
/// request-scoped borrows and delegates to the existing `db::` / [`VectorStore`]
/// functions verbatim — the storage seam with zero behavioral change.
pub struct NativeBackend<'a> {
    db: &'a Database,
    store: &'a dyn VectorStore,
}

impl<'a> NativeBackend<'a> {
    pub fn new(db: &'a Database, store: &'a dyn VectorStore) -> Self {
        Self { db, store }
    }
}

#[async_trait]
impl StorageBackend for NativeBackend<'_> {
    fn capabilities(&self) -> BackendCaps {
        BackendCaps {
            fulltext: true,
            vector: true,
            temporal: true,
            entity: true,
            graph: true,
            native_namespacing: true,
        }
    }

    async fn put_memory(&self, rec: &BackendMemory) -> Result<i64> {
        let id = db::insert_memory(
            self.db,
            &rec.project,
            &rec.session_id,
            &rec.summary,
            rec.tags.as_deref(),
        )
        .await?;
        if let Some(emb) = &rec.embedding {
            self.store
                .upsert(self.db, id, &emb.model, emb.dim, &emb.vector)
                .await?;
        }
        Ok(id)
    }

    async fn get_memory(&self, namespace: &str, id: i64) -> Result<Option<Memory>> {
        db::get_memory_by_id_in_namespace(self.db, id, namespace).await
    }

    async fn fulltext_search(
        &self,
        namespace: &str,
        project: Option<&str>,
        query: &str,
        k: usize,
    ) -> Result<Vec<Candidate>> {
        let rows = match project {
            Some(p) => db::search_memories_in_namespace(self.db, namespace, p, query, k as i64).await?,
            None => db::search_all_memories_in_namespace(self.db, namespace, query, k as i64).await?,
        };
        // FTS rank is the order; the id-based RRF above ignores the score, but a
        // descending positional score keeps it meaningful if ever consumed.
        Ok(rows
            .into_iter()
            .enumerate()
            .map(|(i, m)| Candidate::with_memory(m, k.saturating_sub(i) as f32))
            .collect())
    }

    async fn vector_search(
        &self,
        project: Option<&str>,
        qvec: &[f32],
        model: &str,
        k: usize,
    ) -> Result<Vec<Candidate>> {
        Ok(self
            .store
            .knn(self.db, project, qvec, model, k)
            .await?
            .into_iter()
            .map(|(id, sim)| Candidate::id_only(id, sim))
            .collect())
    }

    async fn memories_by_event_time(
        &self,
        project: Option<&str>,
        needle: &str,
        k: usize,
    ) -> Result<Vec<i64>> {
        db::memories_by_event_time(self.db, project, needle, k).await
    }

    async fn memories_for_entity(
        &self,
        project: Option<&str>,
        entity: &str,
        k: usize,
    ) -> Result<Vec<i64>> {
        db::memories_for_entity(self.db, project, entity, k).await
    }

    async fn dated_memories(&self, project: Option<&str>, k: usize) -> Result<Vec<DatedMemory>> {
        db::dated_memories(self.db, project, k).await
    }

    async fn put_edge(&self, edge: &BackendEdge) -> Result<i64> {
        db::insert_memory_edge(
            self.db,
            &db::NewMemoryEdge {
                project: edge.project.clone(),
                memory_id: edge.memory_id,
                source: edge.source.clone(),
                relation: edge.relation.clone(),
                target: edge.target.clone(),
                valid_from: None,
                valid_until: None,
                confidence: edge.confidence,
            },
        )
        .await
    }

    async fn edges_for_entity(
        &self,
        project: Option<&str>,
        entity: &str,
        include_superseded: bool,
        k: usize,
    ) -> Result<Vec<MemoryEdge>> {
        db::memory_edges_for_entity(self.db, project, entity, include_superseded, k).await
    }

    async fn hide(&self, id: i64, actor: Option<&str>, reason: Option<&str>) -> Result<bool> {
        db::governed_delete_memory(self.db, id, actor, reason).await
    }

    async fn purge(&self, id: i64) -> Result<bool> {
        db::governed_delete_memory(self.db, id, None, Some("purge")).await
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// External reference adapters (P3 vector / P4 graph), REAL network backends.
//
// Both reuse the existing `reqwest` dependency (no new client crate) and follow
// the Mem0/Zep split: content + governance (ledger, tombstones, consent, trust,
// namespaces) stay native; only the vector index (Qdrant) or the entity/edge
// store (Neo4j) lives in the external engine, addressed by the local i64 spine.
// They are selected by config; the native engine remains the deployed default
// (so the existing 28k-memory store keeps full enforcement). Each is proven
// against a LIVE service by an env-gated integration test in `mod conformance`,
// running the same governance conformance suite as the native backend.
// ─────────────────────────────────────────────────────────────────────────────

/// Real vector adapter backed by an external **Qdrant** over its REST API.
/// Embeddings live in Qdrant; everything governance-bearing stays native.
#[allow(dead_code)]
pub struct QdrantVectorBackend<'a> {
    inner: NativeBackend<'a>,
    http: Client,
    base: String,
    collection: String,
}

#[allow(dead_code)]
impl<'a> QdrantVectorBackend<'a> {
    /// Connect and ensure the collection exists (Cosine, `dim`-d vectors).
    pub async fn new(
        inner: NativeBackend<'a>,
        base_url: &str,
        collection: &str,
        dim: usize,
    ) -> Result<Self> {
        let http = Client::new();
        let base = base_url.trim_end_matches('/').to_string();
        let resp = http
            .put(format!("{base}/collections/{collection}"))
            .json(&json!({"vectors": {"size": dim, "distance": "Cosine"}}))
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            // A pre-existing collection is fine; anything else is a real failure.
            if !body.contains("already exists") {
                anyhow::bail!("qdrant create collection {collection}: {status} {body}");
            }
        }
        Ok(Self {
            inner,
            http,
            base,
            collection: collection.to_string(),
        })
    }
}

#[async_trait]
impl StorageBackend for QdrantVectorBackend<'_> {
    fn capabilities(&self) -> BackendCaps {
        BackendCaps {
            vector: true,
            native_namespacing: false, // Qdrant payload-filtered, not native ns
            ..self.inner.capabilities()
        }
    }

    async fn put_memory(&self, rec: &BackendMemory) -> Result<i64> {
        let spine = BackendMemory {
            embedding: None,
            ..rec.clone()
        };
        let id = self.inner.put_memory(&spine).await?;
        if let Some(emb) = &rec.embedding {
            let body = json!({
                "points": [{
                    "id": id,
                    "vector": emb.vector,
                    "payload": {"project": rec.project, "model": emb.model},
                }]
            });
            // wait=true makes the write visible to the read-after-write in the
            // conformance suite (Qdrant indexes asynchronously by default).
            self.http
                .put(format!(
                    "{}/collections/{}/points?wait=true",
                    self.base, self.collection
                ))
                .json(&body)
                .send()
                .await?
                .error_for_status()?;
        }
        Ok(id)
    }

    async fn get_memory(&self, ns: &str, id: i64) -> Result<Option<Memory>> {
        self.inner.get_memory(ns, id).await
    }

    async fn fulltext_search(
        &self,
        ns: &str,
        project: Option<&str>,
        query: &str,
        k: usize,
    ) -> Result<Vec<Candidate>> {
        self.inner.fulltext_search(ns, project, query, k).await
    }

    async fn vector_search(
        &self,
        project: Option<&str>,
        qvec: &[f32],
        model: &str,
        k: usize,
    ) -> Result<Vec<Candidate>> {
        let mut must = vec![json!({"key": "model", "match": {"value": model}})];
        if let Some(p) = project {
            must.push(json!({"key": "project", "match": {"value": p}}));
        }
        let body = json!({
            "vector": qvec,
            "limit": k,
            "with_payload": false,
            "filter": {"must": must},
        });
        let resp = self
            .http
            .post(format!(
                "{}/collections/{}/points/search",
                self.base, self.collection
            ))
            .json(&body)
            .send()
            .await?
            .error_for_status()?;
        let v: Value = resp.json().await?;
        Ok(v["result"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|p| {
                        let id = p["id"].as_i64()?;
                        let score = p["score"].as_f64().unwrap_or(0.0) as f32;
                        Some(Candidate::id_only(id, score))
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn memories_by_event_time(
        &self,
        project: Option<&str>,
        needle: &str,
        k: usize,
    ) -> Result<Vec<i64>> {
        self.inner.memories_by_event_time(project, needle, k).await
    }

    async fn memories_for_entity(
        &self,
        project: Option<&str>,
        entity: &str,
        k: usize,
    ) -> Result<Vec<i64>> {
        self.inner.memories_for_entity(project, entity, k).await
    }

    async fn dated_memories(&self, project: Option<&str>, k: usize) -> Result<Vec<DatedMemory>> {
        self.inner.dated_memories(project, k).await
    }

    async fn put_edge(&self, edge: &BackendEdge) -> Result<i64> {
        self.inner.put_edge(edge).await
    }

    async fn edges_for_entity(
        &self,
        project: Option<&str>,
        entity: &str,
        include_superseded: bool,
        k: usize,
    ) -> Result<Vec<MemoryEdge>> {
        self.inner
            .edges_for_entity(project, entity, include_superseded, k)
            .await
    }

    async fn hide(&self, id: i64, actor: Option<&str>, reason: Option<&str>) -> Result<bool> {
        self.inner.hide(id, actor, reason).await
    }

    async fn purge(&self, id: i64) -> Result<bool> {
        self.inner.purge(id).await
    }
}

/// Real graph adapter backed by an external **Neo4j** over its transactional
/// Cypher HTTP API. Entities + edges live in Neo4j; governance stays native.
#[allow(dead_code)]
pub struct Neo4jGraphBackend<'a> {
    inner: NativeBackend<'a>,
    http: Client,
    endpoint: String,
    user: String,
    pass: String,
}

#[allow(dead_code)]
impl<'a> Neo4jGraphBackend<'a> {
    pub fn new(
        inner: NativeBackend<'a>,
        base_url: &str,
        database: &str,
        user: &str,
        pass: &str,
    ) -> Self {
        let endpoint = format!(
            "{}/db/{}/tx/commit",
            base_url.trim_end_matches('/'),
            database
        );
        Self {
            inner,
            http: Client::new(),
            endpoint,
            user: user.to_string(),
            pass: pass.to_string(),
        }
    }

    async fn cypher(&self, statement: &str, params: Value) -> Result<Value> {
        let resp = self
            .http
            .post(&self.endpoint)
            .basic_auth(&self.user, Some(&self.pass))
            .json(&json!({"statements": [{"statement": statement, "parameters": params}]}))
            .send()
            .await?
            .error_for_status()?;
        let v: Value = resp.json().await?;
        if let Some(errs) = v["errors"].as_array() {
            if !errs.is_empty() {
                anyhow::bail!("neo4j cypher error: {}", v["errors"]);
            }
        }
        Ok(v)
    }
}

#[async_trait]
impl StorageBackend for Neo4jGraphBackend<'_> {
    fn capabilities(&self) -> BackendCaps {
        BackendCaps {
            graph: true,
            ..self.inner.capabilities()
        }
    }

    async fn put_memory(&self, rec: &BackendMemory) -> Result<i64> {
        self.inner.put_memory(rec).await
    }

    async fn get_memory(&self, ns: &str, id: i64) -> Result<Option<Memory>> {
        self.inner.get_memory(ns, id).await
    }

    async fn fulltext_search(
        &self,
        ns: &str,
        project: Option<&str>,
        query: &str,
        k: usize,
    ) -> Result<Vec<Candidate>> {
        self.inner.fulltext_search(ns, project, query, k).await
    }

    async fn vector_search(
        &self,
        project: Option<&str>,
        qvec: &[f32],
        model: &str,
        k: usize,
    ) -> Result<Vec<Candidate>> {
        self.inner.vector_search(project, qvec, model, k).await
    }

    async fn memories_by_event_time(
        &self,
        project: Option<&str>,
        needle: &str,
        k: usize,
    ) -> Result<Vec<i64>> {
        self.inner.memories_by_event_time(project, needle, k).await
    }

    async fn memories_for_entity(
        &self,
        project: Option<&str>,
        entity: &str,
        k: usize,
    ) -> Result<Vec<i64>> {
        self.inner.memories_for_entity(project, entity, k).await
    }

    async fn dated_memories(&self, project: Option<&str>, k: usize) -> Result<Vec<DatedMemory>> {
        self.inner.dated_memories(project, k).await
    }

    async fn put_edge(&self, edge: &BackendEdge) -> Result<i64> {
        let stmt = "MERGE (s:Entity {name: $source, project: $project}) \
                    MERGE (t:Entity {name: $target, project: $project}) \
                    CREATE (s)-[r:REL {memory_id: $mid, relation: $relation, source: $source, \
                            target: $target, project: $project, confidence: $conf}]->(t) \
                    RETURN id(r) AS id";
        let v = self
            .cypher(
                stmt,
                json!({
                    "source": edge.source, "target": edge.target, "project": edge.project,
                    "mid": edge.memory_id, "relation": edge.relation, "conf": edge.confidence,
                }),
            )
            .await?;
        Ok(v["results"][0]["data"][0]["row"][0].as_i64().unwrap_or(0))
    }

    async fn edges_for_entity(
        &self,
        project: Option<&str>,
        entity: &str,
        _include_superseded: bool,
        k: usize,
    ) -> Result<Vec<MemoryEdge>> {
        // k is a trusted usize; inline it (Cypher params are awkward in LIMIT).
        let stmt = format!(
            "MATCH (s:Entity)-[r:REL]->(t:Entity) \
             WHERE ($project IS NULL OR r.project = $project) \
               AND (toLower(r.source) CONTAINS toLower($entity) \
                    OR toLower(r.target) CONTAINS toLower($entity)) \
             RETURN r.memory_id AS memory_id, r.source AS source, r.relation AS relation, \
                    r.target AS target, r.project AS project, r.confidence AS confidence \
             LIMIT {k}"
        );
        let v = self
            .cypher(&stmt, json!({"project": project, "entity": entity}))
            .await?;
        let rows = v["results"][0]["data"].as_array().cloned().unwrap_or_default();
        Ok(rows
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let row = &r["row"];
                MemoryEdge {
                    id: i as i64 + 1,
                    project: row[4].as_str().unwrap_or_default().to_string(),
                    memory_id: row[0].as_i64().unwrap_or(0),
                    source: row[1].as_str().unwrap_or_default().to_string(),
                    relation: row[2].as_str().unwrap_or_default().to_string(),
                    target: row[3].as_str().unwrap_or_default().to_string(),
                    valid_from: None,
                    valid_until: None,
                    observed_at: 0,
                    confidence: row[5].as_f64().unwrap_or(0.0),
                    superseded_by: None,
                    superseded_reason: None,
                    created_at: 0,
                }
            })
            .collect())
    }

    async fn hide(&self, id: i64, actor: Option<&str>, reason: Option<&str>) -> Result<bool> {
        self.inner.hide(id, actor, reason).await
    }

    async fn purge(&self, id: i64) -> Result<bool> {
        self.inner.purge(id).await
    }
}

#[cfg(test)]
mod conformance {
    //! Backend-agnostic governance conformance suite (#4 P2).
    //!
    //! The substrate is pluggable, but governance (consent, namespace authority,
    //! tombstones, ledger hash-chain, trust priority) stays in IronMem and must
    //! compose correctly over WHATEVER store sits behind the trait.
    //! [`assert_conformance`] takes the LOCAL governance `db` AND the `backend` —
    //! modelling exactly that split: governance always keys on the local i64
    //! identity + ledger, the backend only holds bytes. `NativeBackend` runs it
    //! now; the P3 vector and P4 graph adapters reuse the identical harness, so a
    //! green run here is the contract every adapter must meet.
    //!
    //! §4 enforce-vs-observe: the native engine controls write ordering, so the
    //! consent gate runs BEFORE the substrate write (`governed_write` validates
    //! first) — native ENFORCES, it does not merely observe. An external adapter
    //! that can only record after the fact must document the weaker guarantee.

    use super::*;
    use crate::governance::{
        ledger_entry_hash, ConsentState, DataClassification, MemoryGovernance, TrustTier,
    };
    use crate::vectorstore::BruteForceStore;
    use std::sync::{Arc, Mutex};

    fn cosine(a: &[f32], b: &[f32]) -> f32 {
        let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
        let na = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let nb = b.iter().map(|x| x * x).sum::<f32>().sqrt();
        if na == 0.0 || nb == 0.0 {
            0.0
        } else {
            dot / (na * nb)
        }
    }

    /// Stands in for an external vector service (Qdrant/Mem0). In a real adapter
    /// these calls become network calls; the trait surface above does not change.
    #[derive(Default, Clone)]
    struct InProcessVectorIndex {
        rows: Arc<Mutex<Vec<(i64, String, String, Vec<f32>)>>>, // (id, project, model, vector)
    }
    impl InProcessVectorIndex {
        fn insert(&self, id: i64, project: &str, emb: &Embedding) {
            self.rows
                .lock()
                .unwrap()
                .push((id, project.to_string(), emb.model.clone(), emb.vector.clone()));
        }
        fn knn(&self, project: Option<&str>, q: &[f32], model: &str, k: usize) -> Vec<Candidate> {
            let mut scored: Vec<(i64, f32)> = {
                let rows = self.rows.lock().unwrap();
                rows.iter()
                    .filter(|(_, p, m, _)| m == model && project.is_none_or(|pr| p == pr))
                    .map(|(id, _, _, v)| (*id, cosine(q, v)))
                    .collect()
            };
            scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            scored
                .into_iter()
                .take(k)
                .map(|(id, s)| Candidate::id_only(id, s))
                .collect()
        }
    }

    /// P3 reference vector adapter (Mem0-style): content + governance stay native
    /// (local spine + ledger + tombstones), embeddings live in an EXTERNAL vector
    /// index. The in-process index stands in for a Qdrant client; productionizing
    /// is swapping `InProcessVectorIndex` for the network client, trait and
    /// conformance unchanged. Proves governance composes over a non-native vector
    /// substrate — the i64 spine is the bridge (module docs).
    struct VectorAdapter<'a> {
        inner: NativeBackend<'a>,
        index: InProcessVectorIndex,
    }
    #[async_trait]
    impl StorageBackend for VectorAdapter<'_> {
        fn capabilities(&self) -> BackendCaps {
            BackendCaps {
                vector: true,
                ..self.inner.capabilities()
            }
        }
        async fn put_memory(&self, rec: &BackendMemory) -> Result<i64> {
            // Spine (id + content) stays local; the embedding goes to the
            // external index, not the native vector store.
            let spine = BackendMemory {
                embedding: None,
                ..rec.clone()
            };
            let id = self.inner.put_memory(&spine).await?;
            if let Some(emb) = &rec.embedding {
                self.index.insert(id, &rec.project, emb);
            }
            Ok(id)
        }
        async fn get_memory(&self, ns: &str, id: i64) -> Result<Option<Memory>> {
            self.inner.get_memory(ns, id).await
        }
        async fn fulltext_search(
            &self,
            ns: &str,
            project: Option<&str>,
            query: &str,
            k: usize,
        ) -> Result<Vec<Candidate>> {
            self.inner.fulltext_search(ns, project, query, k).await
        }
        async fn vector_search(
            &self,
            project: Option<&str>,
            qvec: &[f32],
            model: &str,
            k: usize,
        ) -> Result<Vec<Candidate>> {
            Ok(self.index.knn(project, qvec, model, k))
        }
        async fn memories_by_event_time(
            &self,
            project: Option<&str>,
            needle: &str,
            k: usize,
        ) -> Result<Vec<i64>> {
            self.inner.memories_by_event_time(project, needle, k).await
        }
        async fn memories_for_entity(
            &self,
            project: Option<&str>,
            entity: &str,
            k: usize,
        ) -> Result<Vec<i64>> {
            self.inner.memories_for_entity(project, entity, k).await
        }
        async fn dated_memories(
            &self,
            project: Option<&str>,
            k: usize,
        ) -> Result<Vec<DatedMemory>> {
            self.inner.dated_memories(project, k).await
        }
        async fn put_edge(&self, edge: &BackendEdge) -> Result<i64> {
            self.inner.put_edge(edge).await
        }
        async fn edges_for_entity(
            &self,
            project: Option<&str>,
            entity: &str,
            include_superseded: bool,
            k: usize,
        ) -> Result<Vec<MemoryEdge>> {
            self.inner
                .edges_for_entity(project, entity, include_superseded, k)
                .await
        }
        async fn hide(&self, id: i64, actor: Option<&str>, reason: Option<&str>) -> Result<bool> {
            self.inner.hide(id, actor, reason).await
        }
        async fn purge(&self, id: i64) -> Result<bool> {
            self.inner.purge(id).await
        }
    }

    /// Stands in for an external graph service (Zep/Graphiti).
    #[derive(Default, Clone)]
    struct InProcessGraphStore {
        edges: Arc<Mutex<Vec<MemoryEdge>>>,
    }
    impl InProcessGraphStore {
        fn put(&self, edge: &BackendEdge) -> i64 {
            let mut v = self.edges.lock().unwrap();
            let id = v.len() as i64 + 1;
            v.push(MemoryEdge {
                id,
                project: edge.project.clone(),
                memory_id: edge.memory_id,
                source: edge.source.clone(),
                relation: edge.relation.clone(),
                target: edge.target.clone(),
                valid_from: None,
                valid_until: None,
                observed_at: 0,
                confidence: edge.confidence,
                superseded_by: None,
                superseded_reason: None,
                created_at: 0,
            });
            id
        }
        fn for_entity(&self, project: Option<&str>, entity: &str, k: usize) -> Vec<MemoryEdge> {
            let needle = entity.to_ascii_lowercase();
            self.edges
                .lock()
                .unwrap()
                .iter()
                .filter(|e| project.is_none_or(|p| e.project == p))
                .filter(|e| {
                    e.source.to_ascii_lowercase().contains(&needle)
                        || e.target.to_ascii_lowercase().contains(&needle)
                })
                .take(k)
                .cloned()
                .collect()
        }
    }

    /// P4 reference graph adapter (Zep/Graphiti-style): content + governance stay
    /// native, entity/edge storage is EXTERNAL. Same composition pattern as the
    /// vector adapter; one-hop bridge retrieval works through `edges_for_entity`.
    struct GraphAdapter<'a> {
        inner: NativeBackend<'a>,
        graph: InProcessGraphStore,
    }
    #[async_trait]
    impl StorageBackend for GraphAdapter<'_> {
        fn capabilities(&self) -> BackendCaps {
            BackendCaps {
                graph: true,
                ..self.inner.capabilities()
            }
        }
        async fn put_memory(&self, rec: &BackendMemory) -> Result<i64> {
            self.inner.put_memory(rec).await
        }
        async fn get_memory(&self, ns: &str, id: i64) -> Result<Option<Memory>> {
            self.inner.get_memory(ns, id).await
        }
        async fn fulltext_search(
            &self,
            ns: &str,
            project: Option<&str>,
            query: &str,
            k: usize,
        ) -> Result<Vec<Candidate>> {
            self.inner.fulltext_search(ns, project, query, k).await
        }
        async fn vector_search(
            &self,
            project: Option<&str>,
            qvec: &[f32],
            model: &str,
            k: usize,
        ) -> Result<Vec<Candidate>> {
            self.inner.vector_search(project, qvec, model, k).await
        }
        async fn memories_by_event_time(
            &self,
            project: Option<&str>,
            needle: &str,
            k: usize,
        ) -> Result<Vec<i64>> {
            self.inner.memories_by_event_time(project, needle, k).await
        }
        async fn memories_for_entity(
            &self,
            project: Option<&str>,
            entity: &str,
            k: usize,
        ) -> Result<Vec<i64>> {
            self.inner.memories_for_entity(project, entity, k).await
        }
        async fn dated_memories(
            &self,
            project: Option<&str>,
            k: usize,
        ) -> Result<Vec<DatedMemory>> {
            self.inner.dated_memories(project, k).await
        }
        async fn put_edge(&self, edge: &BackendEdge) -> Result<i64> {
            Ok(self.graph.put(edge))
        }
        async fn edges_for_entity(
            &self,
            project: Option<&str>,
            entity: &str,
            _include_superseded: bool,
            k: usize,
        ) -> Result<Vec<MemoryEdge>> {
            Ok(self.graph.for_entity(project, entity, k))
        }
        async fn hide(&self, id: i64, actor: Option<&str>, reason: Option<&str>) -> Result<bool> {
            self.inner.hide(id, actor, reason).await
        }
        async fn purge(&self, id: i64) -> Result<bool> {
            self.inner.purge(id).await
        }
    }

    async fn fresh_db() -> Database {
        let path = std::env::temp_dir().join(format!("ironmem-conf-{}.db", uuid::Uuid::new_v4()));
        let db = Database::new(&path.to_string_lossy()).await.unwrap();
        db.migrate().await.unwrap();
        db
    }

    /// A full governed write: consent gate FIRST (enforce), then substrate write
    /// through the trait, then governance metadata + ledger above the trait.
    async fn governed_write(
        db: &Database,
        backend: &dyn StorageBackend,
        project: &str,
        session: &str,
        summary: &str,
        gov: &MemoryGovernance,
        embedding: Option<Embedding>,
    ) -> anyhow::Result<i64> {
        gov.validate()?; // consent gate — refuse before any byte reaches the backend
        let id = backend
            .put_memory(&BackendMemory {
                project: project.to_string(),
                session_id: session.to_string(),
                summary: summary.to_string(),
                tags: None,
                embedding,
            })
            .await?;
        db::apply_memory_governance(
            db,
            id,
            "project",
            "fact",
            gov,
            gov.writer_identity.as_deref(),
            "create",
        )
        .await?;
        Ok(id)
    }

    fn gov_in(namespace: &str, tier: TrustTier) -> MemoryGovernance {
        MemoryGovernance {
            namespace: namespace.to_string(),
            trust_tier: tier,
            ..MemoryGovernance::explicit()
        }
    }

    async fn fulltext_ids(
        backend: &dyn StorageBackend,
        ns: &str,
        project: &str,
        query: &str,
    ) -> Vec<i64> {
        backend
            .fulltext_search(ns, Some(project), query, 20)
            .await
            .unwrap()
            .into_iter()
            .map(|c| c.id)
            .collect()
    }

    /// The reusable contract. Every `StorageBackend` must pass this.
    async fn assert_conformance(db: &Database, backend: &dyn StorageBackend) {
        let project = "/conf/p";
        let session = db::create_session(db, project).await.unwrap();
        let caps = backend.capabilities();

        // ── 0. capabilities are truthful: we exercise exactly what they claim ──
        assert!(caps.fulltext, "native must advertise fulltext");

        // ── 1. put/get round-trip (through the trait) ──
        let id = governed_write(
            db,
            backend,
            project,
            &session,
            "alpha widget specification",
            &gov_in("local", TrustTier::Medium),
            None,
        )
        .await
        .unwrap();
        let got = backend.get_memory("local", id).await.unwrap();
        assert_eq!(
            got.map(|m| m.summary),
            Some("alpha widget specification".to_string()),
            "put/get round-trip must preserve content"
        );
        assert!(
            fulltext_ids(backend, "local", project, "widget").await.contains(&id),
            "fulltext must recall a written memory"
        );

        // ── 2. consent gate: PHI without granted consent is REFUSED (enforce) ──
        let mut phi = gov_in("phi_ns", TrustTier::High);
        phi.classification = DataClassification::Phi;
        phi.consent_state = None;
        let blocked = governed_write(
            db,
            backend,
            project,
            &session,
            "patient bravo record",
            &phi,
            None,
        )
        .await;
        assert!(blocked.is_err(), "PHI write without consent must be refused");
        assert!(
            fulltext_ids(backend, "phi_ns", project, "bravo").await.is_empty(),
            "a refused consent write must leave nothing retrievable"
        );
        assert!(
            db::memory_ledger_for_namespace(db, "phi_ns").await.unwrap().is_empty(),
            "a refused write must not append a ledger entry"
        );
        // And the same record WITH consent granted is admitted.
        phi.consent_state = Some(ConsentState::Granted);
        let phi_id = governed_write(db, backend, project, &session, "patient charlie record", &phi, None)
            .await
            .expect("PHI write with granted consent must be admitted");

        // ── 3. namespace isolation: no cross-namespace leakage ──
        let a_id = governed_write(
            db,
            backend,
            project,
            &session,
            "delta tenantA secret",
            &gov_in("tenant_a", TrustTier::Medium),
            None,
        )
        .await
        .unwrap();
        assert!(backend.get_memory("tenant_a", a_id).await.unwrap().is_some());
        assert!(
            backend.get_memory("tenant_b", a_id).await.unwrap().is_none(),
            "a memory must not be visible from another namespace by id"
        );
        assert!(
            fulltext_ids(backend, "tenant_a", project, "delta").await.contains(&a_id),
            "fulltext must find the memory in its own namespace"
        );
        assert!(
            fulltext_ids(backend, "tenant_b", project, "delta").await.is_empty(),
            "fulltext must not leak the memory into another namespace"
        );

        // ── 4. tombstone hides from retrieval ──
        assert!(fulltext_ids(backend, "tenant_a", project, "delta").await.contains(&a_id));
        let hidden = backend.hide(a_id, Some("admin"), Some("dsr")).await.unwrap();
        assert!(hidden, "hide must report it acted");
        assert!(
            backend.get_memory("tenant_a", a_id).await.unwrap().is_none(),
            "tombstoned memory must be un-retrievable by id"
        );
        assert!(
            !fulltext_ids(backend, "tenant_a", project, "delta").await.contains(&a_id),
            "tombstoned memory must be gone from fulltext"
        );

        // ── 5. ledger hash-chain continuity (over the tenant_a namespace) ──
        let entries = db::memory_ledger_for_namespace(db, "tenant_a").await.unwrap();
        assert!(
            entries.len() >= 2,
            "tenant_a must have a create + forget entry, got {}",
            entries.len()
        );
        assert!(entries[0].prev_hash.is_none(), "first ledger entry has no parent");
        for win in entries.windows(2) {
            assert_eq!(
                win[1].prev_hash.as_deref(),
                Some(win[0].entry_hash.as_str()),
                "each ledger entry must chain to its predecessor"
            );
        }
        for e in &entries {
            let recomputed = ledger_entry_hash(
                e.prev_hash.as_deref(),
                &e.namespace,
                e.memory_id,
                &e.op_type,
                e.actor.as_deref(),
                &e.payload,
                e.created_at,
            );
            assert_eq!(recomputed, e.entry_hash, "ledger entry must be tamper-evident");
        }

        // ── 6. trust-tier metadata fidelity (the signal retrieval prioritizes) ──
        let hi = governed_write(db, backend, project, &session, "echo high-trust fact", &gov_in("trust_ns", TrustTier::High), None).await.unwrap();
        let lo = governed_write(db, backend, project, &session, "echo low-trust fact", &gov_in("trust_ns", TrustTier::Low), None).await.unwrap();
        let tiers = db::trust_tiers_for(db, &[hi, lo]).await.unwrap();
        assert_eq!(tiers.get(&hi).map(String::as_str), Some("high"));
        assert_eq!(tiers.get(&lo).map(String::as_str), Some("low"));

        // ── 7. vector seam (only if advertised) ──
        if caps.vector {
            let v_id = governed_write(
                db,
                backend,
                project,
                &session,
                "foxtrot vector carrier",
                &gov_in("vec_ns", TrustTier::Medium),
                Some(Embedding { model: "conf-model".to_string(), dim: 4, vector: vec![1.0, 0.0, 0.0, 0.0] }),
            )
            .await
            .unwrap();
            let hits = backend
                .vector_search(Some(project), &[1.0, 0.0, 0.0, 0.0], "conf-model", 5)
                .await
                .unwrap();
            assert!(
                hits.iter().any(|c| c.id == v_id),
                "vector_search must recall an embedding written through put_memory"
            );
        }

        // ── 8. graph seam (only if advertised) ──
        if caps.graph {
            let _ = phi_id; // a real memory id to anchor the edge to
            backend
                .put_edge(&BackendEdge {
                    project: project.to_string(),
                    memory_id: phi_id,
                    source: "acme".to_string(),
                    relation: "works_at".to_string(),
                    target: "globex".to_string(),
                    confidence: 0.9,
                })
                .await
                .unwrap();
            let edges = backend.edges_for_entity(Some(project), "acme", false, 5).await.unwrap();
            assert!(
                edges.iter().any(|e| e.memory_id == phi_id && e.target.eq_ignore_ascii_case("globex")),
                "edges_for_entity must recall an edge written through put_edge"
            );
        }
    }

    #[tokio::test]
    async fn native_backend_passes_conformance() {
        let db = fresh_db().await;
        let store = BruteForceStore;
        let backend = NativeBackend::new(&db, &store);
        assert_conformance(&db, &backend).await;
    }

    // The same contract must hold for a NON-native substrate. These two run the
    // identical harness against the P3 vector adapter (embeddings external) and
    // the P4 graph adapter (edges external) — governance composing over a store
    // it does not own. A green run is the generalization proof for #4.
    #[tokio::test]
    async fn vector_adapter_passes_conformance() {
        let db = fresh_db().await;
        let store = BruteForceStore;
        let backend = VectorAdapter {
            inner: NativeBackend::new(&db, &store),
            index: InProcessVectorIndex::default(),
        };
        assert_conformance(&db, &backend).await;
    }

    #[tokio::test]
    async fn graph_adapter_passes_conformance() {
        let db = fresh_db().await;
        let store = BruteForceStore;
        let backend = GraphAdapter {
            inner: NativeBackend::new(&db, &store),
            graph: InProcessGraphStore::default(),
        };
        assert_conformance(&db, &backend).await;
    }

    // ── Live external-service integration (env-gated) ─────────────────────────
    // These run the IDENTICAL conformance suite against a REAL Qdrant / Neo4j.
    // They skip when the service env var is unset (so CI without the services
    // stays green); run with the containers up to prove the real adapters:
    //   IRONMEM_TEST_QDRANT_URL=http://localhost:6333
    //   IRONMEM_TEST_NEO4J_URL=http://localhost:7474  (+ _USER / _PASS)

    #[tokio::test]
    async fn qdrant_vector_adapter_passes_conformance() {
        let Ok(url) = std::env::var("IRONMEM_TEST_QDRANT_URL") else {
            eprintln!("SKIP qdrant integration: set IRONMEM_TEST_QDRANT_URL");
            return;
        };
        let db = fresh_db().await;
        let store = BruteForceStore;
        let collection = format!("conf_{}", uuid::Uuid::new_v4().simple());
        let backend =
            super::QdrantVectorBackend::new(NativeBackend::new(&db, &store), &url, &collection, 4)
                .await
                .expect("connect to live Qdrant");
        assert_conformance(&db, &backend).await;
    }

    #[tokio::test]
    async fn neo4j_graph_adapter_passes_conformance() {
        let Ok(url) = std::env::var("IRONMEM_TEST_NEO4J_URL") else {
            eprintln!("SKIP neo4j integration: set IRONMEM_TEST_NEO4J_URL");
            return;
        };
        let user = std::env::var("IRONMEM_TEST_NEO4J_USER").unwrap_or_else(|_| "neo4j".to_string());
        let pass =
            std::env::var("IRONMEM_TEST_NEO4J_PASS").unwrap_or_else(|_| "ironmempass".to_string());
        let db = fresh_db().await;
        let store = BruteForceStore;
        let backend = super::Neo4jGraphBackend::new(
            NativeBackend::new(&db, &store),
            &url,
            "neo4j",
            &user,
            &pass,
        );
        assert_conformance(&db, &backend).await;
    }
}
