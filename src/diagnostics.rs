//! An administrative preview of the real lexical retrieval and influence path.
//! Rank means lexical candidate position, never an invented semantic score.
use crate::{access, config::Config, db, egress, influence::PolicyPrincipal};
use anyhow::{ensure, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::Row;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub namespace: String,
    pub project: String,
    pub query: Option<String>,
    pub memory_id: Option<i64>,
    #[serde(default = "budget")]
    pub budget_bytes: usize,
    #[serde(default)]
    pub include_content: bool,
    pub purpose: Option<crate::purpose::RecallPurpose>,
}
fn budget() -> usize {
    24000
}

pub fn schema() -> Value {
    json!({"type":"object","properties":{
        "namespace":{"type":"string"},"project":{"type":"string"},
        "query":{"type":"string"},"memory_id":{"type":"integer","minimum":1},
        "budget_bytes":{"type":"integer","minimum":2000,"maximum":24000},
        "include_content":{"type":"boolean"},"purpose":{"type":"object"}
    },"required":["namespace","project"],"oneOf":[{"required":["query"]},{"required":["memory_id"]}],"additionalProperties":false})
}

pub async fn handle(
    db: &db::Database,
    cfg: &Config,
    principal: &PolicyPrincipal,
    request: Request,
) -> Result<Value> {
    let namespace = crate::governance::normalize_namespace(&request.namespace);
    principal.authorize("diagnostics:read", &namespace)?;
    ensure!(
        !request.project.is_empty() && request.project.len() <= 4096,
        "invalid project"
    );
    ensure!(
        (2000..=24000).contains(&request.budget_bytes),
        "budget must be 2000..=24000 bytes"
    );
    ensure!(
        request.query.is_some() != request.memory_id.is_some(),
        "provide exactly one query or memory_id"
    );
    let before = crate::hooks::context_revision(db).await?;
    let candidates = if let Some(query) = &request.query {
        ensure!(
            !query.trim().is_empty() && query.len() <= 4096,
            "query must contain 1..=4096 bytes"
        );
        db::search_memories_in_namespace(db, &namespace, &request.project, query, 100).await?
    } else {
        let id = request.memory_id.unwrap();
        ensure!(id > 0, "invalid memory ID");
        let memory = db::get_memory_by_id_in_namespace(db, id, &namespace)
            .await?
            .filter(|m| m.project == request.project)
            .ok_or_else(|| anyhow::anyhow!("memory not found in active scope"))?;
        vec![memory]
    };
    let ids: Vec<_> = candidates.iter().map(|m| m.id).collect();
    let channel = if principal.authority == "local_operator" {
        egress::PurposeChannel::LocalOperator(principal.actor.clone())
    } else {
        egress::PurposeChannel::Remote {
            authenticated_agent: if principal.authority == "authenticated_agent" {
                Some(
                    principal
                        .actor
                        .strip_prefix("agent:")
                        .unwrap_or(&principal.actor)
                        .to_string(),
                )
            } else {
                None
            },
        }
    };
    let gate = egress::gate_memories_with_query(
        db,
        candidates.clone(),
        &namespace,
        &request.project,
        request.purpose.as_ref(),
        channel,
        egress::ConsumerCapabilities {
            reasoning_only_channel: true,
            exact_source_expansion: false,
            denial_diagnostics: true,
        },
        &cfg.influence,
        request.query.as_deref(),
    )
    .await?;
    // Preserve lexical order across allowed/advisory channels. Advisory content
    // remains labeled separately; a plain context string must not erase that label.
    let allowed: Vec<_> = candidates
        .iter()
        .filter(|m| gate.authorized.iter().any(|a| a.id == m.id))
        .cloned()
        .collect();
    let (context, rendered) = crate::hooks::render_memories(&allowed, request.budget_bytes);
    let stats = access::stats(db, &ids).await?;
    let mut entries = Vec::new();
    for (rank, memory) in candidates.iter().enumerate() {
        let meta = db::get_memory_meta_full(db, memory.id).await?;
        let decision = gate.decisions.iter().find(|d| d.memory_id == memory.id);
        let source = db::get_memory_session_blob(db, memory.id).await?;
        let source_available = if let Some(hash) = &source {
            sqlx::query("SELECT hash FROM blobs WHERE hash=$1")
                .bind(hash)
                .fetch_optional(&db.pool)
                .await?
                .is_some()
        } else {
            false
        };
        let state = if gate.authorized.iter().any(|m| m.id == memory.id) {
            "allowed"
        } else if gate.advisory.iter().any(|m| m.id == memory.id) {
            "reasoning_only"
        } else if gate.source_required.iter().any(|m| m.id == memory.id) {
            "source_required"
        } else {
            "withheld"
        };
        entries.push(json!({"memory_id":memory.id,"candidate_rank":rank+1,"disposition":state,
            "decision":decision,"stats":stats.get(&memory.id),
            "temperature":stats.get(&memory.id).map(|s| crate::working_set::temperature(s, chrono::Utc::now().timestamp())),
            "source":{"reference_present":source.is_some(),"object_present":source_available,"reconstruction_verified":false,"hash":if state == "allowed" { source.as_deref() } else { None }},
            "lineage":{"evidence_root_id":meta.evidence_root_id,"derivation_depth":meta.derivation_depth,"source_type":meta.source_type,"has_parent":meta.parent_memory_id.is_some()},
            "retention":{"expires_at":meta.expires_at,"legal_hold":meta.legal_hold,
                "forget_semantics":"removes active memory; assertion history, audit, session sources and snapshots may retain data"},
            "in_context":rendered.written_ids.contains(&memory.id)}));
    }
    let registration = if !ids.is_empty() {
        sqlx::query("SELECT content_hash,dirty FROM generated_context_files WHERE project=$1")
            .bind(&request.project)
            .fetch_optional(&db.pool)
            .await?
    } else {
        None
    };
    let freshness = if let Some(row) = registration {
        let expected: String = row.get("content_hash");
        let dirty: i64 = row.get("dirty");
        let disk = if principal.authority == "local_operator" {
            match std::fs::read(std::path::Path::new(&request.project).join("IRONMEM.md")) {
                Ok(bytes) => {
                    if format!("{:x}", Sha256::digest(bytes)) == expected {
                        "matches_registration"
                    } else {
                        "user_edited_or_replaced"
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => "missing",
                Err(_) => "unreadable",
            }
        } else {
            "not_inspected_on_remote_request"
        };
        json!({"registered":true,"dirty":dirty>0,"disk":disk})
    } else {
        json!({"registered":false,"freshness":"unknown"})
    };
    let after = crate::hooks::context_revision(db).await?;
    // Do not release a preview if a mutation raced with policy/ranking.
    ensure!(before == after, "memory changed during diagnosis; retry");
    if request.include_content {
        access::delivered(db, access::Delivery::Recall, &rendered.written_ids, None).await;
    }
    Ok(
        json!({"schema":1,"retrieval":"lexical_search_preview","candidate_limit":100,
        "scope":{"namespace":namespace,"project":request.project},"revision":after,
        "runtime":{"version":env!("CARGO_PKG_VERSION"),"influence_enabled":cfg.influence.enabled,"working_sets_enabled":cfg.working_set.enabled,"vector_backend":cfg.storage.vector_backend,"graph_backend":cfg.storage.graph_backend},
        "entries":entries,"generated_context":freshness,"render":rendered,
        "context":if request.include_content { Some(context) } else { None },
        "notes":["This is a fresh preview, not a reconstruction of a past response.",
            "Candidates absent from lexical top 100 have no inferred rejection reason.",
            "Reasoning-only content is omitted from the plain context preview.",
            "Policy decision receipts may be recorded; access counters change only when content is returned."]}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn diagnostics_scope_budget_and_no_content() {
        let dir = tempfile::tempdir().unwrap();
        let database = db::Database::new(&dir.path().join("test.db").to_string_lossy())
            .await
            .unwrap();
        database.migrate().await.unwrap();
        let session = db::create_session(&database, "fixture").await.unwrap();
        let id = db::insert_memory(
            &database,
            "fixture",
            &session,
            "diagnostic canary",
            Some("canary"),
        )
        .await
        .unwrap();
        let request = || Request {
            namespace: "local".into(),
            project: "fixture".into(),
            query: Some("canary".into()),
            memory_id: None,
            budget_bytes: 2000,
            include_content: false,
            purpose: None,
        };
        let principal = PolicyPrincipal::local_operator("test");
        let mut cfg = Config::default();
        cfg.influence.enabled = true;
        let report = handle(&database, &cfg, &principal, request())
            .await
            .unwrap();
        assert!(report["context"].is_null());
        assert_eq!(report["entries"][0]["memory_id"], id);
        assert!(!report.to_string().contains("diagnostic canary"));
        let outsider = PolicyPrincipal::configured(
            "remote",
            "shared",
            vec!["other".into()],
            vec!["diagnostics:read".into()],
        );
        assert!(handle(&database, &cfg, &outsider, request()).await.is_err());
        let mut invalid = request();
        invalid.budget_bytes = 1999;
        assert!(handle(&database, &cfg, &principal, invalid).await.is_err());
        // An explicit content request must still withhold blocked evidence,
        // while explaining the decision without recording a recall.
        db::update_memory_influence_policy(
            &database,
            id,
            "local",
            &principal,
            &crate::influence::PolicyMutationRequest {
                expected_version: 1,
                patch: crate::influence::MemoryInfluencePolicyPatch {
                    state: Some(crate::influence::InfluenceState::Blocked),
                    ..Default::default()
                },
                reason: "test restriction".into(),
                request_id: "diagnostic-block".into(),
            },
        )
        .await
        .unwrap();
        let mut content = request();
        content.include_content = true;
        let blocked = handle(&database, &cfg, &principal, content).await.unwrap();
        assert_eq!(blocked["entries"][0]["disposition"], "withheld");
        assert!(!blocked.to_string().contains("diagnostic canary"));
        assert_eq!(
            access::stats(&database, &[id]).await.unwrap()[&id].recall_count,
            0
        );
        database.pool.close().await;
    }
}
