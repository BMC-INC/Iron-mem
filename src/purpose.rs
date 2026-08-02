//! Verified purpose and scoped confirmation primitives for governed memory egress.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use hmac::{Hmac, Mac};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;

use crate::influence::{normalize_task_type, ActionRisk};

type HmacSha256 = Hmac<Sha256>;
const TOKEN_VERSION: &str = "v1";
const MAX_FIELD_BYTES: usize = 512;
const CLOCK_SKEW_SECONDS: i64 = 30;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecallPurpose {
    pub request_id: String,
    pub namespace: String,
    pub project: String,
    pub task_type: String,
    pub intended_action: Option<String>,
    pub action_risk: ActionRisk,
    #[serde(default)]
    pub require_source_backing: bool,
    #[serde(default)]
    pub purpose_attestation: Option<String>,
    #[serde(default)]
    pub confirmation_receipt: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PurposeAuthority {
    SelfDeclared,
    AuthenticatedAgent,
    TrustedRuntime,
    LocalOperator,
}

impl PurposeAuthority {
    pub fn is_trusted(self) -> bool {
        matches!(self, Self::TrustedRuntime | Self::LocalOperator)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VerifiedRecallPurpose {
    pub request_id: String,
    pub subject_agent_id: Option<String>,
    pub namespace: String,
    pub project: String,
    pub task_type: String,
    pub intended_action: Option<String>,
    pub action_risk: ActionRisk,
    pub require_source_backing: bool,
    pub authority: PurposeAuthority,
    pub attestation_id: Option<String>,
    pub confirmation_receipt_id: Option<String>,
    pub issued_at: i64,
    pub expires_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PurposeAttestationClaims {
    pub attestation_id: String,
    pub issuer: String,
    pub subject_agent_id: Option<String>,
    pub request_id: String,
    pub namespace: String,
    pub project: String,
    pub task_type: String,
    pub intended_action: Option<String>,
    pub action_risk: ActionRisk,
    pub require_source_backing: bool,
    pub issued_at: i64,
    pub expires_at: i64,
    pub nonce: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfirmationClaims {
    pub receipt_id: String,
    pub issuer: String,
    pub confirming_actor: String,
    pub request_id: String,
    pub purpose_hash: String,
    pub namespace: String,
    pub project: String,
    pub task_type: String,
    pub intended_action: Option<String>,
    pub maximum_authorized_risk: ActionRisk,
    pub issued_at: i64,
    pub expires_at: i64,
    pub nonce: String,
    #[serde(default = "default_single_use")]
    pub single_use: bool,
}

fn default_single_use() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PurposeError {
    Invalid(String),
    MissingAttestation,
    InvalidAttestation,
    ExpiredAttestation,
    ScopeMismatch,
    InvalidConfirmation,
    ExpiredConfirmation,
    ConfirmationRiskExceeded,
}

impl PurposeError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid(_) => "purpose_invalid",
            Self::MissingAttestation => "purpose_unattested",
            Self::InvalidAttestation => "purpose_attestation_invalid",
            Self::ExpiredAttestation => "purpose_attestation_expired",
            Self::ScopeMismatch => "purpose_attestation_scope_mismatch",
            Self::InvalidConfirmation => "confirmation_receipt_invalid",
            Self::ExpiredConfirmation => "confirmation_receipt_expired",
            Self::ConfirmationRiskExceeded => "confirmation_risk_exceeded",
        }
    }
}

impl fmt::Display for PurposeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) => f.write_str(message),
            other => f.write_str(other.code()),
        }
    }
}

impl std::error::Error for PurposeError {}

impl RecallPurpose {
    pub fn normalized(&self) -> Result<Self, PurposeError> {
        validate_field("request_id", &self.request_id)?;
        validate_field("namespace", &self.namespace)?;
        validate_field("project", &self.project)?;
        if let Some(action) = &self.intended_action {
            validate_field("intended_action", action)?;
        }
        Ok(Self {
            request_id: self.request_id.trim().to_string(),
            namespace: crate::governance::normalize_namespace(&self.namespace),
            project: self.project.trim().to_string(),
            task_type: normalize_task_type(&self.task_type)
                .map_err(|error| PurposeError::Invalid(error.to_string()))?,
            intended_action: self
                .intended_action
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned),
            action_risk: self.action_risk,
            require_source_backing: self.require_source_backing,
            purpose_attestation: self.purpose_attestation.clone(),
            confirmation_receipt: self.confirmation_receipt.clone(),
        })
    }
}

pub trait PurposeVerifier: Send + Sync {
    fn verify(
        &self,
        purpose: &RecallPurpose,
        authenticated_agent: Option<&str>,
        now: i64,
    ) -> Result<(VerifiedRecallPurpose, Option<PurposeAttestationClaims>), PurposeError>;
}

#[derive(Default)]
pub struct AdvisoryPurposeVerifier;

impl PurposeVerifier for AdvisoryPurposeVerifier {
    fn verify(
        &self,
        purpose: &RecallPurpose,
        authenticated_agent: Option<&str>,
        now: i64,
    ) -> Result<(VerifiedRecallPurpose, Option<PurposeAttestationClaims>), PurposeError> {
        let purpose = purpose.normalized()?;
        Ok((
            VerifiedRecallPurpose {
                request_id: purpose.request_id,
                subject_agent_id: authenticated_agent.map(ToOwned::to_owned),
                namespace: purpose.namespace,
                project: purpose.project,
                task_type: purpose.task_type,
                intended_action: purpose.intended_action,
                action_risk: purpose.action_risk,
                require_source_backing: purpose.require_source_backing,
                authority: if authenticated_agent.is_some() {
                    PurposeAuthority::AuthenticatedAgent
                } else {
                    PurposeAuthority::SelfDeclared
                },
                attestation_id: None,
                confirmation_receipt_id: None,
                issued_at: now,
                expires_at: None,
            },
            None,
        ))
    }
}

pub struct TrustedRuntimeVerifier {
    key: Vec<u8>,
}

impl TrustedRuntimeVerifier {
    pub fn new(key: Vec<u8>) -> Result<Self, PurposeError> {
        if key.len() < 32 {
            return Err(PurposeError::Invalid(
                "attestation key must contain at least 32 bytes".to_string(),
            ));
        }
        Ok(Self { key })
    }
}

impl PurposeVerifier for TrustedRuntimeVerifier {
    fn verify(
        &self,
        purpose: &RecallPurpose,
        authenticated_agent: Option<&str>,
        now: i64,
    ) -> Result<(VerifiedRecallPurpose, Option<PurposeAttestationClaims>), PurposeError> {
        let purpose = purpose.normalized()?;
        let token = purpose
            .purpose_attestation
            .as_deref()
            .ok_or(PurposeError::MissingAttestation)?;
        let claims: PurposeAttestationClaims =
            decode_signed(token, &self.key).map_err(|_| PurposeError::InvalidAttestation)?;
        validate_window(claims.issued_at, claims.expires_at, now)
            .map_err(|_| PurposeError::ExpiredAttestation)?;
        let claim_task =
            normalize_task_type(&claims.task_type).map_err(|_| PurposeError::InvalidAttestation)?;
        let claim_namespace = crate::governance::normalize_namespace(&claims.namespace);
        if claims.request_id != purpose.request_id
            || claim_namespace != purpose.namespace
            || claims.project != purpose.project
            || claim_task != purpose.task_type
            || claims.intended_action != purpose.intended_action
            || claims.action_risk != purpose.action_risk
            || claims.require_source_backing != purpose.require_source_backing
            || authenticated_agent
                .zip(claims.subject_agent_id.as_deref())
                .is_some_and(|(transport, claim)| transport != claim)
        {
            return Err(PurposeError::ScopeMismatch);
        }
        Ok((
            VerifiedRecallPurpose {
                request_id: purpose.request_id,
                subject_agent_id: claims.subject_agent_id.clone(),
                namespace: purpose.namespace,
                project: purpose.project,
                task_type: purpose.task_type,
                intended_action: purpose.intended_action,
                action_risk: purpose.action_risk,
                require_source_backing: purpose.require_source_backing,
                authority: PurposeAuthority::TrustedRuntime,
                attestation_id: Some(claims.attestation_id.clone()),
                confirmation_receipt_id: None,
                issued_at: claims.issued_at,
                expires_at: Some(claims.expires_at),
            },
            Some(claims),
        ))
    }
}

pub fn verify_confirmation(
    token: &str,
    key: &[u8],
    purpose: &VerifiedRecallPurpose,
    now: i64,
) -> Result<ConfirmationClaims, PurposeError> {
    let claims: ConfirmationClaims =
        decode_signed(token, key).map_err(|_| PurposeError::InvalidConfirmation)?;
    validate_window(claims.issued_at, claims.expires_at, now)
        .map_err(|_| PurposeError::ExpiredConfirmation)?;
    if claims.request_id != purpose.request_id
        || claims.purpose_hash != purpose_hash(purpose)
        || crate::governance::normalize_namespace(&claims.namespace) != purpose.namespace
        || claims.project != purpose.project
        || normalize_task_type(&claims.task_type).ok().as_deref() != Some(&purpose.task_type)
        || claims.intended_action != purpose.intended_action
    {
        return Err(PurposeError::InvalidConfirmation);
    }
    if purpose.action_risk > claims.maximum_authorized_risk {
        return Err(PurposeError::ConfirmationRiskExceeded);
    }
    Ok(claims)
}

pub fn local_operator_purpose(
    purpose: &RecallPurpose,
    actor: &str,
    now: i64,
) -> Result<VerifiedRecallPurpose, PurposeError> {
    let purpose = purpose.normalized()?;
    Ok(VerifiedRecallPurpose {
        request_id: purpose.request_id,
        subject_agent_id: Some(actor.to_string()),
        namespace: purpose.namespace,
        project: purpose.project,
        task_type: purpose.task_type,
        intended_action: purpose.intended_action,
        action_risk: purpose.action_risk,
        require_source_backing: purpose.require_source_backing,
        authority: PurposeAuthority::LocalOperator,
        attestation_id: None,
        confirmation_receipt_id: None,
        issued_at: now,
        expires_at: None,
    })
}

pub fn purpose_hash(purpose: &VerifiedRecallPurpose) -> String {
    let bytes = serde_json::to_vec(&serde_json::json!({
        "version": 1,
        "request_id": purpose.request_id,
        "namespace": purpose.namespace,
        "project": purpose.project,
        "task_type": purpose.task_type,
        "intended_action": purpose.intended_action,
        "action_risk": purpose.action_risk,
        "require_source_backing": purpose.require_source_backing,
    }))
    .expect("verified purpose is serializable");
    format!("{:x}", Sha256::digest(bytes))
}

pub fn mint_confirmation_token(
    purpose: &RecallPurpose,
    key: &[u8],
    issuer: &str,
    confirming_actor: &str,
    maximum_authorized_risk: ActionRisk,
    issued_at: i64,
    expires_at: i64,
) -> Result<String, PurposeError> {
    if key.len() < 32 {
        return Err(PurposeError::Invalid(
            "confirmation key must contain at least 32 bytes".to_string(),
        ));
    }
    validate_field("issuer", issuer)?;
    validate_field("confirming_actor", confirming_actor)?;
    validate_window(issued_at, expires_at, issued_at)?;
    let verified = local_operator_purpose(purpose, confirming_actor, issued_at)?;
    let claims = ConfirmationClaims {
        receipt_id: uuid::Uuid::new_v4().to_string(),
        issuer: issuer.to_string(),
        confirming_actor: confirming_actor.to_string(),
        request_id: verified.request_id.clone(),
        purpose_hash: purpose_hash(&verified),
        namespace: verified.namespace,
        project: verified.project,
        task_type: verified.task_type,
        intended_action: verified.intended_action,
        maximum_authorized_risk,
        issued_at,
        expires_at,
        nonce: uuid::Uuid::new_v4().to_string(),
        single_use: true,
    };
    Ok(encode_signed(&claims, key))
}

fn decode_signed<T: DeserializeOwned>(token: &str, key: &[u8]) -> Result<T, PurposeError> {
    let mut parts = token.split('.');
    let version = parts.next().ok_or(PurposeError::InvalidAttestation)?;
    let payload = parts.next().ok_or(PurposeError::InvalidAttestation)?;
    let signature = parts.next().ok_or(PurposeError::InvalidAttestation)?;
    if version != TOKEN_VERSION || parts.next().is_some() {
        return Err(PurposeError::InvalidAttestation);
    }
    let signature = URL_SAFE_NO_PAD
        .decode(signature)
        .map_err(|_| PurposeError::InvalidAttestation)?;
    let mut mac = HmacSha256::new_from_slice(key).map_err(|_| PurposeError::InvalidAttestation)?;
    mac.update(version.as_bytes());
    mac.update(b".");
    mac.update(payload.as_bytes());
    mac.verify_slice(&signature)
        .map_err(|_| PurposeError::InvalidAttestation)?;
    let bytes = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| PurposeError::InvalidAttestation)?;
    serde_json::from_slice(&bytes).map_err(|_| PurposeError::InvalidAttestation)
}

fn encode_signed<T: Serialize>(claims: &T, key: &[u8]) -> String {
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).unwrap());
    let message = format!("{TOKEN_VERSION}.{payload}");
    let mut mac = HmacSha256::new_from_slice(key).unwrap();
    mac.update(message.as_bytes());
    let signature = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
    format!("{message}.{signature}")
}

fn validate_window(issued_at: i64, expires_at: i64, now: i64) -> Result<(), PurposeError> {
    if expires_at <= issued_at
        || now + CLOCK_SKEW_SECONDS < issued_at
        || now - CLOCK_SKEW_SECONDS > expires_at
    {
        return Err(PurposeError::ExpiredAttestation);
    }
    Ok(())
}

fn validate_field(name: &str, value: &str) -> Result<(), PurposeError> {
    let value = value.trim();
    if value.is_empty() || value.len() > MAX_FIELD_BYTES || value.chars().any(char::is_control) {
        return Err(PurposeError::Invalid(format!("invalid {name}")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn purpose(token: Option<String>) -> RecallPurpose {
        RecallPurpose {
            request_id: "req-1".into(),
            namespace: "local".into(),
            project: "/p".into(),
            task_type: "Deploy Production".into(),
            intended_action: Some("release".into()),
            action_risk: ActionRisk::High,
            require_source_backing: true,
            purpose_attestation: token,
            confirmation_receipt: None,
        }
    }

    #[test]
    fn advisory_identity_cannot_become_trusted_authority() {
        let (verified, _) = AdvisoryPurposeVerifier
            .verify(&purpose(None), Some("agent-a"), 100)
            .unwrap();
        assert_eq!(verified.authority, PurposeAuthority::AuthenticatedAgent);
        assert!(!verified.authority.is_trusted());
        assert_eq!(verified.task_type, "deploy_production");
    }

    #[test]
    fn attestation_is_signature_expiry_identity_and_scope_bound() {
        let key = vec![7; 32];
        let claims = PurposeAttestationClaims {
            attestation_id: "att-1".into(),
            issuer: "runtime".into(),
            subject_agent_id: Some("agent-a".into()),
            request_id: "req-1".into(),
            namespace: "local".into(),
            project: "/p".into(),
            task_type: "deploy_production".into(),
            intended_action: Some("release".into()),
            action_risk: ActionRisk::High,
            require_source_backing: true,
            issued_at: 90,
            expires_at: 120,
            nonce: "nonce-1".into(),
        };
        let token = encode_signed(&claims, &key);
        let verifier = TrustedRuntimeVerifier::new(key).unwrap();
        let (verified, _) = verifier
            .verify(&purpose(Some(token.clone())), Some("agent-a"), 100)
            .unwrap();
        assert_eq!(verified.authority, PurposeAuthority::TrustedRuntime);

        assert_eq!(
            verifier
                .verify(&purpose(Some(token.clone())), Some("agent-b"), 100)
                .unwrap_err(),
            PurposeError::ScopeMismatch
        );
        assert_eq!(
            verifier
                .verify(&purpose(Some(token)), Some("agent-a"), 200)
                .unwrap_err(),
            PurposeError::ExpiredAttestation
        );
    }

    #[test]
    fn confirmation_is_bound_to_exact_purpose_and_maximum_risk() {
        let key = vec![9; 32];
        let verified = local_operator_purpose(&purpose(None), "operator", 100).unwrap();
        let claims = ConfirmationClaims {
            receipt_id: "confirm-1".into(),
            issuer: "local".into(),
            confirming_actor: "james".into(),
            request_id: verified.request_id.clone(),
            purpose_hash: purpose_hash(&verified),
            namespace: verified.namespace.clone(),
            project: verified.project.clone(),
            task_type: verified.task_type.clone(),
            intended_action: verified.intended_action.clone(),
            maximum_authorized_risk: ActionRisk::High,
            issued_at: 90,
            expires_at: 120,
            nonce: "confirm-nonce".into(),
            single_use: true,
        };
        let token = encode_signed(&claims, &key);
        assert_eq!(
            verify_confirmation(&token, &key, &verified, 100)
                .unwrap()
                .receipt_id,
            "confirm-1"
        );
        let mut critical = verified;
        critical.action_risk = ActionRisk::Critical;
        assert!(verify_confirmation(&token, &key, &critical, 100).is_err());
    }

    #[test]
    fn minted_confirmation_verifies_for_the_same_later_local_request() {
        let key = vec![11; 32];
        let wire = purpose(None);
        let token =
            mint_confirmation_token(&wire, &key, "local", "james", ActionRisk::High, 90, 120)
                .unwrap();
        let verified = local_operator_purpose(&wire, "ironmem:cli", 100).unwrap();
        assert!(verify_confirmation(&token, &key, &verified, 100).is_ok());
    }
}
