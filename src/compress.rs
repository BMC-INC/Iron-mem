//! Shared session-compression pipeline. One implementation drives every
//! surface (MCP, REST, CLI) so importance scoring and inline embedding can
//! never drift between them. The LLM call and the persistence path are split
//! so the persistence half is unit-testable without network access.

use anyhow::Result;

use crate::config::Config;
use crate::db::{self, Database};
use crate::embedder::Embedder;
use crate::provider::{self, CompressionResult};
use crate::vectorstore::VectorStore;

/// Compress a session into a memory: summarize via the LLM, persist it, record
/// importance, and (best-effort) embed it for semantic recall.
pub async fn run(
    db: &Database,
    embedder: Option<&dyn Embedder>,
    store: &dyn VectorStore,
    cfg: &Config,
    session_id: &str,
) -> Result<i64> {
    let session = db::get_session(db, session_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Session not found: {}", session_id))?;

    let observations = db::get_observations_for_session(db, session_id).await?;
    let result = provider::compress(&observations, cfg).await?;

    let memory_id = persist(db, embedder, store, &session.project, session_id, result).await?;

    // CCR: preserve the verbatim pre-LLM transcript behind the lossy summary so
    // it can be retrieved later. Best-effort — never fail a successful
    // compression because the transcript blob could not be stored.
    if let Err(e) = store_session_transcript(db, memory_id, &observations).await {
        tracing::warn!("CCR session transcript store failed (memory {memory_id}): {e}");
    }

    // Feedback loop: mine error→fix corrections into error_solution memories.
    // Best-effort — never fail compression because mining hiccuped.
    if let Err(e) =
        crate::corrections::mine_and_store(db, embedder, store, &session.project, session_id, &observations)
            .await
    {
        tracing::warn!("correction mining failed (session {session_id}): {e}");
    }
    Ok(memory_id)
}

/// Render observations into a plain-text transcript (the pre-LLM session view).
fn build_transcript(observations: &[db::Observation]) -> String {
    let mut s = String::new();
    for o in observations {
        s.push_str("## ");
        s.push_str(&o.tool);
        s.push('\n');
        if let Some(input) = &o.input {
            s.push_str("input: ");
            s.push_str(input);
            s.push('\n');
        }
        if let Some(output) = &o.output {
            s.push_str("output: ");
            s.push_str(output);
            s.push('\n');
        }
        s.push('\n');
    }
    s
}

/// Store the verbatim session transcript as a CCR blob and link it to `memory_id`
/// via `memory_meta.session_blob`. Returns the blob hash, or `None` when there
/// were no observations to record.
pub async fn store_session_transcript(
    db: &Database,
    memory_id: i64,
    observations: &[db::Observation],
) -> Result<Option<String>> {
    let transcript = build_transcript(observations);
    if transcript.is_empty() {
        return Ok(None);
    }
    let hash = crate::ccr::store_blob(db, transcript.as_bytes(), None).await?.hash;
    db::set_memory_session_blob(db, memory_id, &hash).await?;
    Ok(Some(hash))
}

/// Persist an already-computed compression result. Inserts the memory, marks
/// the session compressed, records importance, then best-effort embeds the
/// memory (embedding failures are logged, never fatal — local-first posture).
pub async fn persist(
    db: &Database,
    embedder: Option<&dyn Embedder>,
    store: &dyn VectorStore,
    project: &str,
    session_id: &str,
    result: CompressionResult,
) -> Result<i64> {
    let memory_id =
        db::insert_memory(db, project, session_id, &result.summary, Some(&result.tags)).await?;
    db::mark_compressed(db, session_id).await?;
    db::upsert_memory_meta(db, memory_id, result.importance as f64 / 10.0).await?;
    // Compressed sessions are project-scoped; record the LLM-classified kind
    // (importance is preserved — set_memory_scope_kind only touches scope+kind).
    db::set_memory_scope_kind(db, memory_id, "project", &result.kind).await?;

    if let Some(emb) = embedder {
        let text = format!("{} {}", result.summary, result.tags);
        match emb.embed(&[text]).await {
            Ok(mut vecs) => {
                if let Some(vec) = vecs.drain(..).next() {
                    if let Err(e) = store.upsert(db, memory_id, emb.id(), emb.dim(), &vec).await {
                        tracing::warn!("inline embed upsert failed (memory {memory_id}): {e}");
                    }
                }
            }
            Err(e) => tracing::warn!("inline embed failed (memory {memory_id}): {e}"),
        }
    }

    // Dual-output compression: persist each extracted atomic fact as its own
    // searchable kind=fact memory in the same project/session. This bakes the
    // benchmark's separate "explicit fact" extraction into the write path so
    // specifics (dates, names, quantities) survive compression and resolve on
    // direct lookup. Best-effort per fact — a single failure is logged, never
    // fatal (local-first posture, matching the inline-embed handling above).
    for fact in &result.facts {
        persist_fact(
            db, embedder, store, project, session_id, memory_id, result.importance, fact,
        )
        .await;
    }

    tracing::info!(
        "Session {session_id} compressed → memory_id={memory_id} (+{} facts)",
        result.facts.len()
    );
    Ok(memory_id)
}

/// Persist one extracted fact as a `kind=fact`, project-scoped memory tied to the
/// originating session, inheriting the parent's importance and (best-effort)
/// carrying its own embedding. Errors are logged against `parent_id` and
/// swallowed so one bad fact never fails an otherwise-successful compression.
#[allow(clippy::too_many_arguments)] // each arg is an independent field of the fact memory
async fn persist_fact(
    db: &Database,
    embedder: Option<&dyn Embedder>,
    store: &dyn VectorStore,
    project: &str,
    session_id: &str,
    parent_id: i64,
    importance: u8,
    fact: &str,
) {
    let tags = format!("fact session:{session_id}");
    let fid = match db::insert_memory(db, project, session_id, fact, Some(&tags)).await {
        Ok(fid) => fid,
        Err(e) => {
            tracing::warn!("fact store failed (parent memory {parent_id}): {e}");
            return;
        }
    };
    if let Err(e) = db::upsert_memory_meta(db, fid, importance as f64 / 10.0).await {
        tracing::warn!("fact meta failed (fact memory {fid}): {e}");
    }
    if let Err(e) = db::set_memory_scope_kind(db, fid, "project", "fact").await {
        tracing::warn!("fact kind tag failed (fact memory {fid}): {e}");
    }
    if let Some(emb) = embedder {
        match emb.embed(&[fact.to_string()]).await {
            Ok(mut vecs) => {
                if let Some(vec) = vecs.drain(..).next() {
                    if let Err(e) = store.upsert(db, fid, emb.id(), emb.dim(), &vec).await {
                        tracing::warn!("fact embed upsert failed (fact memory {fid}): {e}");
                    }
                }
            }
            Err(e) => tracing::warn!("fact embed failed (fact memory {fid}): {e}"),
        }
    }
}

/// Store an explicit, user-curated memory (the Supermemory "add memory" pattern):
/// insert the memory + meta, tag it with scope/kind (both clamped to the known
/// sets), and best-effort embed it for semantic recall. Unlike compression there
/// is no session to summarize — `text` is stored verbatim as the memory.
/// `scope="user"` makes it a cross-project fact; `kind` classifies it. Returns
/// the new memory id.
#[allow(clippy::too_many_arguments)] // each arg is an independent field of the memory
pub async fn remember(
    db: &Database,
    embedder: Option<&dyn Embedder>,
    store: &dyn VectorStore,
    project: &str,
    scope: &str,
    kind: &str,
    text: &str,
    tags: Option<&str>,
) -> Result<i64> {
    // Explicit memories aren't tied to a compressed session; mark the origin so
    // they're distinguishable in session-history joins (no FK to sessions).
    let memory_id = db::insert_memory(db, project, "remember", text, tags).await?;
    // Deliberately curated → slightly above the neutral default importance.
    db::upsert_memory_meta(db, memory_id, 0.7).await?;
    db::set_memory_scope_kind(db, memory_id, scope, kind).await?;

    if let Some(emb) = embedder {
        let embed_text = match tags {
            Some(t) if !t.is_empty() => format!("{text} {t}"),
            _ => text.to_string(),
        };
        match emb.embed(&[embed_text]).await {
            Ok(mut vecs) => {
                if let Some(vec) = vecs.drain(..).next() {
                    if let Err(e) = store.upsert(db, memory_id, emb.id(), emb.dim(), &vec).await {
                        tracing::warn!("remember embed upsert failed (memory {memory_id}): {e}");
                    }
                }
            }
            Err(e) => tracing::warn!("remember embed failed (memory {memory_id}): {e}"),
        }
    }

    tracing::info!(
        "Remembered {}/{} memory → memory_id={memory_id} project={project}",
        db::clamp_scope(scope),
        db::clamp_kind(kind),
    );

    // Best-effort: keep the user profile fresh as cross-project memories grow.
    // Uses the deterministic local rollup (cfg=None → no network), so it never
    // blocks remember and never makes a surprise API call.
    if db::clamp_scope(scope) == "user" {
        let n = db::count_user_memories(db).await.unwrap_or(0);
        let no_profile = matches!(db::get_profile_memory(db).await, Ok(None));
        if no_profile || (n > 0 && n % crate::profile::PROFILE_REFRESH_EVERY == 0) {
            if let Err(e) = crate::profile::regenerate(db, embedder, store, None).await {
                tracing::warn!("profile auto-refresh failed: {e}");
            }
        }
    }

    Ok(memory_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::create_session;
    use crate::embedder::FakeEmbedder;
    use crate::vectorstore::SqliteVecStore;

    #[tokio::test]
    async fn persist_writes_memory_meta_and_embedding() {
        let path = std::env::temp_dir().join(format!("ironmem-cmp-{}.db", uuid::Uuid::new_v4()));
        let db = Database::new(&path.to_string_lossy()).await.unwrap();
        db.migrate().await.unwrap();
        db.ensure_ann(8).await.unwrap();
        let session = create_session(&db, "/tmp/p").await.unwrap();

        let emb = FakeEmbedder::new(8);
        let store = SqliteVecStore;
        let result = CompressionResult {
            summary: "implemented retrieval".into(),
            tags: "rust retrieval rrf".into(),
            importance: 8,
            kind: "architecture".into(),
            ..Default::default()
        };

        let id = persist(&db, Some(&emb), &store, "/tmp/p", &session, result)
            .await
            .unwrap();

        // Memory row exists.
        assert!(db::get_memory_by_id(&db, id).await.unwrap().is_some());
        // Importance persisted as 0.8 (8/10), and the classified kind landed on
        // the meta row at the default project scope.
        let info = db::get_memory_meta_full(&db, id).await.unwrap();
        assert!((info.importance - 0.8).abs() < 1e-9);
        assert_eq!((info.scope.as_str(), info.kind.as_str()), ("project", "architecture"));
        // Embedding persisted under the embedder's model id.
        assert!(db::get_embedding(&db, "memory", id, emb.id())
            .await
            .unwrap()
            .is_some());

        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn persist_stores_facts_as_searchable_memories() {
        let path = std::env::temp_dir().join(format!("ironmem-facts-{}.db", uuid::Uuid::new_v4()));
        let db = Database::new(&path.to_string_lossy()).await.unwrap();
        db.migrate().await.unwrap();
        db.ensure_ann(8).await.unwrap();
        let session = create_session(&db, "/tmp/p").await.unwrap();

        let emb = FakeEmbedder::new(8);
        let store = SqliteVecStore;
        let result = CompressionResult {
            summary: "Caroline attended community events".into(),
            tags: "caroline community".into(),
            importance: 6,
            kind: "session".into(),
            facts: vec![
                "Caroline joined the LGBTQ support group on 7 May 2023".into(),
                "Melanie painted a sunrise in 2022".into(),
            ],
            ..Default::default()
        };

        let id = persist(&db, Some(&emb), &store, "/tmp/p", &session, result)
            .await
            .unwrap();

        // Narrative memory exists.
        assert!(db::get_memory_by_id(&db, id).await.unwrap().is_some());

        // The date-bearing fact is retrievable by its date and tagged kind=fact,
        // as a memory distinct from the narrative.
        let hits = db::search_memories(&db, "/tmp/p", "7 May 2023", 10)
            .await
            .unwrap();
        let fact_hit = hits
            .iter()
            .find(|m| m.summary.contains("7 May 2023"))
            .expect("date fact must be retrievable by its date");
        assert_ne!(fact_hit.id, id, "fact is a memory distinct from the narrative");
        let meta = db::get_memory_meta_full(&db, fact_hit.id).await.unwrap();
        assert_eq!(meta.kind, "fact", "fact memory must be tagged kind=fact");

        // The fact also carries an embedding (semantic recall path).
        assert!(db::get_embedding(&db, "memory", fact_hit.id, emb.id())
            .await
            .unwrap()
            .is_some());

        // The second fact is its own memory too.
        let melanie = db::search_memories(&db, "/tmp/p", "Melanie sunrise", 10)
            .await
            .unwrap();
        assert!(melanie.iter().any(|m| m.summary.contains("Melanie")));

        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn persist_without_embedder_still_writes_meta() {
        let path = std::env::temp_dir().join(format!("ironmem-cmp2-{}.db", uuid::Uuid::new_v4()));
        let db = Database::new(&path.to_string_lossy()).await.unwrap();
        db.migrate().await.unwrap();
        let session = create_session(&db, "/tmp/p").await.unwrap();
        let store = crate::vectorstore::BruteForceStore;
        let result = CompressionResult {
            summary: "no embedder path".into(),
            tags: "fts only".into(),
            importance: 3,
            kind: "session".into(),
            ..Default::default()
        };

        let id = persist(&db, None, &store, "/tmp/p", &session, result)
            .await
            .unwrap();
        assert!(db::get_memory_by_id(&db, id).await.unwrap().is_some());
        assert!((db::get_memory_meta(&db, id).await.unwrap() - 0.3).abs() < 1e-9);
        // No embedding written.
        assert!(db::get_embedding(&db, "memory", id, "fake")
            .await
            .unwrap()
            .is_none());
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn remember_writes_typed_memory_and_embeds() {
        let path = std::env::temp_dir().join(format!("ironmem-rem-{}.db", uuid::Uuid::new_v4()));
        let db = Database::new(&path.to_string_lossy()).await.unwrap();
        db.migrate().await.unwrap();
        db.ensure_ann(8).await.unwrap();
        let emb = FakeEmbedder::new(8);
        let store = SqliteVecStore;

        // A user-scope preference stored from project A.
        let id = remember(
            &db,
            Some(&emb),
            &store,
            "/tmp/projA",
            "user",
            "preference",
            "prefers tabs over spaces",
            Some("style editor"),
        )
        .await
        .unwrap();

        // scope/kind landed; importance bumped above the neutral default.
        let info = db::get_memory_meta_full(&db, id).await.unwrap();
        assert_eq!((info.scope.as_str(), info.kind.as_str()), ("user", "preference"));
        assert!((info.importance - 0.7).abs() < 1e-9);
        // Embedding written under the embedder's model id.
        assert!(db::get_embedding(&db, "memory", id, emb.id())
            .await
            .unwrap()
            .is_some());
        // Retrievable via the global user scope, irrespective of which project
        // it was created in (the cross-project guarantee).
        let users = db::get_recent_memories_scoped(&db, "user", None, 10).await.unwrap();
        assert!(users.iter().any(|m| m.id == id), "user memory must be globally visible");
        // It must NOT appear under another project's project-scope view.
        let proj_b = db::get_recent_memories_scoped(&db, "project", Some("/tmp/projB"), 10)
            .await
            .unwrap();
        assert!(!proj_b.iter().any(|m| m.id == id));

        // Unknown scope/kind clamp to the safe defaults; no embedder is fine.
        let id2 = remember(&db, None, &store, "/tmp/projB", "bogus", "bogus", "x", None)
            .await
            .unwrap();
        let info2 = db::get_memory_meta_full(&db, id2).await.unwrap();
        assert_eq!((info2.scope.as_str(), info2.kind.as_str()), ("project", "session"));

        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn store_session_transcript_round_trips() {
        let path = std::env::temp_dir().join(format!("ironmem-cmp3-{}.db", uuid::Uuid::new_v4()));
        let db = Database::new(&path.to_string_lossy()).await.unwrap();
        db.migrate().await.unwrap();
        let session = create_session(&db, "/tmp/p").await.unwrap();
        let store = crate::vectorstore::BruteForceStore;

        db::insert_observation(&db, &session, "/tmp/p", "Read", Some("src/main.rs"), Some("fn main(){}"), 2048)
            .await
            .unwrap();
        db::insert_observation(&db, &session, "/tmp/p", "Bash", Some("cargo test"), Some("ok"), 2048)
            .await
            .unwrap();
        let observations = db::get_observations_for_session(&db, &session).await.unwrap();

        let result = CompressionResult {
            summary: "s".into(),
            tags: "t".into(),
            importance: 5,
            kind: "session".into(),
            ..Default::default()
        };
        let memory_id = persist(&db, None, &store, "/tmp/p", &session, result)
            .await
            .unwrap();

        let hash = store_session_transcript(&db, memory_id, &observations)
            .await
            .unwrap()
            .expect("transcript stored");

        // Linked on the memory and retrievable byte-exact.
        assert_eq!(
            db::get_memory_session_blob(&db, memory_id).await.unwrap(),
            Some(hash.clone())
        );
        let restored = crate::ccr::load_blob(&db, &hash).await.unwrap();
        let expected = build_transcript(&observations);
        assert_eq!(String::from_utf8(restored).unwrap(), expected);
        assert!(expected.contains("## Read"));

        // No observations → nothing stored.
        assert!(store_session_transcript(&db, memory_id, &[])
            .await
            .unwrap()
            .is_none());

        let _ = std::fs::remove_file(path);
    }
}
