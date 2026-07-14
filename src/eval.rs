use anyhow::{bail, Result};
use chrono::Utc;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::compress;
use crate::config::{Config, Weights};
use crate::db::{self, Database, NewMemoryChunk, NewMemoryEdge};
use crate::governance::{
    ConsentState, DataClassification, MemoryGovernance, MemorySourceType, TrustTier,
};
use crate::retrieval;
use crate::vectorstore::BruteForceStore;

#[derive(Debug, Clone)]
pub struct EvalCase {
    pub name: String,
    pub passed: bool,
    pub detail: String,
}

impl EvalCase {
    fn new(name: &str, passed: bool, detail: String) -> Self {
        Self {
            name: name.to_string(),
            passed,
            detail,
        }
    }
}

#[derive(Debug, Clone)]
pub struct EvalReport {
    pub command: String,
    pub commit: String,
    pub model: String,
    pub generated_at: String,
    pub cases: Vec<EvalCase>,
    pub output_path: Option<PathBuf>,
}

impl EvalReport {
    pub fn passed(&self) -> usize {
        self.cases.iter().filter(|c| c.passed).count()
    }

    pub fn failed(&self) -> usize {
        self.cases.len().saturating_sub(self.passed())
    }

    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("# IronMem Evaluation Report\n\n");
        out.push_str(&format!("- generated_at: `{}`\n", self.generated_at));
        out.push_str(&format!("- command: `{}`\n", self.command));
        out.push_str(&format!("- commit: `{}`\n", self.commit));
        out.push_str(&format!("- model: `{}`\n", self.model));
        out.push_str(&format!(
            "- result: `{}/{} passed`\n\n",
            self.passed(),
            self.cases.len()
        ));
        out.push_str("| Case | Result | Detail |\n");
        out.push_str("| --- | --- | --- |\n");
        for case in &self.cases {
            out.push_str(&format!(
                "| {} | {} | {} |\n",
                case.name,
                if case.passed { "pass" } else { "fail" },
                case.detail.replace('|', "\\|")
            ));
        }
        out
    }
}

pub async fn run(cfg: &Config, out_dir: &Path) -> Result<EvalReport> {
    let generated_at = Utc::now().to_rfc3339();
    let command = "ironmem eval".to_string();
    let commit = git_commit();
    let model = cfg.model.clone();

    let temp_path = std::env::temp_dir().join(format!("ironmem-eval-{}.db", uuid::Uuid::new_v4()));
    let db = Database::new(&temp_path.to_string_lossy()).await?;
    db.migrate().await?;

    let mut cases = Vec::new();
    // Legacy trio (kept verbatim so historical reports stay comparable).
    cases.push(eval_graph_relation_recall(&db).await?);
    cases.push(eval_temporal_update_recall(&db).await?);
    cases.push(eval_procedural_ranking(&db).await?);
    cases.extend(eval_multi_hop_cluster(&db).await?);
    cases.extend(eval_temporal_cluster(&db).await?);
    cases.extend(eval_open_domain_cluster(&db).await?);
    cases.extend(eval_knowledge_update_cluster(&db).await?);
    cases.extend(eval_abstention_cluster(&db).await?);
    cases.extend(eval_governance_cluster(&db).await?);
    cases.extend(eval_entity_cluster(&db).await?);
    cases.extend(eval_chunk_cluster(&db).await?);
    cases.extend(eval_ranking_lever_cluster(&db).await?);
    cases.extend(eval_compliance_cluster(&db).await?);

    let mut report = EvalReport {
        command,
        commit,
        model,
        generated_at,
        cases,
        output_path: None,
    };

    std::fs::create_dir_all(out_dir)?;
    let filename = format!(
        "{}-ironmem-eval.md",
        report.generated_at.replace([':', '.'], "-")
    );
    let path = out_dir.join(filename);
    std::fs::write(&path, report.to_markdown())?;
    report.output_path = Some(path);

    let _ = std::fs::remove_file(temp_path);
    Ok(report)
}

async fn search(db: &Database, project: &str, query: &str, limit: usize) -> Result<Vec<i64>> {
    Ok(
        retrieval::hybrid_search(db, None, &BruteForceStore, Some(project), query, limit)
            .await?
            .into_iter()
            .map(|m| m.id)
            .collect(),
    )
}

async fn store(db: &Database, project: &str, text: &str) -> Result<i64> {
    let session = db::create_session(db, project).await?;
    db::insert_memory(db, project, &session, text, Some("eval")).await
}

// --- legacy cases -----------------------------------------------------------

async fn eval_graph_relation_recall(db: &Database) -> Result<EvalCase> {
    let project = "/tmp/ironmem-eval-graph";
    let session = db::create_session(db, project).await?;
    let memory_id =
        db::insert_memory(db, project, &session, "zzz provenance alpha", Some("eval")).await?;
    db::insert_memory_edge(
        db,
        &NewMemoryEdge {
            project: project.to_string(),
            memory_id,
            source: "Caroline".to_string(),
            relation: "assigned_to".to_string(),
            target: "Operator OS".to_string(),
            valid_from: Some("2026-06-20".to_string()),
            valid_until: None,
            confidence: 0.95,
        },
    )
    .await?;

    let hits = search(db, project, "What is Caroline assigned to?", 1).await?;
    let passed = hits.first() == Some(&memory_id);
    Ok(EvalCase::new(
        "graph_relation_recall",
        passed,
        format!("expected memory {memory_id}, got {:?}", hits.first()),
    ))
}

async fn eval_temporal_update_recall(db: &Database) -> Result<EvalCase> {
    let project = "/tmp/ironmem-eval-temporal";
    let session = db::create_session(db, project).await?;
    let draft =
        db::insert_memory(db, project, &session, "Caroline status draft", Some("eval")).await?;
    let approved = db::insert_memory(
        db,
        project,
        &session,
        "Caroline status approved",
        Some("eval"),
    )
    .await?;

    db::insert_memory_edge(
        db,
        &NewMemoryEdge {
            project: project.to_string(),
            memory_id: draft,
            source: "Caroline".to_string(),
            relation: "status".to_string(),
            target: "draft".to_string(),
            valid_from: Some("2026-06-01".to_string()),
            valid_until: None,
            confidence: 0.9,
        },
    )
    .await?;
    db::insert_memory_edge(
        db,
        &NewMemoryEdge {
            project: project.to_string(),
            memory_id: approved,
            source: "Caroline".to_string(),
            relation: "status".to_string(),
            target: "approved".to_string(),
            valid_from: Some("2026-06-05".to_string()),
            valid_until: None,
            confidence: 0.95,
        },
    )
    .await?;

    let current = db::memory_edges_for_entity_at(
        db,
        Some(project),
        "Caroline",
        false,
        Some("2026-06-06"),
        10,
    )
    .await?;
    let past =
        db::memory_edges_for_entity_at(db, Some(project), "Caroline", true, Some("2026-06-03"), 10)
            .await?;
    let passed =
        current.iter().any(|e| e.target == "approved") && past.iter().any(|e| e.target == "draft");
    Ok(EvalCase::new(
        "temporal_update_recall",
        passed,
        format!(
            "current={:?}; past={:?}",
            current.iter().map(|e| &e.target).collect::<Vec<_>>(),
            past.iter().map(|e| &e.target).collect::<Vec<_>>()
        ),
    ))
}

async fn eval_procedural_ranking(db: &Database) -> Result<EvalCase> {
    let project = "/tmp/ironmem-eval-procedural";
    let session = db::create_session(db, project).await?;
    let session_id =
        db::insert_memory(db, project, &session, "ordinary session note", Some("eval")).await?;
    db::upsert_memory_meta(db, session_id, 0.5).await?;
    db::set_memory_scope_kind(db, session_id, "project", "session").await?;

    let proc_id = db::insert_memory(
        db,
        project,
        &session,
        "Keep tenant isolation explicit before shared memory.",
        Some("eval procedural"),
    )
    .await?;
    db::upsert_memory_meta(db, proc_id, 0.75).await?;
    db::set_memory_scope_kind(db, proc_id, "project", "procedural").await?;

    let ranked = retrieval::injection_rank(
        db,
        None,
        &BruteForceStore,
        project,
        None,
        &Weights::default(),
        30.0,
        2,
    )
    .await?;
    let passed = ranked.first().map(|m| m.id) == Some(proc_id);
    Ok(EvalCase::new(
        "procedural_ranking",
        passed,
        format!(
            "expected procedural {proc_id}, got {:?}",
            ranked.first().map(|m| m.id)
        ),
    ))
}

// --- multi-hop cluster ------------------------------------------------------
// LoCoMo multi-hop is IronMem's weakest category (52.5%). These cases pin the
// graph-bridge behaviors retrieval already has so Phase 1 work can only
// improve them.

async fn eval_multi_hop_cluster(db: &Database) -> Result<Vec<EvalCase>> {
    let mut cases = Vec::new();
    let project = "/tmp/ironmem-eval-multihop";

    let works = store(db, project, "Alice works at Acme as a staff engineer").await?;
    db::insert_memory_edge(
        db,
        &NewMemoryEdge {
            project: project.to_string(),
            memory_id: works,
            source: "Alice".to_string(),
            relation: "works_at".to_string(),
            target: "Acme".to_string(),
            valid_from: None,
            valid_until: None,
            confidence: 0.95,
        },
    )
    .await?;
    let located = store(db, project, "Acme headquarters sit in Berlin").await?;
    db::insert_memory_edge(
        db,
        &NewMemoryEdge {
            project: project.to_string(),
            memory_id: located,
            source: "Acme".to_string(),
            relation: "located_in".to_string(),
            target: "Berlin".to_string(),
            valid_from: None,
            valid_until: None,
            confidence: 0.95,
        },
    )
    .await?;

    // Both hops surface for an explicit bridge question.
    let hits = search(db, project, "How is Alice connected to Berlin?", 3).await?;
    cases.push(EvalCase::new(
        "multi_hop_bridge_recall",
        hits.contains(&works) && hits.contains(&located),
        format!("expected {works} and {located} in top-3, got {hits:?}"),
    ));

    // The second hop shares no tokens with the query: only the graph chain
    // (Alice -> Acme -> Berlin) can surface it.
    let hits = search(
        db,
        project,
        "Which city is related to Alice through her employer?",
        3,
    )
    .await?;
    cases.push(EvalCase::new(
        "multi_hop_chain_second_hop",
        hits.contains(&located),
        format!("expected bridged memory {located} in top-3, got {hits:?}"),
    ));

    let dep_project = "/tmp/ironmem-eval-multihop-dep";
    let dep = store(
        db,
        dep_project,
        "The billing service depends on the auth library",
    )
    .await?;
    db::insert_memory_edge(
        db,
        &NewMemoryEdge {
            project: dep_project.to_string(),
            memory_id: dep,
            source: "billing service".to_string(),
            relation: "depends_on".to_string(),
            target: "auth library".to_string(),
            valid_from: None,
            valid_until: None,
            confidence: 0.9,
        },
    )
    .await?;
    store(db, dep_project, "The docs site build was migrated to CI").await?;
    let hits = search(
        db,
        dep_project,
        "Which service depends on the auth library?",
        1,
    )
    .await?;
    cases.push(EvalCase::new(
        "multi_hop_dependency_recall",
        hits.first() == Some(&dep),
        format!("expected {dep} first, got {hits:?}"),
    ));

    let shared_project = "/tmp/ironmem-eval-multihop-shared";
    let m1 = store(
        db,
        shared_project,
        "Caroline shipped the launch announcement",
    )
    .await?;
    let m2 = store(db, shared_project, "Caroline hosted a retro afterwards").await?;
    let hits = search(
        db,
        shared_project,
        "What else did Caroline do after the launch?",
        2,
    )
    .await?;
    cases.push(EvalCase::new(
        "multi_hop_shared_entity_expansion",
        hits.contains(&m1) && hits.contains(&m2),
        format!("expected {m1} and {m2} in top-2, got {hits:?}"),
    ));

    let positives = [
        "How is Alice connected to Berlin?",
        "What happened after the migration?",
        "Which service depends on the auth library?",
    ];
    let negatives = ["What is the capital of France?", "lunch policy"];
    let route_ok = positives.iter().all(|q| retrieval::is_multi_hop_query(q))
        && negatives.iter().all(|q| !retrieval::is_multi_hop_query(q));
    cases.push(EvalCase::new(
        "multi_hop_route_detection",
        route_ok,
        format!(
            "positives={:?} negatives={:?}",
            positives
                .iter()
                .map(|q| retrieval::is_multi_hop_query(q))
                .collect::<Vec<_>>(),
            negatives
                .iter()
                .map(|q| retrieval::is_multi_hop_query(q))
                .collect::<Vec<_>>()
        ),
    ));

    // Graph signals must not displace the direct answer for a direct question.
    let hits = search(db, project, "Where does Alice work?", 1).await?;
    cases.push(EvalCase::new(
        "multi_hop_direct_still_first",
        hits.first() == Some(&works),
        format!("expected direct memory {works} first, got {hits:?}"),
    ));

    Ok(cases)
}

// --- temporal cluster -------------------------------------------------------

async fn eval_temporal_cluster(db: &Database) -> Result<Vec<EvalCase>> {
    let mut cases = Vec::new();
    let project = "/tmp/ironmem-eval-temporal-events";

    let rome = store(db, project, "Caroline visited Rome for the spring workshop").await?;
    db::set_memory_event_time(db, rome, "2022-04-01").await?;
    let paris = store(db, project, "Caroline visited Paris for the summit").await?;
    db::set_memory_event_time(db, paris, "2023-06-01").await?;

    let hits = search(db, project, "Where did Caroline travel in 2022?", 2).await?;
    cases.push(EvalCase::new(
        "temporal_event_time_recall",
        hits.contains(&rome),
        format!("expected {rome} in top-2, got {hits:?}"),
    ));

    let hits = search(db, project, "Where did Caroline travel in 2022?", 2).await?;
    let rome_rank = hits.iter().position(|id| *id == rome);
    let paris_rank = hits.iter().position(|id| *id == paris);
    cases.push(EvalCase::new(
        "temporal_year_ordering",
        match (rome_rank, paris_rank) {
            (Some(r), Some(p)) => r < p,
            (Some(_), None) => true,
            _ => false,
        },
        format!("rome_rank={rome_rank:?} paris_rank={paris_rank:?} hits={hits:?}"),
    ));

    let bound_project = "/tmp/ironmem-eval-temporal-bound";
    let m = store(db, bound_project, "Melanie became team lead").await?;
    db::insert_memory_edge(
        db,
        &NewMemoryEdge {
            project: bound_project.to_string(),
            memory_id: m,
            source: "Melanie".to_string(),
            relation: "role".to_string(),
            target: "team lead".to_string(),
            valid_from: Some("2026-01-15".to_string()),
            valid_until: None,
            confidence: 0.9,
        },
    )
    .await?;
    let at_start = db::memory_edges_for_entity_at(
        db,
        Some(bound_project),
        "Melanie",
        false,
        Some("2026-01-15"),
        5,
    )
    .await?;
    cases.push(EvalCase::new(
        "temporal_validity_boundary_inclusive",
        at_start.iter().any(|e| e.target == "team lead"),
        format!(
            "edges at valid_from date: {:?}",
            at_start.iter().map(|e| &e.target).collect::<Vec<_>>()
        ),
    ));

    let before = db::memory_edges_for_entity_at(
        db,
        Some(bound_project),
        "Melanie",
        false,
        Some("2026-01-01"),
        5,
    )
    .await?;
    cases.push(EvalCase::new(
        "temporal_validity_before_start_excluded",
        !before.iter().any(|e| e.target == "team lead"),
        format!(
            "edges before valid_from: {:?}",
            before.iter().map(|e| &e.target).collect::<Vec<_>>()
        ),
    ));

    let hits = search(db, project, "When did Caroline visit Paris?", 2).await?;
    cases.push(EvalCase::new(
        "temporal_when_question_recall",
        hits.first() == Some(&paris),
        format!("expected {paris} first, got {hits:?}"),
    ));

    Ok(cases)
}

// --- open-domain cluster ----------------------------------------------------
// LoCoMo open-domain is the other weak category (50.0%). Deterministic recall
// here runs without embeddings, so these pin the keyword/FTS floor.

async fn eval_open_domain_cluster(db: &Database) -> Result<Vec<EvalCase>> {
    let mut cases = Vec::new();
    let project = "/tmp/ironmem-eval-opendomain";

    let policy = store(
        db,
        project,
        "The team adopted a burrito friendly lunch policy on Fridays",
    )
    .await?;
    let hits = search(db, project, "lunch policy", 3).await?;
    cases.push(EvalCase::new(
        "open_domain_keyword_recall",
        hits.first() == Some(&policy),
        format!("expected {policy} first, got {hits:?}"),
    ));

    let painting = store(db, project, "Melanie painted a sunrise on canvas in 2022").await?;
    let hits = search(db, project, "sunrise canvas artwork", 3).await?;
    cases.push(EvalCase::new(
        "open_domain_partial_token_recall",
        hits.contains(&painting),
        format!("expected {painting} in top-3, got {hits:?}"),
    ));

    let pool_project = "/tmp/ironmem-eval-opendomain-pool";
    for i in 0..12 {
        store(
            db,
            pool_project,
            &format!("Routine deployment note number {i} with no special content"),
        )
        .await?;
    }
    let target = store(
        db,
        pool_project,
        "The observability dashboard tracks tail latency percentiles",
    )
    .await?;
    let hits = search(db, pool_project, "tail latency percentiles", 5).await?;
    cases.push(EvalCase::new(
        "open_domain_wide_pool_surfacing",
        hits.first() == Some(&target),
        format!("expected {target} first among 13 memories, got {hits:?}"),
    ));

    let dual_project = "/tmp/ironmem-eval-opendomain-dual";
    let session = db::create_session(db, dual_project).await?;
    let narrative = db::insert_memory(
        db,
        dual_project,
        &session,
        "Long session covering many topics including a discussion of the vineyard trip and other plans",
        Some("eval"),
    )
    .await?;
    db::set_memory_scope_kind(db, narrative, "project", "session").await?;
    let fact = db::insert_memory(
        db,
        dual_project,
        &session,
        "Caroline booked the vineyard trip for 12 September 2025",
        Some("eval"),
    )
    .await?;
    db::set_memory_scope_kind(db, fact, "project", "fact").await?;
    // Both the atomic fact and the narrative surface today; ranking the fact
    // *above* the narrative for specific queries is a Phase 1 target.
    let hits = search(db, dual_project, "vineyard trip booking date", 2).await?;
    cases.push(EvalCase::new(
        "open_domain_fact_retrievable_beside_narrative",
        hits.contains(&fact) && hits.contains(&narrative),
        format!("expected fact {fact} and narrative {narrative} in top-2, got {hits:?}"),
    ));

    let phrase = store(
        db,
        project,
        "Rotate the staging signing key every ninety days",
    )
    .await?;
    let hits = search(db, project, "staging signing key rotation", 3).await?;
    cases.push(EvalCase::new(
        "open_domain_phrase_recall",
        hits.contains(&phrase),
        format!("expected {phrase} in top-3, got {hits:?}"),
    ));

    Ok(cases)
}

// --- knowledge-update cluster -----------------------------------------------
// LongMemEval scores knowledge updates explicitly; these pin the supersession
// machinery that Phase 1 will start enforcing at ranking time.

async fn eval_knowledge_update_cluster(db: &Database) -> Result<Vec<EvalCase>> {
    let mut cases = Vec::new();
    let project = "/tmp/ironmem-eval-knowledge-update";

    let old_m = store(db, project, "Bob status onboarding").await?;
    db::insert_memory_edge(
        db,
        &NewMemoryEdge {
            project: project.to_string(),
            memory_id: old_m,
            source: "Bob".to_string(),
            relation: "status".to_string(),
            target: "onboarding".to_string(),
            valid_from: Some("2026-05-01".to_string()),
            valid_until: None,
            confidence: 0.9,
        },
    )
    .await?;
    let new_m = store(db, project, "Bob status active").await?;
    db::insert_memory_edge(
        db,
        &NewMemoryEdge {
            project: project.to_string(),
            memory_id: new_m,
            source: "Bob".to_string(),
            relation: "status".to_string(),
            target: "active".to_string(),
            valid_from: Some("2026-06-01".to_string()),
            valid_until: None,
            confidence: 0.95,
        },
    )
    .await?;

    let current = db::memory_edges_for_entity_at(db, Some(project), "Bob", false, None, 10).await?;
    cases.push(EvalCase::new(
        "knowledge_update_current_state_supersedes",
        current.iter().any(|e| e.target == "active")
            && !current.iter().any(|e| e.target == "onboarding"),
        format!(
            "active edges: {:?}",
            current.iter().map(|e| &e.target).collect::<Vec<_>>()
        ),
    ));

    let history = db::memory_edges_for_entity_at(db, Some(project), "Bob", true, None, 10).await?;
    cases.push(EvalCase::new(
        "knowledge_update_history_preserved",
        history.iter().any(|e| e.target == "onboarding"),
        format!(
            "historical edges: {:?}",
            history.iter().map(|e| &e.target).collect::<Vec<_>>()
        ),
    ));

    let drink_project = "/tmp/ironmem-eval-knowledge-drink";
    store(db, drink_project, "Dana prefers coffee in the morning").await?;
    let fresh = store(
        db,
        drink_project,
        "Dana now prefers green tea in the morning",
    )
    .await?;
    let hits = search(
        db,
        drink_project,
        "What does Dana prefer in the morning?",
        2,
    )
    .await?;
    cases.push(EvalCase::new(
        "knowledge_update_fresh_fact_retrievable",
        hits.contains(&fresh),
        format!("expected fresh fact {fresh} in top-2, got {hits:?}"),
    ));

    cases.push(EvalCase::new(
        "knowledge_update_negative_feedback_decays",
        db::reinforcement_multiplier(-1.0, 0) < 1.0,
        format!(
            "reinforcement_multiplier(-1.0, 0) = {}",
            db::reinforcement_multiplier(-1.0, 0)
        ),
    ));
    cases.push(EvalCase::new(
        "knowledge_update_positive_feedback_reinforces",
        db::reinforcement_multiplier(1.5, 3) > 1.0,
        format!(
            "reinforcement_multiplier(1.5, 3) = {}",
            db::reinforcement_multiplier(1.5, 3)
        ),
    ));

    let fb_id = db::record_memory_feedback(
        db,
        fresh,
        drink_project,
        "correction",
        -1.0,
        Some("stale preference"),
    )
    .await?;
    let feedback = db::feedback_for_memory(db, fresh).await?;
    cases.push(EvalCase::new(
        "knowledge_update_feedback_roundtrip",
        fb_id > 0 && feedback.iter().any(|f| f.signal == "correction"),
        format!(
            "feedback signals: {:?}",
            feedback.iter().map(|f| &f.signal).collect::<Vec<_>>()
        ),
    ));

    Ok(cases)
}

// --- abstention cluster -----------------------------------------------------
// LongMemEval scores abstention; retrieval must return nothing (not the best
// bad hit) when no supporting memory exists, and must keep tombstoned/expired
// memories out of recall.

async fn eval_abstention_cluster(db: &Database) -> Result<Vec<EvalCase>> {
    let mut cases = Vec::new();

    let hits = search(
        db,
        "/tmp/ironmem-eval-abstention-empty",
        "What is the deployment cadence?",
        5,
    )
    .await?;
    cases.push(EvalCase::new(
        "abstention_empty_project",
        hits.is_empty(),
        format!("expected no hits from empty project, got {hits:?}"),
    ));

    let project = "/tmp/ironmem-eval-abstention";
    store(db, project, "The api gateway rollout finished cleanly").await?;
    let hits = search(db, project, "zxqvite plumbus grommetary", 5).await?;
    cases.push(EvalCase::new(
        "abstention_no_match_returns_empty",
        hits.is_empty(),
        format!("expected no hits for gibberish query, got {hits:?}"),
    ));

    let tomb_project = "/tmp/ironmem-eval-abstention-tomb";
    let doomed = compress::remember(
        db,
        None,
        &BruteForceStore,
        tomb_project,
        "project",
        "fact",
        "The legacy queue password rotation runbook is deprecated",
        Some("eval"),
    )
    .await?;
    let before = search(db, tomb_project, "legacy queue runbook", 3).await?;
    let deleted =
        db::governed_delete_memory(db, doomed, Some("eval"), Some("eval cleanup")).await?;
    let after = search(db, tomb_project, "legacy queue runbook", 3).await?;
    cases.push(EvalCase::new(
        "abstention_tombstoned_excluded",
        before.contains(&doomed) && deleted && !after.contains(&doomed),
        format!("before={before:?} deleted={deleted} after={after:?}"),
    ));

    let exp_project = "/tmp/ironmem-eval-abstention-expired";
    let expired = compress::remember_with_governance(
        db,
        None,
        &BruteForceStore,
        exp_project,
        "project",
        "fact",
        "The trial license token expires quickly",
        Some("eval"),
        MemoryGovernance {
            expires_at: Some(Utc::now().timestamp() - 3600),
            ..MemoryGovernance::explicit()
        },
    )
    .await?;
    let hits = search(db, exp_project, "trial license token", 3).await?;
    cases.push(EvalCase::new(
        "abstention_expired_excluded",
        !hits.contains(&expired),
        format!("expected expired {expired} absent, got {hits:?}"),
    ));

    Ok(cases)
}

// --- governance cluster -----------------------------------------------------
// Governance must be score-neutral: governed writes rank identically to
// ungoverned ones (trust/tier weights default to 0), fail closed on PII, and
// leave a verifiable hash-chained ledger.

async fn eval_governance_cluster(db: &Database) -> Result<Vec<EvalCase>> {
    let mut cases = Vec::new();

    // Identical corpus through the plain vs custom-governance write paths must
    // produce identical retrieval order (governance-parity, the "moat costs
    // 0pp" guarantee).
    let corpus = [
        "The ingestion worker batches uploads every five minutes",
        "Metrics cardinality was reduced by dropping the pod label",
        "The retry budget for webhook delivery is three attempts",
    ];
    let plain_project = "/tmp/ironmem-eval-gov-plain";
    let governed_project = "/tmp/ironmem-eval-gov-custom";
    for text in &corpus {
        compress::remember(
            db,
            None,
            &BruteForceStore,
            plain_project,
            "project",
            "fact",
            text,
            Some("eval"),
        )
        .await?;
        compress::remember_with_governance(
            db,
            None,
            &BruteForceStore,
            governed_project,
            "project",
            "fact",
            text,
            Some("eval"),
            MemoryGovernance {
                source_type: MemorySourceType::ToolOutput,
                trust_tier: TrustTier::Low,
                writer_identity: Some("eval:governed".to_string()),
                ..MemoryGovernance::default()
            },
        )
        .await?;
    }
    let query = "webhook delivery retry budget";
    let plain_hits =
        retrieval::hybrid_search(db, None, &BruteForceStore, Some(plain_project), query, 3).await?;
    let governed_hits =
        retrieval::hybrid_search(db, None, &BruteForceStore, Some(governed_project), query, 3)
            .await?;
    let plain_order: Vec<&str> = plain_hits.iter().map(|m| m.summary.as_str()).collect();
    let governed_order: Vec<&str> = governed_hits.iter().map(|m| m.summary.as_str()).collect();
    cases.push(EvalCase::new(
        "governance_parity_ranking",
        !plain_order.is_empty() && plain_order == governed_order,
        format!("plain={plain_order:?} governed={governed_order:?}"),
    ));

    let pii_project = "/tmp/ironmem-eval-gov-pii";
    let denied = compress::remember_with_governance(
        db,
        None,
        &BruteForceStore,
        pii_project,
        "project",
        "fact",
        "Patient contact number is 555-0100",
        Some("eval"),
        MemoryGovernance {
            classification: DataClassification::Pii,
            consent_state: None,
            ..MemoryGovernance::default()
        },
    )
    .await;
    cases.push(EvalCase::new(
        "governance_pii_fails_closed",
        denied.is_err(),
        format!("write without consent returned {denied:?}"),
    ));

    let granted = compress::remember_with_governance(
        db,
        None,
        &BruteForceStore,
        pii_project,
        "project",
        "fact",
        "Preferred contact channel is the ombudsman mailbox",
        Some("eval"),
        MemoryGovernance {
            classification: DataClassification::Pii,
            consent_state: Some(ConsentState::Granted),
            ..MemoryGovernance::default()
        },
    )
    .await?;
    let hits = search(db, pii_project, "ombudsman mailbox contact channel", 3).await?;
    cases.push(EvalCase::new(
        "governance_pii_with_consent_recall",
        hits.contains(&granted),
        format!("expected consented memory {granted} retrievable, got {hits:?}"),
    ));

    // Hash-chained ledger: every governed write appends an entry whose
    // prev_hash links to the namespace's previous entry.
    let chain_ns = "evalchain";
    let first = compress::remember_with_governance(
        db,
        None,
        &BruteForceStore,
        "/tmp/ironmem-eval-gov-ledger",
        "project",
        "fact",
        "Chain fact one",
        Some("eval"),
        MemoryGovernance {
            namespace: chain_ns.to_string(),
            ..MemoryGovernance::explicit()
        },
    )
    .await?;
    let second = compress::remember_with_governance(
        db,
        None,
        &BruteForceStore,
        "/tmp/ironmem-eval-gov-ledger",
        "project",
        "fact",
        "Chain fact two",
        Some("eval"),
        MemoryGovernance {
            namespace: chain_ns.to_string(),
            ..MemoryGovernance::explicit()
        },
    )
    .await?;
    let first_ledger = db::memory_ledger_for_memory(db, first).await?;
    let second_ledger = db::memory_ledger_for_memory(db, second).await?;
    cases.push(EvalCase::new(
        "governance_ledger_written",
        !first_ledger.is_empty() && !second_ledger.is_empty(),
        format!(
            "first has {} entries, second has {}",
            first_ledger.len(),
            second_ledger.len()
        ),
    ));

    let chain_ok = match (first_ledger.last(), second_ledger.first()) {
        (Some(a), Some(b)) => b.prev_hash.as_deref() == Some(a.entry_hash.as_str()),
        _ => false,
    };
    let latest = db::latest_ledger_hash(db, chain_ns).await?;
    let latest_ok = second_ledger
        .last()
        .map(|e| Some(e.entry_hash.clone()) == latest)
        .unwrap_or(false);
    cases.push(EvalCase::new(
        "governance_ledger_chain_linked",
        chain_ok && latest_ok,
        format!("chain_ok={chain_ok} latest_ok={latest_ok} latest={latest:?}"),
    ));

    // Namespace isolation: a tenant's memory is invisible to the default
    // namespace and visible inside its own.
    let iso_project = "/tmp/ironmem-eval-gov-namespace";
    let tenant_memory = compress::remember_with_governance(
        db,
        None,
        &BruteForceStore,
        iso_project,
        "project",
        "fact",
        "Tenant alpha quota is forty gigabytes",
        Some("eval"),
        MemoryGovernance {
            namespace: "tenant-alpha".to_string(),
            ..MemoryGovernance::explicit()
        },
    )
    .await?;
    let default_hits = search(db, iso_project, "tenant alpha quota", 3).await?;
    let tenant_hits = retrieval::hybrid_search_in_namespace(
        db,
        None,
        &BruteForceStore,
        "tenant-alpha",
        Some(iso_project),
        "tenant alpha quota",
        3,
    )
    .await?
    .into_iter()
    .map(|m| m.id)
    .collect::<Vec<_>>();
    cases.push(EvalCase::new(
        "governance_namespace_isolation",
        !default_hits.contains(&tenant_memory) && tenant_hits.contains(&tenant_memory),
        format!("default={default_hits:?} tenant={tenant_hits:?}"),
    ));

    // Governed delete leaves an auditable trail instead of erasing history.
    let del_project = "/tmp/ironmem-eval-gov-delete";
    let doomed = compress::remember(
        db,
        None,
        &BruteForceStore,
        del_project,
        "project",
        "fact",
        "Temporary marker fact for delete audit",
        Some("eval"),
    )
    .await?;
    db::governed_delete_memory(db, doomed, Some("eval"), Some("audit trail check")).await?;
    let ledger = db::memory_ledger_for_memory(db, doomed).await?;
    cases.push(EvalCase::new(
        "governance_delete_leaves_audit_trail",
        ledger.iter().any(|e| e.op_type == "forget"),
        format!(
            "ledger ops: {:?}",
            ledger.iter().map(|e| &e.op_type).collect::<Vec<_>>()
        ),
    ));

    Ok(cases)
}

// --- entity cluster ---------------------------------------------------------

async fn eval_entity_cluster(db: &Database) -> Result<Vec<EvalCase>> {
    let mut cases = Vec::new();
    let project = "/tmp/ironmem-eval-entity";

    // The entity index stores single normalized tokens (>= 3 chars), so a
    // contiguous name exercises the store+lookup pair.
    let demo = store(db, project, "The demo rehearsal went smoothly").await?;
    db::insert_memory_entity(db, demo, "TelemetryHub").await?;
    let ids = db::memories_for_entity(db, Some(project), "TelemetryHub", 5).await?;
    cases.push(EvalCase::new(
        "entity_index_recall",
        ids.contains(&demo),
        format!("expected {demo} via entity index, got {ids:?}"),
    ));

    let planning = store(db, project, "Planning notes were consolidated").await?;
    db::insert_memory_entity(db, planning, "TelemetryHub").await?;
    let ids = db::memories_for_entity(db, Some(project), "TelemetryHub", 5).await?;
    cases.push(EvalCase::new(
        "entity_multiple_memories",
        ids.contains(&demo) && ids.contains(&planning),
        format!("expected {demo} and {planning}, got {ids:?}"),
    ));

    db::delete_memory_entities(db, demo).await?;
    let ids = db::memories_for_entity(db, Some(project), "TelemetryHub", 5).await?;
    cases.push(EvalCase::new(
        "entity_delete_removes_mapping",
        !ids.contains(&demo) && ids.contains(&planning),
        format!("after delete expected only {planning}, got {ids:?}"),
    ));

    Ok(cases)
}

// --- chunk cluster ----------------------------------------------------------
// The skim layer must exist for every explicit write; Phase 1 fuses chunks
// into open-domain retrieval, so their presence is load-bearing.

async fn eval_chunk_cluster(db: &Database) -> Result<Vec<EvalCase>> {
    let mut cases = Vec::new();
    let project = "/tmp/ironmem-eval-chunks";

    let id = compress::remember(
        db,
        None,
        &BruteForceStore,
        project,
        "project",
        "fact",
        "The nightly compaction job runs at three in the morning",
        Some("eval"),
    )
    .await?;
    let chunk_map = db::chunks_for_memories(db, &[id]).await?;
    let chunks = chunk_map.get(&id).cloned().unwrap_or_default();
    cases.push(EvalCase::new(
        "chunk_skim_written_on_remember",
        !chunks.is_empty(),
        format!("remember() produced {} chunk(s)", chunks.len()),
    ));

    let roundtrip = match chunks.first() {
        Some(chunk) => db::get_memory_chunk(db, &chunk.chunk_id)
            .await?
            .map(|c| c.memory_id == id)
            .unwrap_or(false),
        None => false,
    };
    cases.push(EvalCase::new(
        "chunk_fetch_by_id",
        roundtrip,
        format!(
            "chunk id lookup roundtrip for {:?}",
            chunks.first().map(|c| &c.chunk_id)
        ),
    ));

    let replacement = NewMemoryChunk {
        chunk_id: format!("eval-chunk-{id}"),
        project: project.to_string(),
        memory_id: id,
        session_id: "remember".to_string(),
        ordinal: 0,
        density: "skim".to_string(),
        kind: "fact".to_string(),
        title: "compaction schedule".to_string(),
        summary: "Nightly compaction at 03:00".to_string(),
        source_hash: None,
        source_start: None,
        source_end: None,
        token_estimate: 8,
    };
    db::replace_memory_chunks(db, id, &[replacement]).await?;
    let chunk_map = db::chunks_for_memories(db, &[id]).await?;
    let replaced = chunk_map
        .get(&id)
        .map(|cs| cs.len() == 1 && cs[0].chunk_id == format!("eval-chunk-{id}"))
        .unwrap_or(false);
    cases.push(EvalCase::new(
        "chunk_replace_reflected",
        replaced,
        format!(
            "chunks after replace: {:?}",
            chunk_map.get(&id).map(|c| c.len())
        ),
    ));

    Ok(cases)
}

// --- ranking-lever cluster (Phase 1) ------------------------------------------
// Chunk fusion is on by default; maturity promotion is a dream-sweep dependency
// of the activation lever. Both must behave deterministically.

async fn eval_ranking_lever_cluster(db: &Database) -> Result<Vec<EvalCase>> {
    let mut cases = Vec::new();
    let project = "/tmp/ironmem-eval-levers";

    // A detail that lives only in the skim layer: the memory summary misses
    // the query vocabulary, its chunk carries it. Open-domain fusion must
    // surface the parent memory anyway.
    let parent = store(db, project, "Weekly planning notes were archived").await?;
    db::replace_memory_chunks(
        db,
        parent,
        &[NewMemoryChunk {
            chunk_id: format!("eval-lever-chunk-{parent}"),
            project: project.to_string(),
            memory_id: parent,
            session_id: "eval".to_string(),
            ordinal: 0,
            density: "skim".to_string(),
            kind: "fact".to_string(),
            title: "forecast".to_string(),
            summary: "Reviewed the quarterly forecast spreadsheet totals".to_string(),
            source_hash: None,
            source_start: None,
            source_end: None,
            token_estimate: 8,
        }],
    )
    .await?;
    let hits = search(db, project, "quarterly forecast spreadsheet", 3).await?;
    cases.push(EvalCase::new(
        "lever_chunk_recall_surfaces_parent",
        hits.contains(&parent),
        format!("expected chunk parent {parent} in top-3, got {hits:?}"),
    ));

    // Maturity: explicit set clamps to the known tiers.
    let m = store(db, project, "Maturity clamp subject memory").await?;
    db::upsert_memory_meta(db, m, 0.6).await?;
    db::set_memory_maturity(db, m, "CORE").await?;
    let meta = db::activation_meta_for(db, &[m]).await?;
    let clamped_core = meta.get(&m).and_then(|a| a.maturity.clone()) == Some("core".to_string());
    db::set_memory_maturity(db, m, "not-a-tier").await?;
    let meta = db::activation_meta_for(db, &[m]).await?;
    let clamped_draft = meta.get(&m).and_then(|a| a.maturity.clone()) == Some("draft".to_string());
    cases.push(EvalCase::new(
        "lever_maturity_set_clamps_tiers",
        clamped_core && clamped_draft,
        format!("clamped_core={clamped_core} clamped_draft={clamped_draft}"),
    ));

    // Promotion flow: 3 injections graduate draft -> stable; net feedback >= 2
    // graduates stable -> core. Idempotent across repeat sweeps.
    let promoted = store(db, project, "Promotion subject memory").await?;
    db::upsert_memory_meta(db, promoted, 0.6).await?;
    if let Some(memory) = db::get_memory_by_id_any_namespace(db, promoted).await? {
        for _ in 0..3 {
            db::record_injection_events(
                db,
                project,
                None,
                Some("eval"),
                std::slice::from_ref(&memory),
            )
            .await?;
        }
    }
    db::promote_memories_maturity(db).await?;
    let meta = db::activation_meta_for(db, &[promoted]).await?;
    let stable = meta.get(&promoted).and_then(|a| a.maturity.clone()) == Some("stable".to_string());
    db::record_memory_feedback(db, promoted, project, "useful", 2.0, None).await?;
    db::promote_memories_maturity(db).await?;
    let meta = db::activation_meta_for(db, &[promoted]).await?;
    let core = meta.get(&promoted).and_then(|a| a.maturity.clone()) == Some("core".to_string());
    cases.push(EvalCase::new(
        "lever_maturity_promotion_flow",
        stable && core,
        format!("after_injections_stable={stable} after_feedback_core={core}"),
    ));

    // The maturity multiplier feeding activation scoring is monotone.
    let monotone = db::maturity_multiplier(Some("core")) > db::maturity_multiplier(Some("stable"))
        && db::maturity_multiplier(Some("stable")) > db::maturity_multiplier(None);
    cases.push(EvalCase::new(
        "lever_maturity_multiplier_monotone",
        monotone,
        format!(
            "core={} stable={} draft={}",
            db::maturity_multiplier(Some("core")),
            db::maturity_multiplier(Some("stable")),
            db::maturity_multiplier(None)
        ),
    ));

    Ok(cases)
}

// --- compliance cluster (Phase 3) ---------------------------------------------
// The compliance product's guarantees must hold deterministically: chain
// verification passes on honest history, fails on tampered history, and
// lineage answers "who wrote this and where did it act" for any memory.

async fn eval_compliance_cluster(db: &Database) -> Result<Vec<EvalCase>> {
    let mut cases = Vec::new();
    let project = "/tmp/ironmem-eval-compliance";
    let namespace = "evalcompliance";

    let governance = || MemoryGovernance {
        namespace: namespace.to_string(),
        ..MemoryGovernance::explicit()
    };
    let first = compress::remember_with_governance(
        db,
        None,
        &BruteForceStore,
        project,
        "project",
        "fact",
        "Compliance chain fact alpha",
        Some("eval"),
        governance(),
    )
    .await?;
    compress::remember_with_governance(
        db,
        None,
        &BruteForceStore,
        project,
        "project",
        "fact",
        "Compliance chain fact beta",
        Some("eval"),
        governance(),
    )
    .await?;

    let verification = crate::compliance::verify_ledger_chain(db, namespace).await?;
    cases.push(EvalCase::new(
        "compliance_chain_verifies_honest_history",
        verification.valid && verification.entries >= 2,
        format!(
            "valid={} entries={}",
            verification.valid, verification.entries
        ),
    ));

    // Tamper with history behind the ledger's back: verification must catch it
    // and name the first broken entry.
    sqlx::query("UPDATE memory_ledger SET payload = 'tampered' WHERE memory_id = $1")
        .bind(first)
        .execute(&db.pool)
        .await?;
    let verification = crate::compliance::verify_ledger_chain(db, namespace).await?;
    cases.push(EvalCase::new(
        "compliance_chain_detects_tampering",
        !verification.valid && verification.first_broken_id.is_some(),
        format!(
            "valid={} first_broken_id={:?}",
            verification.valid, verification.first_broken_id
        ),
    ));

    // Lineage: writer attribution + ledger trail + injection (action) records.
    let acted = compress::remember(
        db,
        None,
        &BruteForceStore,
        project,
        "project",
        "fact",
        "Compliance lineage subject",
        Some("eval"),
    )
    .await?;
    if let Some(memory) = db::get_memory_by_id_any_namespace(db, acted).await? {
        db::record_injection_events(
            db,
            project,
            Some("eval-session"),
            Some("lineage query"),
            std::slice::from_ref(&memory),
        )
        .await?;
    }
    let lineage = crate::compliance::memory_lineage(db, acted).await?;
    cases.push(EvalCase::new(
        "compliance_lineage_traces_write_and_action",
        lineage.writer_identity.as_deref() == Some("ironmem:remember")
            && lineage.ledger.iter().any(|e| e.op_type == "remember")
            && lineage
                .injections
                .iter()
                .any(|i| i.session_id.as_deref() == Some("eval-session")),
        format!(
            "writer={:?} ledger_ops={:?} injections={}",
            lineage.writer_identity,
            lineage
                .ledger
                .iter()
                .map(|e| &e.op_type)
                .collect::<Vec<_>>(),
            lineage.injections.len()
        ),
    ));

    // Full report: chains + inventory + Art. 12 section render end-to-end.
    let report = crate::compliance::generate(db).await?;
    let markdown = report.to_markdown();
    cases.push(EvalCase::new(
        "compliance_report_generates",
        !report.chains.is_empty()
            && !report.inventory.is_empty()
            && markdown.contains("Art. 12")
            && markdown.contains("CHAIN VERIFICATION FAILED"),
        format!(
            "chains={} inventory_rows={} (tampered namespace must surface as failed)",
            report.chains.len(),
            report.inventory.len()
        ),
    ));

    Ok(cases)
}

fn git_commit() -> String {
    Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

pub fn ensure_passed(report: &EvalReport) -> Result<()> {
    if report.failed() == 0 {
        return Ok(());
    }
    let failures: Vec<String> = report
        .cases
        .iter()
        .filter(|c| !c.passed)
        .map(|c| format!("{}: {}", c.name, c.detail))
        .collect();
    bail!(
        "{} eval case(s) failed:\n{}",
        report.failed(),
        failures.join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // CI regression gate: the full deterministic eval suite must pass on every
    // change. `cargo test eval_suite_gate` is the local equivalent of
    // `ironmem eval`.
    #[tokio::test]
    async fn eval_suite_gate() -> Result<()> {
        let out_dir =
            std::env::temp_dir().join(format!("ironmem-eval-gate-{}", uuid::Uuid::new_v4()));
        let report = run(&Config::default(), &out_dir).await?;
        let result = ensure_passed(&report);
        let _ = std::fs::remove_dir_all(out_dir);
        result
    }
}
