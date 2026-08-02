use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use crate::db::{self, Database};
use crate::influence::{
    PolicyError, PolicyPrincipal, POLICY_READ_CAPABILITY, POLICY_WRITE_CAPABILITY,
};

pub const CLAIM_SCHEMA_VERSION: u32 = 1;
const MAX_CLAIM_KEY_BYTES: usize = 160;
const MAX_BASIS_BYTES: usize = 512;
const MAX_MEMBERS: usize = 32;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClaimCardinality {
    Single,
    Set,
    Ordered,
    Custom,
}

impl fmt::Display for ClaimCardinality {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Single => "single",
            Self::Set => "set",
            Self::Ordered => "ordered",
            Self::Custom => "custom",
        })
    }
}

impl FromStr for ClaimCardinality {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "single" => Ok(Self::Single),
            "set" => Ok(Self::Set),
            "ordered" => Ok(Self::Ordered),
            "custom" => Ok(Self::Custom),
            _ => anyhow::bail!("unknown claim cardinality '{value}'"),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContradictionStatus {
    Unresolved,
    Preferred,
    Resolved,
    Obsolete,
}

impl fmt::Display for ContradictionStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Unresolved => "unresolved",
            Self::Preferred => "preferred",
            Self::Resolved => "resolved",
            Self::Obsolete => "obsolete",
        })
    }
}

impl FromStr for ContradictionStatus {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "unresolved" => Ok(Self::Unresolved),
            "preferred" => Ok(Self::Preferred),
            "resolved" => Ok(Self::Resolved),
            "obsolete" => Ok(Self::Obsolete),
            _ => anyhow::bail!("unknown contradiction status '{value}'"),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemberStance {
    Supports,
    Contradicts,
    Competing,
}

impl fmt::Display for MemberStance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Supports => "supports",
            Self::Contradicts => "contradicts",
            Self::Competing => "competing",
        })
    }
}

impl FromStr for MemberStance {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "supports" => Ok(Self::Supports),
            "contradicts" => Ok(Self::Contradicts),
            "competing" => Ok(Self::Competing),
            _ => anyhow::bail!("unknown contradiction stance '{value}'"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContradictionMember {
    pub memory_id: i64,
    pub stance: MemberStance,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContradictionSet {
    pub id: String,
    pub namespace: String,
    pub realm: String,
    pub project: Option<String>,
    pub claim_key: String,
    pub claim_schema_version: u32,
    pub cardinality: ClaimCardinality,
    pub preferred_memory_id: Option<i64>,
    pub status: ContradictionStatus,
    pub resolution_basis: Option<String>,
    pub resolved_by: Option<String>,
    pub version: u64,
    pub created_at: i64,
    pub updated_at: i64,
    pub members: Vec<ContradictionMember>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateContradictionRequest {
    pub namespace: String,
    #[serde(default = "default_realm")]
    pub realm: String,
    pub project: Option<String>,
    pub claim_key: String,
    #[serde(default = "default_cardinality")]
    pub cardinality: ClaimCardinality,
    pub members: Vec<ContradictionMember>,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateContradictionRequest {
    pub expected_version: u64,
    pub status: ContradictionStatus,
    pub preferred_memory_id: Option<i64>,
    pub basis: String,
}

fn default_realm() -> String {
    "project".to_string()
}

fn default_cardinality() -> ClaimCardinality {
    ClaimCardinality::Single
}

pub fn canonical_claim_key(value: &str) -> Result<String> {
    let mut result = String::with_capacity(value.len());
    let mut separator = false;
    for ch in value.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            result.push(ch.to_ascii_lowercase());
            separator = false;
        } else if matches!(ch, '.' | '_' | '-' | ' ' | ':' | '/')
            && !separator
            && !result.is_empty()
        {
            result.push('.');
            separator = true;
        } else if ch.is_control() {
            anyhow::bail!("claim key contains a control character");
        }
    }
    while result.ends_with('.') {
        result.pop();
    }
    if result.is_empty() || result.len() > MAX_CLAIM_KEY_BYTES {
        anyhow::bail!("claim key must contain 1 to {MAX_CLAIM_KEY_BYTES} bytes");
    }
    Ok(result)
}

pub fn relation_cardinality(relation: &str) -> Option<ClaimCardinality> {
    match relation.trim().to_ascii_lowercase().as_str() {
        "auth_mode" | "authentication_mode" | "deployment_target" | "status" | "version" => {
            Some(ClaimCardinality::Single)
        }
        "depends_on" | "member_of" | "supports" | "tagged" | "uses" => Some(ClaimCardinality::Set),
        _ => None,
    }
}

fn validate_text(label: &str, value: &str, max: usize) -> Result<()> {
    if value.trim().is_empty() || value.len() > max || value.chars().any(char::is_control) {
        anyhow::bail!("{label} must contain 1 to {max} non-control bytes");
    }
    Ok(())
}

fn validate_realm(realm: &str, project: Option<&str>) -> Result<()> {
    match realm {
        "project" if project.is_some_and(|value| !value.trim().is_empty()) => Ok(()),
        "user" if project.is_none() => Ok(()),
        "project" => anyhow::bail!("project realm requires a project"),
        "user" => anyhow::bail!("user realm must not use a fake project identity"),
        _ => anyhow::bail!("realm must be 'project' or 'user'"),
    }
}

pub async fn create(
    database: &Database,
    principal: &PolicyPrincipal,
    request: &CreateContradictionRequest,
) -> Result<ContradictionSet> {
    let namespace = crate::governance::normalize_namespace(&request.namespace);
    principal
        .authorize(POLICY_WRITE_CAPABILITY, &namespace)
        .map_err(anyhow::Error::new)?;
    validate_realm(&request.realm, request.project.as_deref()).map_err(invalid_policy)?;
    validate_text("reason", &request.reason, MAX_BASIS_BYTES).map_err(invalid_policy)?;
    if request.members.len() < 2 || request.members.len() > MAX_MEMBERS {
        return Err(invalid_policy(anyhow::anyhow!(
            "contradiction set requires 2 to {MAX_MEMBERS} members"
        )));
    }
    let ids = request
        .members
        .iter()
        .map(|member| member.memory_id)
        .collect::<BTreeSet<_>>();
    if ids.len() != request.members.len() {
        return Err(invalid_policy(anyhow::anyhow!(
            "contradiction members must be distinct"
        )));
    }
    let claim_key = canonical_claim_key(&request.claim_key).map_err(invalid_policy)?;
    db::create_contradiction_set(database, principal, request, &namespace, &claim_key).await
}

pub async fn get(
    database: &Database,
    principal: &PolicyPrincipal,
    id: &str,
    namespace: &str,
) -> Result<ContradictionSet> {
    let namespace = crate::governance::normalize_namespace(namespace);
    principal
        .authorize(POLICY_READ_CAPABILITY, &namespace)
        .map_err(anyhow::Error::new)?;
    db::get_contradiction_set(database, id, &namespace).await
}

pub async fn update(
    database: &Database,
    principal: &PolicyPrincipal,
    id: &str,
    namespace: &str,
    request: &UpdateContradictionRequest,
) -> Result<ContradictionSet> {
    let namespace = crate::governance::normalize_namespace(namespace);
    principal
        .authorize(POLICY_WRITE_CAPABILITY, &namespace)
        .map_err(anyhow::Error::new)?;
    validate_text("basis", &request.basis, MAX_BASIS_BYTES).map_err(invalid_policy)?;
    db::update_contradiction_set(database, principal, id, &namespace, request).await
}

fn invalid_policy(error: anyhow::Error) -> anyhow::Error {
    anyhow::Error::new(PolicyError::InvalidPolicy(error.to_string()))
}

pub async fn list_for_memory(
    database: &Database,
    principal: &PolicyPrincipal,
    memory_id: i64,
    namespace: &str,
) -> Result<Vec<ContradictionSet>> {
    let namespace = crate::governance::normalize_namespace(namespace);
    principal
        .authorize(POLICY_READ_CAPABILITY, &namespace)
        .map_err(anyhow::Error::new)?;
    db::contradiction_sets_for_memory(database, memory_id, &namespace).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claim_keys_are_canonical_and_cardinality_is_explicit() {
        assert_eq!(
            canonical_claim_key(" Project/Auth Mode ").unwrap(),
            "project.auth.mode"
        );
        assert_eq!(
            relation_cardinality("auth_mode"),
            Some(ClaimCardinality::Single)
        );
        assert_eq!(
            relation_cardinality("depends_on"),
            Some(ClaimCardinality::Set)
        );
        assert_eq!(relation_cardinality("unknown"), None);
    }

    #[test]
    fn user_realm_rejects_fake_project_identity() {
        assert!(validate_realm("user", None).is_ok());
        assert!(validate_realm("user", Some("/fake")).is_err());
    }

    #[tokio::test]
    async fn create_prefer_and_resolve_preserve_competing_memories_and_write_ledger() -> Result<()>
    {
        let path =
            std::env::temp_dir().join(format!("ironmem-contradiction-{}.db", uuid::Uuid::new_v4()));
        let database = Database::new(path.to_str().unwrap()).await?;
        database.migrate().await?;
        let project = "/tmp/contradiction";
        let session = db::create_session(&database, project).await?;
        let first = db::insert_memory(&database, project, &session, "auth is oauth", None).await?;
        let second =
            db::insert_memory(&database, project, &session, "auth is passkey", None).await?;
        let principal = PolicyPrincipal::local_operator("test");
        let created = create(
            &database,
            &principal,
            &CreateContradictionRequest {
                namespace: "local".into(),
                realm: "project".into(),
                project: Some(project.into()),
                claim_key: "Project/Auth Mode".into(),
                cardinality: ClaimCardinality::Single,
                members: vec![
                    ContradictionMember {
                        memory_id: first,
                        stance: MemberStance::Competing,
                    },
                    ContradictionMember {
                        memory_id: second,
                        stance: MemberStance::Competing,
                    },
                ],
                reason: "two observed configurations".into(),
            },
        )
        .await?;
        assert_eq!(created.claim_key, "project.auth.mode");
        assert_eq!(created.members.len(), 2);
        let preferred = update(
            &database,
            &principal,
            &created.id,
            "local",
            &UpdateContradictionRequest {
                expected_version: 1,
                status: ContradictionStatus::Preferred,
                preferred_memory_id: Some(second),
                basis: "passing runtime configuration".into(),
            },
        )
        .await?;
        assert_eq!(preferred.version, 2);
        assert_eq!(preferred.members.len(), 2);
        assert!(db::get_memory_by_id(&database, first).await?.is_some());
        let denied = PolicyPrincipal::configured(
            "other-tenant",
            "authenticated_agent",
            vec!["other".into()],
            vec![POLICY_READ_CAPABILITY.into()],
        );
        assert!(get(&database, &denied, &created.id, "local").await.is_err());
        let ledger = sqlx::query(
            "SELECT COUNT(*) AS count FROM memory_ledger
              WHERE op_type IN ('contradiction_create','contradiction_update')",
        )
        .fetch_one(&database.pool)
        .await?;
        use sqlx::Row;
        assert_eq!(ledger.get::<i64, _>("count"), 2);
        drop(database);
        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[tokio::test]
    async fn user_scoped_sets_use_user_realm_without_a_fake_project() -> Result<()> {
        let path = std::env::temp_dir().join(format!(
            "ironmem-user-contradiction-{}.db",
            uuid::Uuid::new_v4()
        ));
        let database = Database::new(path.to_str().unwrap()).await?;
        database.migrate().await?;
        let session = db::create_session(&database, "/tmp/user-realm").await?;
        let first =
            db::insert_memory(&database, "/tmp/user-realm", &session, "prefers dark", None).await?;
        let second = db::insert_memory(
            &database,
            "/tmp/user-realm",
            &session,
            "prefers light",
            None,
        )
        .await?;
        db::set_memory_scope_kind(&database, first, "user", "preference").await?;
        db::set_memory_scope_kind(&database, second, "user", "preference").await?;
        let set = create(
            &database,
            &PolicyPrincipal::local_operator("test"),
            &CreateContradictionRequest {
                namespace: "local".into(),
                realm: "user".into(),
                project: None,
                claim_key: "user.theme".into(),
                cardinality: ClaimCardinality::Single,
                members: vec![
                    ContradictionMember {
                        memory_id: first,
                        stance: MemberStance::Competing,
                    },
                    ContradictionMember {
                        memory_id: second,
                        stance: MemberStance::Competing,
                    },
                ],
                reason: "conflicting preferences".into(),
            },
        )
        .await?;
        assert_eq!(set.realm, "user");
        assert!(set.project.is_none());
        drop(database);
        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[tokio::test]
    async fn graph_single_value_conflict_creates_annotation_but_set_value_does_not() -> Result<()> {
        let path = std::env::temp_dir().join(format!(
            "ironmem-graph-contradiction-{}.db",
            uuid::Uuid::new_v4()
        ));
        let database = Database::new(path.to_str().unwrap()).await?;
        database.migrate().await?;
        let project = "/tmp/graph-contradiction";
        let session = db::create_session(&database, project).await?;
        let first = db::insert_memory(&database, project, &session, "oauth", None).await?;
        let second = db::insert_memory(&database, project, &session, "passkey", None).await?;
        for (memory_id, target) in [(first, "oauth"), (second, "passkey")] {
            db::insert_memory_edge(
                &database,
                &db::NewMemoryEdge {
                    project: project.into(),
                    memory_id,
                    source: "Project".into(),
                    relation: "auth_mode".into(),
                    target: target.into(),
                    valid_from: None,
                    valid_until: None,
                    confidence: 1.0,
                },
            )
            .await?;
        }
        let contexts = db::influence_contexts_for(&database, &[first, second], "local").await?;
        assert_eq!(contexts[&first].unresolved_contradiction_set_ids.len(), 1);
        assert_eq!(
            contexts[&first].unresolved_contradiction_set_ids,
            contexts[&second].unresolved_contradiction_set_ids
        );

        let third = db::insert_memory(&database, project, &session, "rust", None).await?;
        let fourth = db::insert_memory(&database, project, &session, "python", None).await?;
        for (memory_id, target) in [(third, "rust"), (fourth, "python")] {
            db::insert_memory_edge(
                &database,
                &db::NewMemoryEdge {
                    project: project.into(),
                    memory_id,
                    source: "Project".into(),
                    relation: "uses".into(),
                    target: target.into(),
                    valid_from: None,
                    valid_until: None,
                    confidence: 1.0,
                },
            )
            .await?;
        }
        let contexts = db::influence_contexts_for(&database, &[third, fourth], "local").await?;
        assert!(contexts[&third].unresolved_contradiction_set_ids.is_empty());
        drop(database);
        let _ = std::fs::remove_file(path);
        Ok(())
    }
}
