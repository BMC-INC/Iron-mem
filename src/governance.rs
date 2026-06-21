use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;

pub const DEFAULT_NAMESPACE: &str = "local";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemorySourceType {
    UserInput,
    ToolOutput,
    AgentGenerated,
    Derived,
    External,
    SyncPeer,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TrustTier {
    High,
    Medium,
    Low,
    Untrusted,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DataClassification {
    Public,
    Internal,
    Confidential,
    Restricted,
    Phi,
    Pii,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConsentState {
    Required,
    Granted,
    Denied,
    Withdrawn,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryGovernance {
    pub namespace: String,
    pub source_type: MemorySourceType,
    pub trust_tier: TrustTier,
    pub writer_identity: Option<String>,
    pub source_ref: Option<String>,
    pub parent_memory_id: Option<i64>,
    pub classification: DataClassification,
    pub consent_state: Option<ConsentState>,
    pub residency: Option<String>,
    pub retention_policy_id: Option<String>,
    pub expires_at: Option<i64>,
    pub legal_hold: bool,
}

impl Default for MemoryGovernance {
    fn default() -> Self {
        Self {
            namespace: DEFAULT_NAMESPACE.to_string(),
            source_type: MemorySourceType::Derived,
            trust_tier: TrustTier::Medium,
            writer_identity: Some("ironmem".to_string()),
            source_ref: None,
            parent_memory_id: None,
            classification: DataClassification::Internal,
            consent_state: None,
            residency: None,
            retention_policy_id: None,
            expires_at: None,
            legal_hold: false,
        }
    }
}

impl MemoryGovernance {
    pub fn explicit() -> Self {
        Self {
            source_type: MemorySourceType::UserInput,
            trust_tier: TrustTier::High,
            writer_identity: Some("ironmem:remember".to_string()),
            ..Self::default()
        }
    }

    pub fn compressed_session() -> Self {
        Self {
            source_type: MemorySourceType::Derived,
            trust_tier: TrustTier::Medium,
            writer_identity: Some("ironmem:compress".to_string()),
            ..Self::default()
        }
    }

    pub fn derived_from(parent_memory_id: i64) -> Self {
        Self {
            source_type: MemorySourceType::Derived,
            trust_tier: TrustTier::Medium,
            writer_identity: Some("ironmem:derive".to_string()),
            parent_memory_id: Some(parent_memory_id),
            ..Self::default()
        }
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        if matches!(
            self.classification,
            DataClassification::Phi | DataClassification::Pii
        ) && self.consent_state != Some(ConsentState::Granted)
        {
            anyhow::bail!(
                "{:?} memory requires consent_state=granted before it can be stored",
                self.classification
            );
        }
        Ok(())
    }
}

pub fn normalize_namespace(namespace: &str) -> String {
    let cleaned = namespace.trim();
    if cleaned.is_empty() {
        DEFAULT_NAMESPACE.to_string()
    } else {
        cleaned
            .chars()
            .map(|c| {
                if c.is_control() || c.is_whitespace() {
                    '_'
                } else {
                    c
                }
            })
            .collect()
    }
}

pub fn parse_source_type(value: &str) -> MemorySourceType {
    match value.trim().to_ascii_lowercase().as_str() {
        "user" | "user_input" => MemorySourceType::UserInput,
        "tool" | "tool_output" => MemorySourceType::ToolOutput,
        "agent" | "agent_generated" => MemorySourceType::AgentGenerated,
        "external" => MemorySourceType::External,
        "sync" | "sync_peer" => MemorySourceType::SyncPeer,
        _ => MemorySourceType::Derived,
    }
}

pub fn parse_trust_tier(value: &str) -> TrustTier {
    match value.trim().to_ascii_lowercase().as_str() {
        "high" => TrustTier::High,
        "low" => TrustTier::Low,
        "untrusted" => TrustTier::Untrusted,
        _ => TrustTier::Medium,
    }
}

pub fn parse_classification(value: &str) -> DataClassification {
    match value.trim().to_ascii_lowercase().as_str() {
        "public" => DataClassification::Public,
        "confidential" => DataClassification::Confidential,
        "restricted" => DataClassification::Restricted,
        "phi" => DataClassification::Phi,
        "pii" => DataClassification::Pii,
        _ => DataClassification::Internal,
    }
}

pub fn parse_consent_state(value: &str) -> Option<ConsentState> {
    match value.trim().to_ascii_lowercase().as_str() {
        "required" => Some(ConsentState::Required),
        "granted" => Some(ConsentState::Granted),
        "denied" => Some(ConsentState::Denied),
        "withdrawn" => Some(ConsentState::Withdrawn),
        "" | "none" => None,
        _ => None,
    }
}

pub fn source_type_str(value: MemorySourceType) -> &'static str {
    match value {
        MemorySourceType::UserInput => "user_input",
        MemorySourceType::ToolOutput => "tool_output",
        MemorySourceType::AgentGenerated => "agent_generated",
        MemorySourceType::Derived => "derived",
        MemorySourceType::External => "external",
        MemorySourceType::SyncPeer => "sync_peer",
    }
}

pub fn trust_tier_str(value: TrustTier) -> &'static str {
    match value {
        TrustTier::High => "high",
        TrustTier::Medium => "medium",
        TrustTier::Low => "low",
        TrustTier::Untrusted => "untrusted",
    }
}

pub fn classification_str(value: DataClassification) -> &'static str {
    match value {
        DataClassification::Public => "public",
        DataClassification::Internal => "internal",
        DataClassification::Confidential => "confidential",
        DataClassification::Restricted => "restricted",
        DataClassification::Phi => "phi",
        DataClassification::Pii => "pii",
    }
}

pub fn consent_state_str(value: ConsentState) -> &'static str {
    match value {
        ConsentState::Required => "required",
        ConsentState::Granted => "granted",
        ConsentState::Denied => "denied",
        ConsentState::Withdrawn => "withdrawn",
    }
}

pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

pub fn sha256_hex(input: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input);
    hex(&hasher.finalize())
}

#[allow(clippy::too_many_arguments)]
pub fn memory_record_hash(
    project: &str,
    session_id: &str,
    summary: &str,
    tags: Option<&str>,
    scope: &str,
    kind: &str,
    governance: &MemoryGovernance,
) -> String {
    let payload = serde_json::json!({
        "classification": classification_str(governance.classification),
        "consent_state": governance.consent_state.map(consent_state_str),
        "expires_at": governance.expires_at,
        "kind": kind,
        "legal_hold": governance.legal_hold,
        "namespace": normalize_namespace(&governance.namespace),
        "parent_memory_id": governance.parent_memory_id,
        "project": project,
        "residency": governance.residency.as_deref(),
        "retention_policy_id": governance.retention_policy_id.as_deref(),
        "scope": scope,
        "session_id": session_id,
        "source_ref": governance.source_ref.as_deref(),
        "source_type": source_type_str(governance.source_type),
        "summary": summary,
        "tags": tags,
        "trust_tier": trust_tier_str(governance.trust_tier),
        "writer_identity": governance.writer_identity.as_deref(),
    });
    sha256_hex(payload.to_string().as_bytes())
}

pub fn ledger_entry_hash(
    prev_hash: Option<&str>,
    namespace: &str,
    memory_id: Option<i64>,
    op_type: &str,
    actor: Option<&str>,
    payload: &str,
    created_at: i64,
) -> String {
    let payload = serde_json::json!({
        "actor": actor,
        "created_at": created_at,
        "memory_id": memory_id,
        "namespace": normalize_namespace(namespace),
        "op_type": op_type,
        "payload": payload,
        "prev_hash": prev_hash,
    });
    sha256_hex(payload.to_string().as_bytes())
}
