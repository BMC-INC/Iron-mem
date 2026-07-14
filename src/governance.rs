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
        crate::metrics::timed(crate::metrics::GovOp::ConsentCheck, || {
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
        })
    }
}

pub fn normalize_namespace(namespace: &str) -> String {
    crate::metrics::timed(crate::metrics::GovOp::NamespaceResolve, || {
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
    })
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
    crate::metrics::timed(crate::metrics::GovOp::TrustEval, || {
        match value.trim().to_ascii_lowercase().as_str() {
            "high" => TrustTier::High,
            "low" => TrustTier::Low,
            "untrusted" => TrustTier::Untrusted,
            _ => TrustTier::Medium,
        }
    })
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

// Wired into the retrieval ranker via `retrieval::apply_trust_boost`, gated by
// `temporal_trust.weight`; the synthesis pass populates ref_count/last_validated.
/// Temporal-trust retrieval boost (paper Finding 4 — trust earned over time, not a
/// static scalar). Blends two trajectory signals, then scales by `weight`:
///   • reference — how many receipt-confirmed references the memory has (saturating)
///   • recency   — how recently it was last validated (exponential half-life decay)
/// The two are multiplied, so a memory must be BOTH referenced AND recently
/// confirmed to score high; trust lapses if not revalidated. Returns an additive
/// boost for a candidate's retrieval score. A never-validated memory contributes 0,
/// and `weight <= 0.0` is a hard no-op (the lever is off by default).
pub fn trust_trajectory_boost(
    ref_count: i64,
    last_validated_at: Option<i64>,
    now: i64,
    weight: f64,
    recency_halflife_days: f64,
    ref_saturation: f64,
) -> f64 {
    if weight <= 0.0 || ref_count <= 0 {
        return 0.0;
    }
    let sat = ref_saturation.max(1.0);
    let ref_term = (ref_count as f64) / (ref_count as f64 + sat); // (0, 1)
    let recency_term = match last_validated_at {
        Some(ts) => {
            let age_days = ((now - ts).max(0) as f64) / 86_400.0;
            let hl = recency_halflife_days.max(0.0001);
            0.5_f64.powf(age_days / hl) // (0, 1]
        }
        None => return 0.0,
    };
    weight * ref_term * recency_term
}

// Wired into the retrieval ranker via `retrieval::apply_tier_boost`, gated by
// `governance_router.weight` (paper M3 — query-time governance routing).
/// Governed-retrieval authority boost (#1): a memory's *writer trust tier* —
/// recorded at write time but, until now, never consulted at query time — nudges
/// its retrieval rank. User-explicit (`High`) facts outrank machine-`Derived`
/// (`Medium`) ones on near-ties; `Low`/`Untrusted` writers are pushed down.
/// Additive and symmetric around `Medium` (the default tier), so undifferentiated
/// corpora are unaffected. `weight <= 0.0` is a hard no-op (lever off).
pub fn tier_authority_boost(tier: TrustTier, weight: f64) -> f64 {
    if weight <= 0.0 {
        return 0.0;
    }
    let scale = match tier {
        TrustTier::High => 1.0,
        TrustTier::Medium => 0.0,
        TrustTier::Low => -1.0,
        TrustTier::Untrusted => -2.0,
    };
    weight * scale
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_authority_boost_orders_high_above_low_and_is_gated() {
        // Off by default (weight 0) regardless of tier.
        assert_eq!(tier_authority_boost(TrustTier::High, 0.0), 0.0);
        assert_eq!(tier_authority_boost(TrustTier::Untrusted, 0.0), 0.0);
        // When on: High > Medium(==0) > Low > Untrusted.
        let w = 0.05;
        let high = tier_authority_boost(TrustTier::High, w);
        let med = tier_authority_boost(TrustTier::Medium, w);
        let low = tier_authority_boost(TrustTier::Low, w);
        let unt = tier_authority_boost(TrustTier::Untrusted, w);
        assert!(high > med && med == 0.0 && med > low && low > unt);
        assert_eq!(high, w);
        assert_eq!(unt, -2.0 * w);
    }

    #[test]
    fn trajectory_boost_is_off_when_disabled_or_unreferenced() {
        assert_eq!(
            trust_trajectory_boost(5, Some(100), 100, 0.0, 30.0, 5.0),
            0.0
        );
        assert_eq!(trust_trajectory_boost(0, None, 100, 0.1, 30.0, 5.0), 0.0);
        assert_eq!(trust_trajectory_boost(3, None, 100, 0.1, 30.0, 5.0), 0.0);
    }

    #[test]
    fn trajectory_boost_rewards_recent_and_referenced() {
        let now = 1_000_000_000;
        let fresh = trust_trajectory_boost(10, Some(now), now, 0.1, 30.0, 5.0);
        let stale = trust_trajectory_boost(10, Some(now - 86_400 * 90), now, 0.1, 30.0, 5.0);
        assert!(fresh > stale, "recent validation should outscore stale");
        assert!(
            fresh > 0.0 && fresh <= 0.1,
            "boost stays within weight bound"
        );
    }

    #[test]
    fn trajectory_boost_saturates_on_reference_count() {
        let now = 2_000;
        let many = trust_trajectory_boost(100, Some(now), now, 0.1, 30.0, 5.0);
        assert!(
            many < 0.1,
            "reference term saturates below the weight ceiling"
        );
    }
}
