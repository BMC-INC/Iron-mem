//! Deterministic policy evaluation shared by every content-bearing surface.

use serde::{Deserialize, Serialize};

use crate::config::InfluenceConfig;
use crate::influence::{
    state_disposition, InfluenceState, InfluenceUse, MemoryInfluencePolicy,
    MemoryInfluencePolicyRecord, StateDisposition,
};
use crate::purpose::VerifiedRecallPurpose;
use crate::purpose::{
    local_operator_purpose, verify_confirmation, AdvisoryPurposeVerifier, PurposeVerifier,
    RecallPurpose, TrustedRuntimeVerifier,
};

pub const EVALUATOR_VERSION: &str = "influence-v1";

#[derive(Debug, Clone)]
pub struct CandidatePolicyContext {
    pub record: MemoryInfluencePolicyRecord,
    pub derivation_depth: u32,
    pub evidence_root_count: usize,
    pub record_hash: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InfluenceDecisionKind {
    Allow,
    AllowReasoningOnly,
    RequireOriginalSource,
    RequireHumanConfirmation,
    Deny,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ConsumerCapabilities {
    #[serde(default)]
    pub reasoning_only_channel: bool,
    #[serde(default)]
    pub exact_source_expansion: bool,
    #[serde(default)]
    pub denial_diagnostics: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InfluenceDecision {
    pub decision_id: String,
    pub request_id: String,
    pub memory_id: i64,
    pub decision: InfluenceDecisionKind,
    pub reason_codes: Vec<String>,
    pub policy: MemoryInfluencePolicy,
    pub purpose: VerifiedRecallPurpose,
    pub policy_version: u64,
    pub evaluator_version: String,
    pub config_hash: String,
    pub evidence_root_count: usize,
    pub derivation_depth: u32,
    #[serde(default)]
    pub contradiction_set_ids: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum PurposeChannel {
    LocalOperator(String),
    Remote { authenticated_agent: Option<String> },
}

#[derive(Debug, Clone, Serialize)]
pub struct GateResult {
    pub authorized: Vec<crate::db::Memory>,
    pub advisory: Vec<crate::db::Memory>,
    pub source_required: Vec<crate::db::Memory>,
    pub denied_memory_ids: Vec<i64>,
    pub decisions: Vec<InfluenceDecision>,
}

#[allow(clippy::too_many_arguments)]
pub async fn gate_memories(
    db: &crate::db::Database,
    memories: Vec<crate::db::Memory>,
    namespace: &str,
    project: &str,
    supplied_purpose: Option<&RecallPurpose>,
    channel: PurposeChannel,
    consumer: ConsumerCapabilities,
    config: &InfluenceConfig,
) -> anyhow::Result<GateResult> {
    gate_memories_with_query(
        db,
        memories,
        namespace,
        project,
        supplied_purpose,
        channel,
        consumer,
        config,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn gate_memories_with_query(
    db: &crate::db::Database,
    memories: Vec<crate::db::Memory>,
    namespace: &str,
    project: &str,
    supplied_purpose: Option<&RecallPurpose>,
    channel: PurposeChannel,
    consumer: ConsumerCapabilities,
    config: &InfluenceConfig,
    query: Option<&str>,
) -> anyhow::Result<GateResult> {
    if !config.enabled {
        return Ok(GateResult {
            authorized: memories,
            advisory: Vec::new(),
            source_required: Vec::new(),
            denied_memory_ids: Vec::new(),
            decisions: Vec::new(),
        });
    }

    let legacy;
    let wire = match supplied_purpose {
        Some(purpose) => purpose,
        None if config.require_purpose || config.mode == crate::config::InfluenceMode::Strict => {
            anyhow::bail!("purpose_required")
        }
        None => {
            legacy = RecallPurpose {
                request_id: uuid::Uuid::new_v4().to_string(),
                namespace: namespace.to_string(),
                project: project.to_string(),
                task_type: "legacy_recall".to_string(),
                intended_action: None,
                action_risk: crate::influence::ActionRisk::None,
                require_source_backing: false,
                purpose_attestation: None,
                confirmation_receipt: None,
            };
            &legacy
        }
    };
    let now = chrono::Utc::now().timestamp();
    let (mut verified, attestation_claims) = match &channel {
        PurposeChannel::LocalOperator(actor) => (local_operator_purpose(wire, actor, now)?, None),
        PurposeChannel::Remote {
            authenticated_agent,
        } if config.mode == crate::config::InfluenceMode::Strict
            || config.require_trusted_attestation =>
        {
            let path = config
                .attestation_key_file
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("trusted attestation key file is not configured"))?;
            let key = std::fs::read(path)?;
            TrustedRuntimeVerifier::new(key)?.verify(wire, authenticated_agent.as_deref(), now)?
        }
        PurposeChannel::Remote {
            authenticated_agent,
        } => AdvisoryPurposeVerifier.verify(wire, authenticated_agent.as_deref(), now)?,
    };
    if verified.namespace != crate::governance::normalize_namespace(namespace)
        || verified.project != project
    {
        anyhow::bail!("purpose_scope_mismatch");
    }
    if config.mode == crate::config::InfluenceMode::Strict && !verified.authority.is_trusted() {
        anyhow::bail!("purpose_unattested");
    }

    let confirmation_claims = if let Some(token) = wire.confirmation_receipt.as_deref() {
        let path = config
            .confirmation_key_file
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("confirmation key file is not configured"))?;
        let key = std::fs::read(path)?;
        let claims = verify_confirmation(token, &key, &verified, now)?;
        verified.confirmation_receipt_id = Some(claims.receipt_id.clone());
        Some(claims)
    } else {
        None
    };

    // Replay registration is deliberately request-scoped and occurs before any
    // memory content is released.
    if let Some(claims) = attestation_claims {
        crate::db::claim_influence_nonce(
            db,
            "purpose_attestation",
            &claims.issuer,
            &claims.nonce,
            &claims.request_id,
            claims.expires_at,
        )
        .await?;
    }
    if let Some(claims) = &confirmation_claims {
        if claims.single_use || config.confirmation_replay_protection {
            crate::db::claim_influence_nonce(
                db,
                "confirmation_receipt",
                &claims.issuer,
                &claims.nonce,
                &claims.request_id,
                claims.expires_at,
            )
            .await?;
        }
    }

    let ids: Vec<i64> = memories.iter().map(|memory| memory.id).collect();
    let contexts = crate::db::influence_contexts_for(db, &ids, namespace).await?;
    let mut result = GateResult {
        authorized: Vec::new(),
        advisory: Vec::new(),
        source_required: Vec::new(),
        denied_memory_ids: Vec::new(),
        decisions: Vec::with_capacity(memories.len()),
    };
    for memory in memories {
        let Some(context) = contexts.get(&memory.id) else {
            if config.fail_closed_on_policy_error {
                result.denied_memory_ids.push(memory.id);
                continue;
            }
            result.authorized.push(memory);
            continue;
        };
        let decision = evaluate(
            context,
            &verified,
            consumer,
            confirmation_claims.is_some(),
            config,
        );
        match decision.decision {
            InfluenceDecisionKind::Allow => result.authorized.push(memory),
            InfluenceDecisionKind::AllowReasoningOnly => result.advisory.push(memory),
            InfluenceDecisionKind::RequireOriginalSource => result.source_required.push(memory),
            InfluenceDecisionKind::RequireHumanConfirmation | InfluenceDecisionKind::Deny => {
                result.denied_memory_ids.push(memory.id)
            }
        }
        result.decisions.push(decision);
    }
    let query_hash = query.map(hash_sensitive_value);
    crate::db::record_influence_decisions(
        db,
        &result.decisions,
        &contexts,
        config.record_denials,
        query_hash.as_deref(),
    )
    .await?;
    Ok(result)
}

pub fn evaluate(
    candidate: &CandidatePolicyContext,
    purpose: &VerifiedRecallPurpose,
    consumer: ConsumerCapabilities,
    confirmation_valid: bool,
    config: &InfluenceConfig,
) -> InfluenceDecision {
    let started = crate::metrics::start();
    let policy = &candidate.record.policy;
    let action_use = purpose.intended_action.is_some()
        || !matches!(purpose.action_risk, crate::influence::ActionRisk::None);
    let use_kind = if action_use {
        InfluenceUse::Action
    } else {
        InfluenceUse::Reasoning
    };
    let mut reasons = Vec::new();
    let mut decision = match state_disposition(policy, use_kind, purpose.action_risk) {
        StateDisposition::Allow => InfluenceDecisionKind::Allow,
        StateDisposition::AllowReasoningOnly if consumer.reasoning_only_channel => {
            InfluenceDecisionKind::AllowReasoningOnly
        }
        StateDisposition::AllowReasoningOnly => {
            reasons.push("consumer_cannot_enforce_reasoning_only".to_string());
            InfluenceDecisionKind::Deny
        }
        StateDisposition::Deny => {
            reasons.push(
                match policy.state {
                    InfluenceState::Blocked => "state_blocked",
                    InfluenceState::Quarantined => "state_quarantined",
                    InfluenceState::Superseded => "state_superseded",
                    InfluenceState::ReasoningOnly => "state_reasoning_only_for_action",
                    InfluenceState::ActionRestricted => "action_risk_exceeded",
                    InfluenceState::Eligible => "policy_denied",
                }
                .to_string(),
            );
            InfluenceDecisionKind::Deny
        }
    };

    if decision != InfluenceDecisionKind::Deny {
        if policy.denied_task_types.contains(&purpose.task_type) {
            reasons.push("task_explicitly_denied".to_string());
            decision = InfluenceDecisionKind::Deny;
        } else if !policy.allowed_task_types.is_empty()
            && !policy.allowed_task_types.contains(&purpose.task_type)
        {
            reasons.push("task_not_allowed".to_string());
            decision = InfluenceDecisionKind::Deny;
        }
    }
    if decision != InfluenceDecisionKind::Deny && purpose.action_risk > policy.maximum_action_risk {
        reasons.push("action_risk_exceeded".to_string());
        decision = InfluenceDecisionKind::Deny;
    }
    if decision != InfluenceDecisionKind::Deny
        && policy
            .maximum_derivation_depth
            .is_some_and(|maximum| candidate.derivation_depth > maximum)
    {
        reasons.push("derivation_depth_exceeded".to_string());
        decision = InfluenceDecisionKind::Deny;
    }

    let source_required = policy.requires_original_source
        || purpose.require_source_backing
        || (config.high_risk_requires_source
            && purpose.action_risk >= crate::influence::ActionRisk::High);
    if decision != InfluenceDecisionKind::Deny && source_required {
        reasons.push("source_expansion_required".to_string());
        decision = if consumer.exact_source_expansion {
            InfluenceDecisionKind::RequireOriginalSource
        } else {
            InfluenceDecisionKind::Deny
        };
    }

    let confirmation_required = policy.requires_human_confirmation
        || (config.critical_risk_requires_confirmation
            && purpose.action_risk == crate::influence::ActionRisk::Critical);
    if decision != InfluenceDecisionKind::Deny && confirmation_required && !confirmation_valid {
        reasons.push("human_confirmation_required".to_string());
        decision = InfluenceDecisionKind::RequireHumanConfirmation;
    }

    let result = InfluenceDecision {
        decision_id: uuid::Uuid::new_v4().to_string(),
        request_id: purpose.request_id.clone(),
        memory_id: candidate.record.memory_id,
        decision,
        reason_codes: reasons,
        policy: policy.clone(),
        purpose: purpose.clone(),
        policy_version: policy.version,
        evaluator_version: EVALUATOR_VERSION.to_string(),
        config_hash: config_hash(config),
        evidence_root_count: candidate.evidence_root_count,
        derivation_depth: candidate.derivation_depth,
        contradiction_set_ids: Vec::new(),
    };
    crate::metrics::record(crate::metrics::GovOp::InfluenceEval, started.elapsed());
    result
}

fn config_hash(config: &InfluenceConfig) -> String {
    use sha2::{Digest, Sha256};
    let bytes = serde_json::to_vec(config).expect("influence config is serializable");
    format!("{:x}", Sha256::digest(bytes))
}

fn hash_sensitive_value(value: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::influence::{ActionRisk, InfluenceState};
    use crate::purpose::{PurposeAuthority, VerifiedRecallPurpose};
    use sqlx::Row;

    fn purpose(task: &str, risk: ActionRisk) -> VerifiedRecallPurpose {
        VerifiedRecallPurpose {
            request_id: "r1".into(),
            subject_agent_id: Some("local".into()),
            namespace: "local".into(),
            project: "/p".into(),
            task_type: task.into(),
            intended_action: (risk != ActionRisk::None).then(|| "act".into()),
            action_risk: risk,
            require_source_backing: false,
            authority: PurposeAuthority::LocalOperator,
            attestation_id: None,
            confirmation_receipt_id: None,
            issued_at: 1,
            expires_at: None,
        }
    }

    fn candidate(policy: MemoryInfluencePolicy) -> CandidatePolicyContext {
        CandidatePolicyContext {
            record: MemoryInfluencePolicyRecord {
                memory_id: 7,
                namespace: "local".into(),
                policy,
                explicit: true,
                updated_by: None,
                updated_at: None,
            },
            derivation_depth: 2,
            evidence_root_count: 1,
            record_hash: None,
        }
    }

    #[test]
    fn deny_overrides_allow_and_unknown_task_fails_positive_allowlist() {
        let mut policy = MemoryInfluencePolicy {
            allowed_task_types: vec!["deploy".into()],
            denied_task_types: vec!["deploy".into()],
            ..Default::default()
        };
        let d = evaluate(
            &candidate(policy.clone()),
            &purpose("deploy", ActionRisk::Low),
            ConsumerCapabilities::default(),
            false,
            &InfluenceConfig::default(),
        );
        assert_eq!(d.decision, InfluenceDecisionKind::Deny);
        assert!(d.reason_codes.contains(&"task_explicitly_denied".into()));
        policy.denied_task_types.clear();
        let d = evaluate(
            &candidate(policy),
            &purpose("unknown", ActionRisk::Low),
            ConsumerCapabilities::default(),
            false,
            &InfluenceConfig::default(),
        );
        assert_eq!(d.decision, InfluenceDecisionKind::Deny);
    }

    #[test]
    fn reasoning_only_requires_a_capable_consumer() {
        let policy = MemoryInfluencePolicy {
            state: InfluenceState::ReasoningOnly,
            ..Default::default()
        };
        let incapable = evaluate(
            &candidate(policy.clone()),
            &purpose("answer", ActionRisk::None),
            ConsumerCapabilities::default(),
            false,
            &InfluenceConfig::default(),
        );
        assert_eq!(incapable.decision, InfluenceDecisionKind::Deny);
        let capable = evaluate(
            &candidate(policy),
            &purpose("answer", ActionRisk::None),
            ConsumerCapabilities {
                reasoning_only_channel: true,
                ..Default::default()
            },
            false,
            &InfluenceConfig::default(),
        );
        assert_eq!(capable.decision, InfluenceDecisionKind::AllowReasoningOnly);
    }

    #[test]
    fn source_confirmation_depth_and_risk_are_enforced() {
        let mut policy = MemoryInfluencePolicy {
            maximum_action_risk: ActionRisk::Medium,
            ..Default::default()
        };
        assert_eq!(
            evaluate(
                &candidate(policy.clone()),
                &purpose("deploy", ActionRisk::High),
                ConsumerCapabilities::default(),
                false,
                &InfluenceConfig::default(),
            )
            .decision,
            InfluenceDecisionKind::Deny
        );
        policy.maximum_action_risk = ActionRisk::Critical;
        policy.maximum_derivation_depth = Some(1);
        assert_eq!(
            evaluate(
                &candidate(policy),
                &purpose("answer", ActionRisk::None),
                ConsumerCapabilities::default(),
                false,
                &InfluenceConfig::default(),
            )
            .decision,
            InfluenceDecisionKind::Deny
        );
    }

    #[test]
    fn evaluator_is_bounded_well_below_the_local_latency_budget() {
        let candidate = candidate(MemoryInfluencePolicy::default());
        let purpose = purpose("answer", ActionRisk::None);
        let started = std::time::Instant::now();
        for _ in 0..1_000 {
            let decision = evaluate(
                &candidate,
                &purpose,
                ConsumerCapabilities::default(),
                false,
                &InfluenceConfig::default(),
            );
            assert_eq!(decision.decision, InfluenceDecisionKind::Allow);
        }
        assert!(started.elapsed().as_micros() / 1_000 < 1_000);
    }

    #[tokio::test]
    async fn shared_gate_denies_content_records_receipt_and_rejects_replay() -> anyhow::Result<()> {
        let path = std::env::temp_dir().join(format!("ironmem-egress-{}.db", uuid::Uuid::new_v4()));
        let db = crate::db::Database::new(path.to_str().unwrap()).await?;
        db.migrate().await?;
        let project = "/tmp/governed-egress";
        let session = crate::db::create_session(&db, project).await?;
        let memory_id =
            crate::db::insert_memory(&db, project, &session, "secret fact", None).await?;
        let principal = crate::influence::PolicyPrincipal::local_operator("test");
        crate::influence::update_memory_policy(
            &db,
            &principal,
            memory_id,
            "local",
            &crate::influence::PolicyMutationRequest {
                expected_version: 1,
                patch: crate::influence::MemoryInfluencePolicyPatch {
                    state: Some(InfluenceState::Blocked),
                    ..Default::default()
                },
                reason: "test block".into(),
                request_id: "policy-1".into(),
            },
        )
        .await?;
        let memory = crate::db::get_memory_by_id(&db, memory_id).await?.unwrap();
        let config = InfluenceConfig {
            enabled: true,
            ..Default::default()
        };
        let gate = gate_memories_with_query(
            &db,
            vec![memory],
            "local",
            project,
            None,
            PurposeChannel::LocalOperator("test".into()),
            ConsumerCapabilities::default(),
            &config,
            Some("private deployment query"),
        )
        .await?;
        assert!(gate.authorized.is_empty());
        assert_eq!(gate.denied_memory_ids, vec![memory_id]);
        let row = sqlx::query(
            "SELECT COUNT(*) AS count, MAX(query_hash) AS query_hash
               FROM memory_influence_events",
        )
        .fetch_one(&db.pool)
        .await?;
        assert_eq!(row.get::<i64, _>("count"), 1);
        assert_eq!(
            row.get::<String, _>("query_hash"),
            hash_sensitive_value("private deployment query")
        );

        let replay_nonce = uuid::Uuid::new_v4().to_string();
        crate::db::claim_influence_nonce(
            &db,
            "purpose_attestation",
            "issuer",
            &replay_nonce,
            "r",
            9999999999,
        )
        .await?;
        assert!(crate::db::claim_influence_nonce(
            &db,
            "purpose_attestation",
            "issuer",
            &replay_nonce,
            "r",
            9999999999
        )
        .await
        .is_err());
        drop(db);
        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[tokio::test]
    async fn confirmation_receipt_is_scope_bound_and_single_use_at_the_gate() -> anyhow::Result<()>
    {
        let path =
            std::env::temp_dir().join(format!("ironmem-confirmation-{}.db", uuid::Uuid::new_v4()));
        let key_path = path.with_extension("key");
        let key = vec![13; 32];
        std::fs::write(&key_path, &key)?;
        let db = crate::db::Database::new(path.to_str().unwrap()).await?;
        db.migrate().await?;
        let project = "/tmp/confirmation";
        let session = crate::db::create_session(&db, project).await?;
        let memory_id =
            crate::db::insert_memory(&db, project, &session, "confirmed fact", None).await?;
        crate::influence::update_memory_policy(
            &db,
            &crate::influence::PolicyPrincipal::local_operator("test"),
            memory_id,
            "local",
            &crate::influence::PolicyMutationRequest {
                expected_version: 1,
                patch: crate::influence::MemoryInfluencePolicyPatch {
                    requires_human_confirmation: Some(true),
                    ..Default::default()
                },
                reason: "confirmation test".into(),
                request_id: "confirmation-policy".into(),
            },
        )
        .await?;
        let mut wire = crate::purpose::RecallPurpose {
            request_id: "confirmed-request".into(),
            namespace: "local".into(),
            project: project.into(),
            task_type: "deploy".into(),
            intended_action: Some("release".into()),
            action_risk: ActionRisk::High,
            require_source_backing: false,
            purpose_attestation: None,
            confirmation_receipt: None,
        };
        let now = chrono::Utc::now().timestamp();
        wire.confirmation_receipt = Some(crate::purpose::mint_confirmation_token(
            &wire,
            &key,
            "local",
            "james",
            ActionRisk::High,
            now,
            now + 300,
        )?);
        let config = InfluenceConfig {
            enabled: true,
            confirmation_key_file: Some(key_path.to_string_lossy().to_string()),
            ..Default::default()
        };
        let memory = crate::db::get_memory_by_id(&db, memory_id).await?.unwrap();
        let first = gate_memories(
            &db,
            vec![memory.clone()],
            "local",
            project,
            Some(&wire),
            PurposeChannel::LocalOperator("ironmem:cli".into()),
            ConsumerCapabilities::default(),
            &config,
        )
        .await?;
        assert_eq!(first.authorized.len(), 1);
        assert!(gate_memories(
            &db,
            vec![memory],
            "local",
            project,
            Some(&wire),
            PurposeChannel::LocalOperator("ironmem:cli".into()),
            ConsumerCapabilities::default(),
            &config,
        )
        .await
        .is_err());
        drop(db);
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(key_path);
        Ok(())
    }
}
