//! Opt-in structured claims. Events are immutable through the public API; current
//! pointers are derived, never selected by popularity or last-writer-wins.
use crate::{
    db::{self, Backend, Database},
    egress,
    influence::PolicyPrincipal,
};
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::Row;

pub const TABLE: &str = "assertion_events";
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub namespace: String,
    pub project: String,
    pub subject: String,
    pub predicate: String,
}
impl Scope {
    fn validate(&self) -> Result<()> {
        for (name, value, max) in [
            ("namespace", &self.namespace, 128),
            ("project", &self.project, 4096),
            ("subject", &self.subject, 256),
            ("predicate", &self.predicate, 128),
        ] {
            ensure!(
                !value.is_empty()
                    && value.trim() == value
                    && value.len() <= max
                    && !value.chars().any(char::is_control),
                "invalid assertion {name}"
            );
        }
        ensure!(
            crate::governance::normalize_namespace(&self.namespace) == self.namespace,
            "namespace must be canonical"
        );
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Write {
        scope: Scope,
        expected_version: i64,
        memory_id: i64,
        value: Option<String>,
        valid_from: i64,
        #[serde(default)]
        valid_until: Option<i64>,
        #[serde(default)]
        supersedes: Option<String>,
    },
    Query {
        scope: Scope,
        #[serde(default)]
        valid_at: Option<i64>,
        #[serde(default)]
        recorded_before: Option<i64>,
        #[serde(default)]
        history: bool,
        #[serde(default)]
        purpose: Option<crate::purpose::RecallPurpose>,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub scope: Scope,
    pub version: i64,
    pub memory_id: i64,
    pub evidence_hash: String,
    pub value: Option<String>,
    pub valid_from: i64,
    pub valid_until: Option<i64>,
    pub supersedes: Option<String>,
    pub recorded_at: i64,
    pub actor: String,
    pub previous_hash: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub id: String,
    #[serde(flatten)]
    pub event: Event,
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn event_id(event: &Event) -> Result<String> {
    Ok(hash(&serde_json::to_vec(event)?))
}

/// Shared machine-readable request contract for MCP clients.
pub fn request_schema() -> serde_json::Value {
    let scope = serde_json::json!({"type":"object","properties":{"namespace":{"type":"string"},"project":{"type":"string"},"subject":{"type":"string"},"predicate":{"type":"string"}},"required":["namespace","project","subject","predicate"],"additionalProperties":false});
    serde_json::json!({"oneOf":[
        {"type":"object","properties":{"op":{"const":"write"},"scope":scope,"expected_version":{"type":"integer","minimum":0},"memory_id":{"type":"integer"},"value":{"type":["string","null"],"maxLength":4096},"valid_from":{"type":"integer","description":"Unix seconds, inclusive"},"valid_until":{"type":["integer","null"],"description":"Unix seconds, exclusive"},"supersedes":{"type":["string","null"]}},"required":["op","scope","expected_version","memory_id","value","valid_from"],"additionalProperties":false},
        {"type":"object","properties":{"op":{"const":"query"},"scope":scope,"valid_at":{"type":["integer","null"]},"recorded_before":{"type":["integer","null"],"description":"Unix milliseconds, inclusive"},"history":{"type":"boolean"},"purpose":{"type":["object","null"]}},"required":["op","scope"],"additionalProperties":false}
    ]})
}

pub async fn migrate(db: &Database) -> Result<()> {
    sqlx::query("CREATE TABLE IF NOT EXISTS assertion_events(id TEXT PRIMARY KEY,project TEXT NOT NULL,namespace TEXT NOT NULL,subject TEXT NOT NULL,predicate TEXT NOT NULL,version BIGINT NOT NULL,memory_id BIGINT NOT NULL,recorded_at BIGINT NOT NULL,data TEXT NOT NULL,UNIQUE(namespace,project,subject,predicate,version))").execute(&db.pool).await?;
    // No cascading evidence FK: removal makes a claim ineligible, not rewritten history.
    Ok(())
}
async fn entries(conn: &mut sqlx::AnyConnection, scope: &Scope) -> Result<Vec<Entry>> {
    let rows = sqlx::query("SELECT id,data FROM assertion_events WHERE namespace=$1 AND project=$2 AND subject=$3 AND predicate=$4 ORDER BY version")
        .bind(&scope.namespace).bind(&scope.project).bind(&scope.subject).bind(&scope.predicate).fetch_all(conn).await?;
    let result = rows
        .into_iter()
        .map(|r| {
            Ok(Entry {
                id: r.get("id"),
                event: serde_json::from_str(&r.get::<String, _>("data"))?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    validate_chain(&result)?;
    Ok(result)
}
fn validate_chain(entries: &[Entry]) -> Result<()> {
    let mut previous: Option<&Entry> = None;
    for entry in entries {
        let event = &entry.event;
        event.scope.validate()?;
        ensure!(event_id(event)? == entry.id, "assertion hash mismatch");
        ensure!(
            event.version == previous.map_or(1, |p| p.event.version + 1)
                && event.previous_hash.as_deref() == previous.map(|p| p.id.as_str()),
            "assertion history is not contiguous"
        );
        ensure!(
            previous.is_none_or(
                |p| p.event.scope == event.scope && p.event.recorded_at <= event.recorded_at
            ),
            "assertion scope or clock mismatch"
        );
        ensure!(
            event.valid_until.is_none_or(|end| end > event.valid_from),
            "invalid validity interval"
        );
        ensure!(
            event
                .value
                .as_ref()
                .is_none_or(|v| !v.trim().is_empty() && v.len() <= 4096),
            "invalid assertion value"
        );
        ensure!(
            event.value.is_some() || event.supersedes.is_some(),
            "retraction requires a target"
        );
        if let Some(target) = &event.supersedes {
            ensure!(
                entries.iter().any(|p| &p.id == target
                    && p.event.version < event.version
                    && p.event.value.is_some()
                    && p.event.valid_from <= event.valid_from),
                "invalid supersession target"
            );
        }
        previous = Some(entry);
    }
    Ok(())
}
/// Verify row projections too: an envelope hash alone does not prove semantic validity.
pub fn validate_checkpoint(table: &crate::checkpoint::Table) -> Result<()> {
    let mut scopes = std::collections::BTreeMap::<String, Vec<Entry>>::new();
    for row in &table.rows {
        ensure!(
            [
                "id",
                "project",
                "namespace",
                "subject",
                "predicate",
                "version",
                "memory_id",
                "recorded_at",
                "data"
            ]
            .iter()
            .all(|k| row.contains_key(*k)),
            "incomplete assertion row"
        );
        let event: Event = serde_json::from_str(
            row["data"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("invalid assertion data"))?,
        )?;
        for (field, value) in [
            ("project", serde_json::json!(event.scope.project)),
            ("namespace", serde_json::json!(event.scope.namespace)),
            ("subject", serde_json::json!(event.scope.subject)),
            ("predicate", serde_json::json!(event.scope.predicate)),
            ("version", serde_json::json!(event.version)),
            ("memory_id", serde_json::json!(event.memory_id)),
            ("recorded_at", serde_json::json!(event.recorded_at)),
        ] {
            ensure!(
                row.get(field) == Some(&value),
                "assertion row projection mismatch"
            );
        }
        let id = row["id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("invalid assertion ID"))?
            .to_owned();
        scopes
            .entry(serde_json::to_string(&event.scope)?)
            .or_default()
            .push(Entry { id, event });
    }
    for entries in scopes.values_mut() {
        entries.sort_by_key(|e| e.event.version);
        validate_chain(entries)?;
    }
    Ok(())
}

pub async fn handle(
    db: &Database,
    cfg: &crate::config::Config,
    principal: &PolicyPrincipal,
    request: Request,
) -> Result<serde_json::Value> {
    ensure!(
        cfg.assertions.enabled,
        "structured assertions are disabled; set assertions.enabled=true to opt in"
    );
    let (scope, capability) = match &request {
        Request::Write { scope, .. } => (scope, "assertions:write"),
        Request::Query { scope, .. } => (scope, "assertions:read"),
    };
    scope.validate()?;
    principal.authorize(capability, &scope.namespace)?;
    match request {
        Request::Write {
            scope,
            expected_version,
            memory_id,
            value,
            valid_from,
            valid_until,
            supersedes,
        } => {
            ensure!(
                (0..i64::MAX).contains(&expected_version),
                "invalid expected_version"
            );
            let mut tx = db::begin_write(db).await?;
            if matches!(db.backend, Backend::Postgres) {
                // Same namespace lock as the governance ledger, acquired before state reads.
                sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
                    .bind(&scope.namespace)
                    .execute(&mut *tx)
                    .await?;
            }
            let mut history = entries(&mut tx, &scope).await?;
            let version = history.last().map_or(0, |p| p.event.version);
            ensure!(
                version == expected_version,
                "assertion version conflict: expected {expected_version}, current {version}"
            );
            // Read evidence on the same transaction as the append. Existing source bytes/IDs remain intact.
            let id_col = if matches!(db.backend, Backend::Sqlite) {
                "rowid"
            } else {
                "id"
            };
            let row = sqlx::query(&format!("SELECT m.summary FROM memories m JOIN memory_meta mm ON mm.memory_id=m.{id_col} WHERE m.{id_col}=$1 AND m.project=$2 AND mm.namespace=$3 AND mm.tombstoned_at IS NULL AND (mm.expires_at IS NULL OR mm.expires_at>$4)"))
                .bind(memory_id).bind(&scope.project).bind(&scope.namespace).bind(chrono::Utc::now().timestamp()).fetch_optional(&mut *tx).await?.ok_or_else(|| anyhow::anyhow!("active scoped evidence memory required"))?;
            let event = Event {
                scope: scope.clone(),
                version: version + 1,
                memory_id,
                evidence_hash: hash(row.get::<String, _>("summary").as_bytes()),
                value,
                valid_from,
                valid_until,
                supersedes,
                recorded_at: chrono::Utc::now()
                    .timestamp_millis()
                    .max(history.last().map_or(0, |p| p.event.recorded_at)),
                actor: principal.actor.clone(),
                previous_hash: history.last().map(|p| p.id.clone()),
            };
            let entry = Entry {
                id: event_id(&event)?,
                event,
            };
            history.push(entry.clone());
            validate_chain(&history)?;
            let data = serde_json::to_string(&entry.event)?;
            sqlx::query("INSERT INTO assertion_events(id,project,namespace,subject,predicate,version,memory_id,recorded_at,data) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)")
                .bind(&entry.id).bind(&scope.project).bind(&scope.namespace).bind(&scope.subject).bind(&scope.predicate).bind(entry.event.version).bind(memory_id).bind(entry.event.recorded_at).bind(&data).execute(&mut *tx).await?;
            db::append_memory_ledger_on_connection(&mut tx,&scope.namespace,Some(memory_id),"assertion_append",Some(&principal.actor), &serde_json::json!({"assertion_id":entry.id,"version":entry.event.version,"previous_hash":entry.event.previous_hash}).to_string(),None).await?;
            sqlx::query("UPDATE context_revision SET revision=revision+1 WHERE singleton=1")
                .execute(&mut *tx)
                .await?;
            sqlx::query("UPDATE generated_context_files SET dirty=1 WHERE project=$1")
                .bind(&scope.project)
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "UPDATE memory_mutations SET mutation_count=mutation_count+1 WHERE memory_id=$1",
            )
            .bind(memory_id)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            if let Err(error) = crate::hooks::invalidate_generated_files(db).await {
                tracing::warn!(%error,"assertion committed; context invalidation queued");
            }
            Ok(
                serde_json::json!({"id":entry.id,"version":entry.event.version,"recorded_at":entry.event.recorded_at}),
            )
        }
        Request::Query {
            scope,
            valid_at,
            recorded_before,
            history,
            purpose,
        } => {
            let now = chrono::Utc::now();
            let valid_at = valid_at.unwrap_or(now.timestamp());
            let recorded_before = recorded_before.unwrap_or(now.timestamp_millis());
            let mut conn = db.pool.acquire().await?;
            let all = entries(&mut conn, &scope).await?;
            drop(conn);
            let version = all.last().map_or(0, |e| e.event.version);
            let known: Vec<_> = all
                .into_iter()
                .filter(|e| e.event.recorded_at <= recorded_before)
                .collect();
            let candidates: Vec<_> = known
                .iter()
                .filter(|e| {
                    history
                        || (e.event.value.is_some()
                            && e.event.valid_from <= valid_at
                            && e.event.valid_until.is_none_or(|end| valid_at < end)
                            && !known.iter().any(|new| {
                                new.event.supersedes.as_deref() == Some(&e.id)
                                    && new.event.valid_from <= valid_at
                            }))
                })
                .collect();
            let mut visible = Vec::new();
            let mut withheld = false;
            for entry in candidates {
                let Some(memory) =
                    db::get_memory_by_id_in_namespace(db, entry.event.memory_id, &scope.namespace)
                        .await?
                else {
                    withheld = true;
                    continue;
                };
                if memory.project != scope.project
                    || hash(memory.summary.as_bytes()) != entry.event.evidence_hash
                {
                    withheld = true;
                    continue;
                }
                let channel = if principal.authority == "local_operator" {
                    egress::PurposeChannel::LocalOperator(principal.actor.clone())
                } else {
                    egress::PurposeChannel::Remote {
                        authenticated_agent: (principal.authority == "authenticated_agent")
                            .then(|| principal.actor.trim_start_matches("agent:").to_owned()),
                    }
                };
                let gate = egress::gate_memories(
                    db,
                    vec![memory],
                    &scope.namespace,
                    &scope.project,
                    purpose.as_ref(),
                    channel,
                    egress::ConsumerCapabilities {
                        reasoning_only_channel: true,
                        ..Default::default()
                    },
                    &cfg.influence,
                )
                .await?;
                if gate.authorized.is_empty() && gate.advisory.is_empty() {
                    withheld = true;
                    continue;
                }
                visible.push(serde_json::json!({"assertion":entry,"reasoning_only":!gate.advisory.is_empty()}));
            }
            let current_id = if !history && !withheld && visible.len() == 1 {
                visible[0]["assertion"]["id"].as_str().map(str::to_owned)
            } else {
                None
            };
            let status = if withheld {
                "incomplete"
            } else if history {
                "history"
            } else if visible.len() > 1 {
                "conflict"
            } else if visible.is_empty() {
                "unknown"
            } else {
                "current"
            };
            let ids = visible
                .iter()
                .filter_map(|v| v["assertion"]["memory_id"].as_i64())
                .collect::<Vec<_>>();
            crate::access::delivered(db, crate::access::Delivery::Recall, &ids, None).await;
            Ok(
                serde_json::json!({"scope":scope,"version":version,"valid_at":valid_at,"recorded_before":recorded_before,"status":status,"current_id":current_id,"assertions":visible}),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn cfg() -> crate::config::Config {
        let mut c = crate::config::Config::default();
        c.assertions.enabled = true;
        c
    }
    fn principal() -> PolicyPrincipal {
        PolicyPrincipal::local_operator("synthetic-test")
    }
    fn scope(project: &str) -> Scope {
        Scope {
            namespace: "local".into(),
            project: project.into(),
            subject: "toolchain:rust".into(),
            predicate: "version".into(),
        }
    }
    fn write(
        scope: &Scope,
        version: i64,
        memory_id: i64,
        value: Option<&str>,
        at: i64,
        supersedes: Option<&str>,
    ) -> Request {
        Request::Write {
            scope: scope.clone(),
            expected_version: version,
            memory_id,
            value: value.map(str::to_owned),
            valid_from: at,
            valid_until: None,
            supersedes: supersedes.map(str::to_owned),
        }
    }
    fn query(scope: &Scope, valid_at: i64) -> Request {
        Request::Query {
            scope: scope.clone(),
            valid_at: Some(valid_at),
            recorded_before: None,
            history: false,
            purpose: None,
        }
    }
    async fn fixture() -> Result<(tempfile::TempDir, Database, Scope, i64, i64)> {
        let dir = tempfile::tempdir()?;
        let db = Database::new(dir.path().join("assertions.db").to_str().unwrap()).await?;
        db.migrate().await?;
        let project = dir.path().join("project");
        std::fs::create_dir(&project)?;
        let s = scope(project.to_str().unwrap());
        let session = db::create_session(&db, &s.project).await?;
        let a = db::insert_memory(&db, &s.project, &session, "Rust 1.80", None).await?;
        let b = db::insert_memory(&db, &s.project, &session, "Rust 1.81", None).await?;
        Ok((dir, db, s, a, b))
    }
    async fn temporal_contract(db: &Database, s: &Scope, a: i64, b: i64) -> Result<()> {
        let cfg = cfg();
        let principal = principal();
        let first = handle(db, &cfg, &principal, write(s, 0, a, Some("1.80"), 10, None)).await?;
        let first_id = first["id"].as_str().unwrap();
        let mut before = query(s, 30);
        if let Request::Query {
            recorded_before, ..
        } = &mut before
        {
            *recorded_before = Some(first["recorded_at"].as_i64().unwrap() - 1);
        }
        assert_eq!(
            handle(db, &cfg, &principal, before).await?["status"],
            "unknown"
        );
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        let corrected = handle(
            db,
            &cfg,
            &principal,
            write(s, 1, b, Some("1.81"), 20, Some(first_id)),
        )
        .await?;
        let mut historical = query(s, 25);
        if let Request::Query {
            recorded_before, ..
        } = &mut historical
        {
            *recorded_before = first["recorded_at"].as_i64();
        }
        assert_eq!(
            handle(db, &cfg, &principal, historical).await?["current_id"],
            first["id"]
        );
        assert_eq!(
            handle(db, &cfg, &principal, query(s, 15)).await?["current_id"],
            first["id"]
        );
        assert_eq!(
            handle(db, &cfg, &principal, query(s, 25)).await?["current_id"],
            corrected["id"]
        );
        let conflict = handle(db, &cfg, &principal, write(s, 2, a, Some("1.80"), 20, None)).await?;
        let result = handle(db, &cfg, &principal, query(s, 25)).await?;
        assert_eq!(result["status"], "conflict");
        assert!(result["current_id"].is_null());
        handle(
            db,
            &cfg,
            &principal,
            write(s, 3, b, None, 30, conflict["id"].as_str()),
        )
        .await?;
        assert_eq!(
            handle(db, &cfg, &principal, query(s, 35)).await?["current_id"],
            corrected["id"]
        );
        let unchanged = handle(
            db,
            &cfg,
            &principal,
            write(s, 3, b, Some("stale"), 40, None),
        )
        .await
        .unwrap_err();
        assert!(unchanged.to_string().contains("version conflict"));
        let request = write(s, 4, b, Some("concurrent"), 40, None);
        let (x, y) = tokio::join!(
            handle(db, &cfg, &principal, request.clone()),
            handle(db, &cfg, &principal, request)
        );
        assert_eq!(usize::from(x.is_ok()) + usize::from(y.is_ok()), 1);
        let mut conn = db.pool.acquire().await?;
        let events = entries(&mut conn, s).await?;
        drop(conn);
        assert_eq!(events.len(), 5);
        let ledger = db::memory_ledger_for_memory(db, b).await?;
        assert!(ledger.iter().any(|e| e.op_type == "assertion_append"));
        // Restrict current evidence: never resurrect the replaced assertion.
        sqlx::query("UPDATE memory_meta SET tombstoned_at=1 WHERE memory_id=$1")
            .bind(b)
            .execute(&db.pool)
            .await?;
        let denied = handle(db, &cfg, &principal, query(s, 35)).await?;
        assert_eq!(denied["status"], "incomplete");
        assert_eq!(denied["assertions"], serde_json::json!([]));
        assert_eq!(entries(&mut *db.pool.acquire().await?, s).await?.len(), 5);
        Ok(())
    }
    #[tokio::test]
    async fn assertions_temporal_conflict_retraction_concurrency() -> Result<()> {
        let (_dir, db, s, a, b) = fixture().await?;
        temporal_contract(&db, &s, a, b).await?;
        db.pool.close().await;
        Ok(())
    }
    #[tokio::test]
    async fn assertions_scope_capabilities_evidence_and_disabled() -> Result<()> {
        let (_dir, db, s, a, _) = fixture().await?;
        assert!(handle(
            &db,
            &crate::config::Config::default(),
            &principal(),
            write(&s, 0, a, Some("x"), 0, None)
        )
        .await
        .is_err());
        let remote = PolicyPrincipal::configured("remote", "shared_token", vec![], vec![]);
        assert!(handle(&db, &cfg(), &remote, query(&s, 0)).await.is_err());
        let wrong = PolicyPrincipal::configured(
            "remote",
            "shared_token",
            vec!["other".into()],
            vec!["assertions:write".into()],
        );
        assert!(
            handle(&db, &cfg(), &wrong, write(&s, 0, a, Some("x"), 0, None))
                .await
                .is_err()
        );
        let mut foreign = s.clone();
        foreign.project.push_str("-other");
        assert!(handle(
            &db,
            &cfg(),
            &principal(),
            write(&foreign, 0, a, Some("x"), 0, None)
        )
        .await
        .is_err());
        assert!(
            handle(&db, &cfg(), &principal(), write(&s, 0, a, None, 0, None))
                .await
                .is_err()
        );
        let mut ambiguous = s.clone();
        ambiguous.subject = " toolchain:rust".into();
        assert!(handle(
            &db,
            &cfg(),
            &principal(),
            write(&ambiguous, 0, a, Some("x"), 0, None)
        )
        .await
        .is_err());
        handle(
            &db,
            &cfg(),
            &principal(),
            write(&s, 0, a, Some("1.80"), 0, None),
        )
        .await?;
        let before = crate::access::stats(&db, &[a]).await?[&a].recall_count;
        let mut strict = cfg();
        strict.influence.enabled = true;
        strict.influence.require_purpose = true;
        assert!(handle(&db, &strict, &principal(), query(&s, 0))
            .await
            .is_err());
        assert_eq!(
            crate::access::stats(&db, &[a]).await?[&a].recall_count,
            before
        );
        let mut governed = cfg();
        governed.influence.enabled = true;
        db::update_memory_influence_policy(
            &db,
            a,
            "local",
            &principal(),
            &crate::influence::PolicyMutationRequest {
                expected_version: 1,
                patch: crate::influence::MemoryInfluencePolicyPatch {
                    state: Some(crate::influence::InfluenceState::Blocked),
                    ..Default::default()
                },
                reason: "synthetic restriction".into(),
                request_id: "assertion-policy-test".into(),
            },
        )
        .await?;
        let restricted = handle(&db, &governed, &principal(), query(&s, 0)).await?;
        assert_eq!(restricted["status"], "incomplete");
        assert_eq!(restricted["assertions"], serde_json::json!([]));
        assert_eq!(
            crate::access::stats(&db, &[a]).await?[&a].recall_count,
            before
        );
        db::delete_memory(&db, a).await?;
        assert_eq!(
            handle(&db, &cfg(), &principal(), query(&s, 0)).await?["status"],
            "incomplete"
        );
        db.pool.close().await;
        Ok(())
    }
    #[tokio::test]
    async fn assertions_snapshot_delta_export_restore_and_legacy() -> Result<()> {
        let (dir, db, s, a, b) = fixture().await?;
        let first = handle(
            &db,
            &cfg(),
            &principal(),
            write(&s, 0, a, Some("1.80"), 10, None),
        )
        .await?;
        let snap = crate::checkpoint::create(&db, None, &s.project, false).await?;
        let base = crate::checkpoint::load(&db, &snap.blob_hash).await?;
        let context_path = std::path::Path::new(&s.project).join("IRONMEM.md");
        std::fs::write(&context_path, "user-owned project notes")?;

        let second = handle(
            &db,
            &cfg(),
            &principal(),
            write(&s, 1, b, Some("1.81"), 20, first["id"].as_str()),
        )
        .await?;
        let delta = crate::checkpoint::create(&db, None, &s.project, true).await?;
        let state = crate::checkpoint::load(&db, &delta.blob_hash).await?;
        assert_eq!(state.tables[TABLE].rows.len(), 2);
        crate::checkpoint::restore(&db, &base, &snap.id, false).await?;
        assert_eq!(
            std::fs::read_to_string(&context_path)?,
            "user-owned project notes"
        );
        assert_eq!(
            handle(&db, &cfg(), &principal(), query(&s, 25)).await?["current_id"],
            second["id"]
        );
        let mut legacy = base.clone();
        legacy.tables.remove(TABLE);
        crate::checkpoint::restore(&db, &legacy, "old-v5", false).await?;
        assert_eq!(
            handle(&db, &cfg(), &principal(), query(&s, 25)).await?["version"],
            2
        );
        let path = dir.path().join("export.json");
        crate::checkpoint::export(&db, &delta.id, &path).await?;
        let fresh = Database::new(dir.path().join("fresh.db").to_str().unwrap()).await?;
        fresh.migrate().await?;
        crate::checkpoint::import(&fresh, &path, false).await?;
        assert_eq!(
            handle(&fresh, &cfg(), &principal(), query(&s, 25)).await?["current_id"],
            second["id"]
        );
        let mut corrupt = state.clone();
        corrupt.tables.get_mut(TABLE).unwrap().rows[0]
            .insert("version".into(), serde_json::json!(99));
        assert!(
            crate::checkpoint::restore(&fresh, &corrupt, "corrupt", false)
                .await
                .is_err()
        );
        assert_eq!(
            handle(&fresh, &cfg(), &principal(), query(&s, 25)).await?["version"],
            2
        );
        fresh.pool.close().await;
        db.pool.close().await;
        Ok(())
    }
    #[tokio::test]
    async fn assertions_measurement_and_intervals() -> Result<()> {
        let (_dir, db, s, a, b) = fixture().await?;
        sqlx::query("VACUUM").execute(&db.pool).await?;
        let pages: i64 = sqlx::query("PRAGMA page_count")
            .fetch_one(&db.pool)
            .await?
            .get(0);
        let page_size: i64 = sqlx::query("PRAGMA page_size")
            .fetch_one(&db.pool)
            .await?
            .get(0);
        let independent_bytes = pages * page_size;
        let mut previous = None;
        for i in 0..10 {
            let mut request = write(
                &s,
                i,
                if i % 2 == 0 { a } else { b },
                Some(if i % 2 == 0 { "1.80" } else { "1.81" }),
                i * 10,
                previous.as_deref(),
            );
            if let Request::Write { valid_until, .. } = &mut request {
                *valid_until = Some(i * 10 + 10);
            }
            let receipt = handle(&db, &cfg(), &principal(), request).await?;
            previous = receipt["id"].as_str().map(str::to_owned);
        }
        // Independent expected states across validity boundaries; expiry never revives a predecessor.
        for t in -1..=100 {
            let result = handle(&db, &cfg(), &principal(), query(&s, t)).await?;
            if !(0..100).contains(&t) {
                assert_eq!(result["status"], "unknown");
            } else {
                assert_eq!(
                    result["assertions"][0]["assertion"]["value"],
                    if (t / 10) % 2 == 0 { "1.80" } else { "1.81" }
                );
            }
        }
        let mut assertion_us = vec![];
        let mut independent_us = vec![];
        for i in 0..30 {
            let start = std::time::Instant::now();
            let _ = handle(&db, &cfg(), &principal(), query(&s, 95)).await?;
            let elapsed = start.elapsed().as_micros() as u64;
            let start = std::time::Instant::now();
            let _ = db::get_recent_memories(&db, &s.project, 20).await?;
            if i >= 10 {
                assertion_us.push(elapsed);
                independent_us.push(start.elapsed().as_micros() as u64);
            }
        }
        sqlx::query("VACUUM").execute(&db.pool).await?;
        let pages: i64 = sqlx::query("PRAGMA page_count")
            .fetch_one(&db.pool)
            .await?
            .get(0);
        assertion_us.sort();
        independent_us.sort();
        let report = serde_json::json!({"schema_version":1,"source_sha256":env!("IRONMEM_SOURCE_SHA"),"fixture":"two synthetic evidence memories, ten scoped value events, 102 independently expected valid-time points","temporal_points_passed":102,"samples":20,"assertion_delivery_p50_us":assertion_us[9],"assertion_delivery_p95_us":assertion_us[18],"independent_memory_read_p50_us":independent_us[9],"independent_memory_read_p95_us":independent_us[18],"independent_database_bytes":independent_bytes,"database_bytes_with_assertions":pages*page_size,"cache":"warm process; OS cache uncontrolled","profile":"debug without debug symbols","comparison":"assertion delivery includes history verification, governance and access writes; independent read is a lower-level baseline without temporal resolution or delivery writes","scored_accuracy":null,"default_enabled":false});
        println!("ASSERTION_MEASUREMENT={report}");
        if let Some(path) = std::env::var_os("IRONMEM_ASSERTION_REPORT") {
            std::fs::write(path, serde_json::to_vec_pretty(&report)?)?;
        }
        db.pool.close().await;
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires isolated IRONMEM_TEST_POSTGRES_URL"]
    async fn assertions_postgres_temporal_and_snapshots() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let db = Database::new(&std::env::var("IRONMEM_TEST_POSTGRES_URL")?).await?;
        db.migrate().await?;
        let s = scope(dir.path().to_str().unwrap());
        let session = db::create_session(&db, &s.project).await?;
        let a = db::insert_memory(&db, &s.project, &session, "Rust 1.80", None).await?;
        let b = db::insert_memory(&db, &s.project, &session, "Rust 1.81", None).await?;
        temporal_contract(&db, &s, a, b).await?;
        let snap = crate::checkpoint::create(&db, None, &s.project, true).await?;
        let state = crate::checkpoint::load(&db, &snap.blob_hash).await?;
        crate::checkpoint::restore(&db, &state, &snap.id, false).await?;
        assert_eq!(state.tables[TABLE].rows.len(), 5);
        db.pool.close().await;
        Ok(())
    }
}
