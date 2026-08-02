use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use crate::db::{self, Database};

pub const POLICY_READ_CAPABILITY: &str = "influence_policy:read";
pub const POLICY_WRITE_CAPABILITY: &str = "influence_policy:write";
pub const MAX_TASK_TYPES: usize = 64;
pub const MAX_TASK_TYPE_BYTES: usize = 64;
pub const MAX_REASON_BYTES: usize = 512;
pub const MAX_REQUEST_ID_BYTES: usize = 128;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InfluenceState {
    Eligible,
    Quarantined,
    ReasoningOnly,
    ActionRestricted,
    Blocked,
    Superseded,
}

impl fmt::Display for InfluenceState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Eligible => "eligible",
            Self::Quarantined => "quarantined",
            Self::ReasoningOnly => "reasoning_only",
            Self::ActionRestricted => "action_restricted",
            Self::Blocked => "blocked",
            Self::Superseded => "superseded",
        })
    }
}

impl FromStr for InfluenceState {
    type Err = PolicyError;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "eligible" => Ok(Self::Eligible),
            "quarantined" => Ok(Self::Quarantined),
            "reasoning_only" | "reasoning-only" => Ok(Self::ReasoningOnly),
            "action_restricted" | "action-restricted" => Ok(Self::ActionRestricted),
            "blocked" => Ok(Self::Blocked),
            "superseded" => Ok(Self::Superseded),
            _ => Err(PolicyError::InvalidPolicy(format!(
                "unknown influence state '{value}'"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ActionRisk {
    None,
    Low,
    Medium,
    High,
    Critical,
}

impl fmt::Display for ActionRisk {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::None => "none",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Critical => "critical",
        })
    }
}

impl FromStr for ActionRisk {
    type Err = PolicyError;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "none" => Ok(Self::None),
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            "critical" => Ok(Self::Critical),
            _ => Err(PolicyError::InvalidPolicy(format!(
                "unknown action risk '{value}'"
            ))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryInfluencePolicy {
    pub version: u64,
    pub state: InfluenceState,
    pub allowed_task_types: Vec<String>,
    pub denied_task_types: Vec<String>,
    pub maximum_action_risk: ActionRisk,
    pub requires_original_source: bool,
    pub requires_human_confirmation: bool,
    pub maximum_derivation_depth: Option<u32>,
}

impl Default for MemoryInfluencePolicy {
    fn default() -> Self {
        Self {
            version: 1,
            state: InfluenceState::Eligible,
            allowed_task_types: Vec::new(),
            denied_task_types: Vec::new(),
            maximum_action_risk: ActionRisk::Critical,
            requires_original_source: false,
            requires_human_confirmation: false,
            maximum_derivation_depth: None,
        }
    }
}

impl MemoryInfluencePolicy {
    #[allow(dead_code)] // Used by policy diagnostics and the Phase 3 evaluator.
    pub fn is_permissive(&self) -> bool {
        let defaults = Self::default();
        self.state == defaults.state
            && self.allowed_task_types.is_empty()
            && self.denied_task_types.is_empty()
            && self.maximum_action_risk == defaults.maximum_action_risk
            && !self.requires_original_source
            && !self.requires_human_confirmation
            && self.maximum_derivation_depth.is_none()
    }

    pub(crate) fn from_storage(
        stored: StoredInfluencePolicy<'_>,
    ) -> std::result::Result<Self, PolicyError> {
        let version = u64::try_from(stored.version).map_err(|_| {
            PolicyError::InvalidPolicy("stored policy version is negative".to_string())
        })?;
        if version == 0 {
            return Err(PolicyError::InvalidPolicy(
                "stored policy version must be at least 1".to_string(),
            ));
        }
        let maximum_derivation_depth = stored
            .maximum_derivation_depth
            .map(|depth| {
                u32::try_from(depth).map_err(|_| {
                    PolicyError::InvalidPolicy(
                        "stored maximum derivation depth is out of range".to_string(),
                    )
                })
            })
            .transpose()?;
        let mut policy = Self {
            version,
            state: stored.state.parse()?,
            allowed_task_types: serde_json::from_str(stored.allowed_task_types).map_err(
                |error| {
                    PolicyError::InvalidPolicy(format!(
                        "stored allowed task list is invalid JSON: {error}"
                    ))
                },
            )?,
            denied_task_types: serde_json::from_str(stored.denied_task_types).map_err(|error| {
                PolicyError::InvalidPolicy(format!(
                    "stored denied task list is invalid JSON: {error}"
                ))
            })?,
            maximum_action_risk: stored.maximum_action_risk.parse()?,
            requires_original_source: stored.requires_original_source,
            requires_human_confirmation: stored.requires_human_confirmation,
            maximum_derivation_depth,
        };
        policy.allowed_task_types = canonicalize_task_types(policy.allowed_task_types)?;
        policy.denied_task_types = canonicalize_task_types(policy.denied_task_types)?;
        Ok(policy)
    }
}

pub(crate) struct StoredInfluencePolicy<'a> {
    pub version: i64,
    pub state: &'a str,
    pub allowed_task_types: &'a str,
    pub denied_task_types: &'a str,
    pub maximum_action_risk: &'a str,
    pub requires_original_source: bool,
    pub requires_human_confirmation: bool,
    pub maximum_derivation_depth: Option<i64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)] // Public policy-model seam consumed by the Phase 3 egress gate.
pub enum InfluenceUse {
    Reasoning,
    Action,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)] // Public policy-model seam consumed by the Phase 3 egress gate.
pub enum StateDisposition {
    Allow,
    AllowReasoningOnly,
    Deny,
}

/// Baseline state matrix used by the purpose-bound evaluator in Phase 3.
/// This function never performs content egress and therefore cannot be bypassed
/// by request fields. Task, attestation, source, and confirmation rules are
/// layered on top by the shared evaluator.
#[allow(dead_code)] // Public policy-model seam consumed by the Phase 3 egress gate.
pub fn state_disposition(
    policy: &MemoryInfluencePolicy,
    use_kind: InfluenceUse,
    action_risk: ActionRisk,
) -> StateDisposition {
    match policy.state {
        InfluenceState::Blocked | InfluenceState::Quarantined | InfluenceState::Superseded => {
            StateDisposition::Deny
        }
        InfluenceState::ReasoningOnly => match use_kind {
            InfluenceUse::Reasoning => StateDisposition::AllowReasoningOnly,
            InfluenceUse::Action => StateDisposition::Deny,
        },
        InfluenceState::ActionRestricted => match use_kind {
            InfluenceUse::Reasoning => StateDisposition::Allow,
            InfluenceUse::Action if action_risk <= policy.maximum_action_risk => {
                StateDisposition::Allow
            }
            InfluenceUse::Action => StateDisposition::Deny,
        },
        InfluenceState::Eligible => StateDisposition::Allow,
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryInfluencePolicyPatch {
    pub state: Option<InfluenceState>,
    pub allowed_task_types: Option<Vec<String>>,
    pub denied_task_types: Option<Vec<String>>,
    pub maximum_action_risk: Option<ActionRisk>,
    pub requires_original_source: Option<bool>,
    pub requires_human_confirmation: Option<bool>,
    pub maximum_derivation_depth: Option<Option<u32>>,
}

impl MemoryInfluencePolicyPatch {
    pub fn is_empty(&self) -> bool {
        self.state.is_none()
            && self.allowed_task_types.is_none()
            && self.denied_task_types.is_none()
            && self.maximum_action_risk.is_none()
            && self.requires_original_source.is_none()
            && self.requires_human_confirmation.is_none()
            && self.maximum_derivation_depth.is_none()
    }

    pub fn apply(
        &self,
        current: &MemoryInfluencePolicy,
    ) -> std::result::Result<MemoryInfluencePolicy, PolicyError> {
        if self.is_empty() {
            return Err(PolicyError::InvalidPolicy(
                "policy update contains no changes".to_string(),
            ));
        }
        let mut next = current.clone();
        if let Some(state) = self.state {
            next.state = state;
        }
        if let Some(task_types) = &self.allowed_task_types {
            next.allowed_task_types = canonicalize_task_types(task_types.clone())?;
        }
        if let Some(task_types) = &self.denied_task_types {
            next.denied_task_types = canonicalize_task_types(task_types.clone())?;
        }
        if let Some(risk) = self.maximum_action_risk {
            next.maximum_action_risk = risk;
        }
        if let Some(required) = self.requires_original_source {
            next.requires_original_source = required;
        }
        if let Some(required) = self.requires_human_confirmation {
            next.requires_human_confirmation = required;
        }
        if let Some(depth) = self.maximum_derivation_depth {
            next.maximum_derivation_depth = depth;
        }
        Ok(next)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PolicyMutationRequest {
    pub expected_version: u64,
    pub patch: MemoryInfluencePolicyPatch,
    pub reason: String,
    pub request_id: String,
}

impl PolicyMutationRequest {
    pub fn validate(&self) -> std::result::Result<(), PolicyError> {
        validate_bounded_text("reason", &self.reason, MAX_REASON_BYTES)?;
        validate_bounded_text("request_id", &self.request_id, MAX_REQUEST_ID_BYTES)?;
        if self.patch.is_empty() {
            return Err(PolicyError::InvalidPolicy(
                "policy update contains no changes".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryInfluencePolicyRecord {
    pub memory_id: i64,
    pub namespace: String,
    pub policy: MemoryInfluencePolicy,
    pub explicit: bool,
    pub updated_by: Option<String>,
    pub updated_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PolicyPrincipal {
    pub actor: String,
    pub authority: String,
    pub namespaces: Vec<String>,
    pub capabilities: Vec<String>,
}

impl PolicyPrincipal {
    pub fn local_operator(actor: impl Into<String>) -> Self {
        Self {
            actor: actor.into(),
            authority: "local_operator".to_string(),
            namespaces: Vec::new(),
            capabilities: vec![
                POLICY_READ_CAPABILITY.to_string(),
                POLICY_WRITE_CAPABILITY.to_string(),
            ],
        }
    }

    pub fn configured(
        actor: impl Into<String>,
        authority: impl Into<String>,
        namespaces: Vec<String>,
        capabilities: Vec<String>,
    ) -> Self {
        Self {
            actor: actor.into(),
            authority: authority.into(),
            namespaces: namespaces
                .into_iter()
                .map(|namespace| crate::governance::normalize_namespace(&namespace))
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
            capabilities: capabilities
                .into_iter()
                .map(|capability| capability.trim().to_ascii_lowercase())
                .filter(|capability| !capability.is_empty())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
        }
    }

    pub fn authorize(
        &self,
        capability: &'static str,
        namespace: &str,
    ) -> std::result::Result<(), PolicyError> {
        let namespace = crate::governance::normalize_namespace(namespace);
        if !self.namespaces.is_empty() && !self.namespaces.contains(&namespace) {
            return Err(PolicyError::NamespaceDenied {
                namespace,
                actor: self.actor.clone(),
            });
        }
        if !self
            .capabilities
            .iter()
            .any(|configured| configured == capability)
        {
            return Err(PolicyError::CapabilityRequired {
                capability,
                actor: self.actor.clone(),
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyError {
    MemoryNotFound {
        memory_id: i64,
        namespace: String,
    },
    NamespaceDenied {
        namespace: String,
        actor: String,
    },
    CapabilityRequired {
        capability: &'static str,
        actor: String,
    },
    VersionConflict {
        expected: u64,
        actual: u64,
    },
    InvalidPolicy(String),
    NoChange,
}

impl PolicyError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::MemoryNotFound { .. } => "memory_not_found",
            Self::NamespaceDenied { .. } => "namespace_denied",
            Self::CapabilityRequired { .. } => "influence_policy_capability_required",
            Self::VersionConflict { .. } => "policy_version_conflict",
            Self::InvalidPolicy(_) => "invalid_influence_policy",
            Self::NoChange => "influence_policy_unchanged",
        }
    }

    pub fn current_version(&self) -> Option<u64> {
        match self {
            Self::VersionConflict { actual, .. } => Some(*actual),
            _ => None,
        }
    }
}

impl fmt::Display for PolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MemoryNotFound {
                memory_id,
                namespace,
            } => write!(
                formatter,
                "memory {memory_id} was not found in namespace '{namespace}'"
            ),
            Self::NamespaceDenied { namespace, actor } => write!(
                formatter,
                "actor '{actor}' may not access namespace '{namespace}'"
            ),
            Self::CapabilityRequired { capability, actor } => write!(
                formatter,
                "actor '{actor}' lacks required capability '{capability}'"
            ),
            Self::VersionConflict { expected, actual } => write!(
                formatter,
                "policy version conflict: expected {expected}, current version is {actual}"
            ),
            Self::InvalidPolicy(message) => formatter.write_str(message),
            Self::NoChange => formatter.write_str("policy update does not change the policy"),
        }
    }
}

impl std::error::Error for PolicyError {}

pub async fn get_memory_policy(
    database: &Database,
    principal: &PolicyPrincipal,
    memory_id: i64,
    namespace: &str,
) -> Result<MemoryInfluencePolicyRecord> {
    principal
        .authorize(POLICY_READ_CAPABILITY, namespace)
        .map_err(anyhow::Error::new)?;
    db::get_memory_influence_policy(database, memory_id, namespace).await
}

pub async fn update_memory_policy(
    database: &Database,
    principal: &PolicyPrincipal,
    memory_id: i64,
    namespace: &str,
    request: &PolicyMutationRequest,
) -> Result<MemoryInfluencePolicyRecord> {
    principal
        .authorize(POLICY_WRITE_CAPABILITY, namespace)
        .map_err(anyhow::Error::new)?;
    request.validate().map_err(anyhow::Error::new)?;
    db::update_memory_influence_policy(database, memory_id, namespace, principal, request).await
}

pub fn policy_error(error: &anyhow::Error) -> Option<&PolicyError> {
    error.downcast_ref::<PolicyError>()
}

fn canonicalize_task_types(
    task_types: Vec<String>,
) -> std::result::Result<Vec<String>, PolicyError> {
    if task_types.len() > MAX_TASK_TYPES {
        return Err(PolicyError::InvalidPolicy(format!(
            "task list exceeds the maximum of {MAX_TASK_TYPES} entries"
        )));
    }
    task_types
        .into_iter()
        .map(|task_type| normalize_task_type(&task_type))
        .collect::<std::result::Result<BTreeSet<_>, _>>()
        .map(|task_types| task_types.into_iter().collect())
}

pub fn normalize_task_type(value: &str) -> std::result::Result<String, PolicyError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(PolicyError::InvalidPolicy(
            "task type must not be empty".to_string(),
        ));
    }
    if value.len() > MAX_TASK_TYPE_BYTES {
        return Err(PolicyError::InvalidPolicy(format!(
            "task type exceeds {MAX_TASK_TYPE_BYTES} bytes"
        )));
    }
    if value.chars().any(char::is_control) {
        return Err(PolicyError::InvalidPolicy(
            "task type contains control characters".to_string(),
        ));
    }
    let mut normalized = String::with_capacity(value.len());
    let mut separator = false;
    for character in value.chars() {
        if character.is_ascii_alphanumeric() {
            normalized.push(character.to_ascii_lowercase());
            separator = false;
        } else if matches!(character, '_' | '-' | ' ') {
            if !separator && !normalized.is_empty() {
                normalized.push('_');
                separator = true;
            }
        } else {
            return Err(PolicyError::InvalidPolicy(format!(
                "task type '{value}' contains unsupported characters"
            )));
        }
    }
    while normalized.ends_with('_') {
        normalized.pop();
    }
    if normalized.is_empty() {
        return Err(PolicyError::InvalidPolicy(
            "task type must contain letters or digits".to_string(),
        ));
    }
    Ok(normalized)
}

fn validate_bounded_text(
    field: &str,
    value: &str,
    maximum_bytes: usize,
) -> std::result::Result<(), PolicyError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(PolicyError::InvalidPolicy(format!(
            "{field} must not be empty"
        )));
    }
    if trimmed.len() > maximum_bytes {
        return Err(PolicyError::InvalidPolicy(format!(
            "{field} exceeds {maximum_bytes} bytes"
        )));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(PolicyError::InvalidPolicy(format!(
            "{field} contains control characters"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_is_permissive_and_stable() {
        let policy = MemoryInfluencePolicy::default();
        assert!(policy.is_permissive());
        assert_eq!(policy.version, 1);
        assert_eq!(policy.state, InfluenceState::Eligible);
        assert_eq!(policy.maximum_action_risk, ActionRisk::Critical);
        assert_eq!(serde_json::to_value(&policy).unwrap()["state"], "eligible");
    }

    #[test]
    fn state_matrix_cannot_override_restricted_states() {
        let mut policy = MemoryInfluencePolicy::default();
        for state in [
            InfluenceState::Blocked,
            InfluenceState::Quarantined,
            InfluenceState::Superseded,
        ] {
            policy.state = state;
            assert_eq!(
                state_disposition(&policy, InfluenceUse::Reasoning, ActionRisk::None),
                StateDisposition::Deny
            );
            assert_eq!(
                state_disposition(&policy, InfluenceUse::Action, ActionRisk::None),
                StateDisposition::Deny
            );
        }

        policy.state = InfluenceState::ReasoningOnly;
        assert_eq!(
            state_disposition(&policy, InfluenceUse::Reasoning, ActionRisk::None),
            StateDisposition::AllowReasoningOnly
        );
        assert_eq!(
            state_disposition(&policy, InfluenceUse::Action, ActionRisk::None),
            StateDisposition::Deny
        );

        policy.state = InfluenceState::ActionRestricted;
        policy.maximum_action_risk = ActionRisk::Medium;
        assert_eq!(
            state_disposition(&policy, InfluenceUse::Action, ActionRisk::Medium),
            StateDisposition::Allow
        );
        assert_eq!(
            state_disposition(&policy, InfluenceUse::Action, ActionRisk::High),
            StateDisposition::Deny
        );
    }

    #[test]
    fn policy_patch_normalizes_sorts_and_deduplicates_tasks() {
        let patch = MemoryInfluencePolicyPatch {
            allowed_task_types: Some(vec![
                "Deploy Prod".to_string(),
                "deploy-prod".to_string(),
                "code_review".to_string(),
            ]),
            ..Default::default()
        };
        let policy = patch.apply(&MemoryInfluencePolicy::default()).unwrap();
        assert_eq!(policy.allowed_task_types, ["code_review", "deploy_prod"]);
    }

    #[test]
    fn principal_requires_namespace_and_explicit_capability() {
        let principal = PolicyPrincipal::configured(
            "agent:test",
            "authenticated_agent",
            vec!["tenant-a".to_string()],
            vec![POLICY_READ_CAPABILITY.to_string()],
        );
        assert!(principal
            .authorize(POLICY_READ_CAPABILITY, "tenant-a")
            .is_ok());
        assert!(matches!(
            principal.authorize(POLICY_WRITE_CAPABILITY, "tenant-a"),
            Err(PolicyError::CapabilityRequired { .. })
        ));
        assert!(matches!(
            principal.authorize(POLICY_READ_CAPABILITY, "tenant-b"),
            Err(PolicyError::NamespaceDenied { .. })
        ));
    }
}
