use anyhow::{bail, Result};
use chrono::Utc;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::{Config, Weights};
use crate::db::{self, Database, NewMemoryEdge};
use crate::retrieval;
use crate::vectorstore::BruteForceStore;

#[derive(Debug, Clone)]
pub struct EvalCase {
    pub name: String,
    pub passed: bool,
    pub detail: String,
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
    cases.push(eval_graph_relation_recall(&db).await?);
    cases.push(eval_temporal_update_recall(&db).await?);
    cases.push(eval_procedural_ranking(&db).await?);

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

    let hits = retrieval::hybrid_search(
        db,
        None,
        &BruteForceStore,
        Some(project),
        "What is Caroline assigned to?",
        1,
    )
    .await?;
    let passed = hits.first().map(|m| m.id) == Some(memory_id);
    Ok(EvalCase {
        name: "graph_relation_recall".to_string(),
        passed,
        detail: format!(
            "expected memory {memory_id}, got {:?}",
            hits.first().map(|m| m.id)
        ),
    })
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
    Ok(EvalCase {
        name: "temporal_update_recall".to_string(),
        passed,
        detail: format!(
            "current={:?}; past={:?}",
            current.iter().map(|e| &e.target).collect::<Vec<_>>(),
            past.iter().map(|e| &e.target).collect::<Vec<_>>()
        ),
    })
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
    Ok(EvalCase {
        name: "procedural_ranking".to_string(),
        passed,
        detail: format!(
            "expected procedural {proc_id}, got {:?}",
            ranked.first().map(|m| m.id)
        ),
    })
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
    bail!("{} eval case(s) failed", report.failed())
}
