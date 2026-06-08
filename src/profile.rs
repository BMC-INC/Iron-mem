//! User-profile extraction. Distills the user's cross-project (`scope=user`)
//! memories into a single durable `kind=profile` memory — stable facts plus
//! recent activity — that is always injected at session start (mirrors how
//! Claude Code's `MEMORY.md` separates durable user facts from project facts).
//!
//! Regeneration uses one LLM call when a provider/API key is available, and a
//! deterministic local rollup otherwise, so it degrades gracefully and never
//! blocks (local-first / no-egress posture). The profile is a singleton: each
//! regeneration replaces the previous profile memory.

use anyhow::Result;

use crate::config::Config;
use crate::db::{self, Database, Memory};
use crate::embedder::Embedder;
use crate::provider;
use crate::vectorstore::{self, VectorStore};

/// Sentinel project for the (global) profile memory. Keeps it out of every real
/// project's unscoped recent-memory list; scope=user makes it globally visible.
pub const PROFILE_PROJECT: &str = "ironmem:profile";

/// Max user memories fed to the profile generator.
const PROFILE_SOURCE_LIMIT: i64 = 100;

/// Max bullets in the deterministic rollup.
const PROFILE_MAX_BULLETS: usize = 25;

/// Regenerate the profile after this many user-memory writes (best-effort).
pub const PROFILE_REFRESH_EVERY: i64 = 5;

/// Regenerate the singleton user profile from `scope=user` memories. Returns the
/// new profile memory id, or `None` when there are no user memories to profile.
///
/// `cfg = Some(_)` attempts an LLM summary (falling back to the local rollup on
/// any error); `cfg = None` uses the deterministic rollup directly (no network).
pub async fn regenerate(
    db: &Database,
    embedder: Option<&dyn Embedder>,
    store: &dyn VectorStore,
    cfg: Option<&Config>,
) -> Result<Option<i64>> {
    // Source = user memories that are not themselves the profile.
    let user_mems = db::get_recent_memories_scoped(db, "user", None, PROFILE_SOURCE_LIMIT).await?;
    let mut sources = Vec::new();
    let mut stale = Vec::new();
    for m in user_mems {
        if db::get_memory_meta_full(db, m.id).await?.kind == "profile" {
            stale.push(m.id);
        } else {
            sources.push(m);
        }
    }
    if sources.is_empty() {
        return Ok(None);
    }

    let text = match cfg {
        Some(c) => build_profile_text(&sources, c).await,
        None => deterministic_profile(&sources),
    };

    // Replace any prior profile(s) so exactly one exists.
    for id in stale {
        let _ = db::decref_memory_session_blob(db, id).await;
        db::delete_memory(db, id).await?;
        let _ = vectorstore::purge_memory(db, store, id).await;
    }

    let id = db::insert_memory(db, PROFILE_PROJECT, "profile", &text, Some("profile user")).await?;
    db::upsert_memory_meta(db, id, 0.9).await?; // the profile is high-importance
    db::set_memory_scope_kind(db, id, "user", "profile").await?;

    if let Some(emb) = embedder {
        match emb.embed(std::slice::from_ref(&text)).await {
            Ok(mut v) => {
                if let Some(vec) = v.drain(..).next() {
                    if let Err(e) = store.upsert(db, id, emb.id(), emb.dim(), &vec).await {
                        tracing::warn!("profile embed upsert failed (memory {id}): {e}");
                    }
                }
            }
            Err(e) => tracing::warn!("profile embed failed (memory {id}): {e}"),
        }
    }

    tracing::info!(
        "Regenerated user profile → memory_id={id} ({} sources)",
        sources.len()
    );
    Ok(Some(id))
}

/// Build the profile text: an LLM summary when reachable, else the local rollup.
async fn build_profile_text(sources: &[Memory], cfg: &Config) -> String {
    match provider::complete(&profile_prompt(sources), cfg).await {
        Ok(t) if !t.trim().is_empty() => t.trim().to_string(),
        Ok(_) => deterministic_profile(sources),
        Err(e) => {
            tracing::warn!("profile LLM unavailable, using local rollup: {e}");
            deterministic_profile(sources)
        }
    }
}

fn profile_prompt(sources: &[Memory]) -> String {
    let mut p = String::from(
        "You maintain a durable USER PROFILE for a software developer. From the memories below \
         (the user's explicit preferences and cross-project facts), write a concise profile in \
         markdown with two short sections: '## Stable facts' (durable preferences, tools, working \
         style) and '## Recent activity' (what they have focused on lately). Be specific and \
         terse. Output only the profile markdown.\n\nUSER MEMORIES:\n",
    );
    for (i, m) in sources.iter().enumerate() {
        p.push_str(&format!(
            "{}. {}\n",
            i + 1,
            crate::strutil::safe_truncate(m.summary.trim(), 300)
        ));
    }
    p
}

/// Deterministic, no-network profile: the user's most-recent memories as a
/// bulleted rollup. Always available, so profile generation never blocks.
fn deterministic_profile(sources: &[Memory]) -> String {
    let mut out = String::from(
        "# User Profile (auto-generated)\n\nDurable facts and recent activity distilled from the \
         user's cross-project memories:\n\n",
    );
    for m in sources.iter().take(PROFILE_MAX_BULLETS) {
        out.push_str("- ");
        out.push_str(&crate::strutil::safe_truncate(m.summary.trim(), 240));
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::create_session;
    use crate::vectorstore::BruteForceStore;

    async fn test_db() -> (Database, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!("ironmem-prof-{}.db", uuid::Uuid::new_v4()));
        let db = Database::new(&path.to_string_lossy()).await.unwrap();
        db.migrate().await.unwrap();
        (db, path)
    }

    /// Seed `n` user-scope preference memories.
    async fn seed_user_prefs(db: &Database, n: usize) {
        let s = create_session(db, "/tmp/p").await.unwrap();
        for i in 0..n {
            let id = db::insert_memory(db, "/tmp/p", &s, &format!("user fact number {i}"), Some("pref"))
                .await
                .unwrap();
            db::set_memory_scope_kind(db, id, "user", "preference").await.unwrap();
        }
    }

    #[tokio::test]
    async fn regenerate_builds_singleton_profile_from_user_memories() {
        let (db, path) = test_db().await;
        let store = BruteForceStore;
        seed_user_prefs(&db, 3).await;

        // Local (deterministic) regeneration — no network.
        let id = regenerate(&db, None, &store, None).await.unwrap().expect("profile created");
        let info = db::get_memory_meta_full(&db, id).await.unwrap();
        assert_eq!((info.scope.as_str(), info.kind.as_str()), ("user", "profile"));

        let prof = db::get_profile_memory(&db).await.unwrap().expect("profile retrievable");
        assert_eq!(prof.id, id);
        assert!(prof.summary.contains("user fact number"));

        // Regenerating again replaces it — still exactly one profile (the row id
        // may be reused by SQLite, so assert the singleton, not id inequality).
        let id2 = regenerate(&db, None, &store, None).await.unwrap().unwrap();
        assert_eq!(
            db::get_profile_memory(&db).await.unwrap().unwrap().id,
            id2,
            "the live profile is the freshly regenerated one"
        );
        let all_profiles =
            db::get_recent_memories_scoped(&db, "user", None, 100).await.unwrap();
        let n_profiles = {
            let mut c = 0;
            for m in &all_profiles {
                if db::get_memory_meta_full(&db, m.id).await.unwrap().kind == "profile" {
                    c += 1;
                }
            }
            c
        };
        assert_eq!(n_profiles, 1, "profile is a singleton");
        // Source preferences (3) are untouched; profile excluded from its own input.
        assert_eq!(db::count_user_memories(&db).await.unwrap(), 3);

        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn regenerate_with_no_user_memories_is_noop() {
        let (db, path) = test_db().await;
        let store = BruteForceStore;
        assert!(regenerate(&db, None, &store, None).await.unwrap().is_none());
        assert!(db::get_profile_memory(&db).await.unwrap().is_none());
        let _ = std::fs::remove_file(path);
    }
}
