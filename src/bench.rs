//! LongMemEval benchmark harness (`ironmem bench longmemeval`).
//!
//! Runs the LongMemEval question set (Wu et al., ICLR 2025) against IronMem's
//! real write + retrieval paths and grades answers with an LLM judge, emitting
//! per-ability accuracy so retrieval work can target the five scored
//! abilities: information extraction, multi-session reasoning, temporal
//! reasoning, knowledge updates, and abstention.
//!
//! Methodology notes (the "show your work" rules the roadmap commits to):
//! - The dataset file is the official LongMemEval JSON (e.g.
//!   `longmemeval_s.json`); nothing is vendored, pass `--data`.
//! - `--full-context` skips memory entirely and stuffs the (truncated)
//!   haystack into the answer prompt: that is the baseline column every
//!   IronMem score must be published next to.
//! - Answer and judge models are recorded in the report so runs are
//!   comparable; vendor-style "our pipeline, unstated judge" numbers are not.

use anyhow::{bail, Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::db::{self, Database};
use crate::embedder::Embedder;
use crate::provider;
use crate::retrieval;
use crate::vectorstore::{BruteForceStore, VectorStore};

const ANSWER_ABSTAIN_MARKER: &str = "I don't know";

#[derive(Debug, Clone, Deserialize)]
pub struct LmeTurn {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LmeQuestion {
    pub question_id: String,
    pub question_type: String,
    pub question: String,
    pub answer: serde_json::Value,
    #[serde(default)]
    pub question_date: Option<String>,
    #[serde(default)]
    pub haystack_dates: Vec<String>,
    pub haystack_sessions: Vec<Vec<LmeTurn>>,
}

impl LmeQuestion {
    /// Map a question onto the LongMemEval ability it scores. Abstention is
    /// marked by the `_abs` question-id suffix, not a question type.
    pub fn ability(&self) -> &'static str {
        if self.question_id.ends_with("_abs") {
            return "abstention";
        }
        match self.question_type.as_str() {
            "temporal-reasoning" => "temporal-reasoning",
            "knowledge-update" => "knowledge-update",
            "multi-session" => "multi-session",
            "single-session-preference" => "preference",
            _ => "information-extraction",
        }
    }

    pub fn gold_answer(&self) -> String {
        match &self.answer {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct BenchOptions {
    pub data: PathBuf,
    pub out_dir: PathBuf,
    pub limit: Option<usize>,
    pub retrieve_k: usize,
    pub answer_model: Option<String>,
    pub judge_model: Option<String>,
    pub full_context: bool,
    pub context_chars: usize,
    /// Ingest + retrieve but skip the answer/judge LLM calls. Scores nothing;
    /// verifies the pipeline and reports retrieval counts (usable in CI
    /// without API keys).
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct QuestionResult {
    pub question_id: String,
    pub ability: String,
    pub question: String,
    pub gold: String,
    pub hypothesis: String,
    pub correct: bool,
    pub retrieved: usize,
    pub answer_ms: u128,
}

#[derive(Debug, Clone, Serialize)]
pub struct AbilityScore {
    pub total: usize,
    pub correct: usize,
}

impl AbilityScore {
    pub fn accuracy(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.correct as f64 / self.total as f64
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct BenchReport {
    pub suite: String,
    pub generated_at: String,
    pub commit: String,
    pub dataset: String,
    pub mode: String,
    pub answer_model: String,
    pub judge_model: String,
    pub embedder: String,
    pub retrieve_k: usize,
    pub total: usize,
    pub correct: usize,
    pub per_ability: BTreeMap<String, AbilityScore>,
    pub results: Vec<QuestionResult>,
}

impl BenchReport {
    pub fn accuracy(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.correct as f64 / self.total as f64
        }
    }

    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("# IronMem LongMemEval Report\n\n");
        out.push_str(&format!("- generated_at: `{}`\n", self.generated_at));
        out.push_str(&format!("- commit: `{}`\n", self.commit));
        out.push_str(&format!("- dataset: `{}`\n", self.dataset));
        out.push_str(&format!("- mode: `{}`\n", self.mode));
        out.push_str(&format!("- answer_model: `{}`\n", self.answer_model));
        out.push_str(&format!("- judge_model: `{}`\n", self.judge_model));
        out.push_str(&format!("- embedder: `{}`\n", self.embedder));
        out.push_str(&format!("- retrieve_k: `{}`\n", self.retrieve_k));
        out.push_str(&format!(
            "- overall: `{}/{} = {:.1}%`\n\n",
            self.correct,
            self.total,
            self.accuracy() * 100.0
        ));
        out.push_str("| Ability | Correct | Total | Accuracy |\n");
        out.push_str("| --- | --- | --- | --- |\n");
        for (ability, score) in &self.per_ability {
            out.push_str(&format!(
                "| {} | {} | {} | {:.1}% |\n",
                ability,
                score.correct,
                score.total,
                score.accuracy() * 100.0
            ));
        }
        out
    }
}

pub fn load_dataset(path: &Path) -> Result<Vec<LmeQuestion>> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading LongMemEval dataset {}", path.display()))?;
    let questions: Vec<LmeQuestion> =
        serde_json::from_str(&raw).context("parsing LongMemEval dataset JSON")?;
    if questions.is_empty() {
        bail!("dataset {} contains no questions", path.display());
    }
    Ok(questions)
}

pub async fn run(cfg: &Config, opts: &BenchOptions) -> Result<BenchReport> {
    let questions = load_dataset(&opts.data)?;
    let take = opts.limit.unwrap_or(questions.len());
    let questions: Vec<LmeQuestion> = questions.into_iter().take(take).collect();

    let answer_model = opts
        .answer_model
        .clone()
        .unwrap_or_else(|| cfg.model.clone());
    let judge_model = opts
        .judge_model
        .clone()
        .unwrap_or_else(|| cfg.model.clone());

    let temp_db = std::env::temp_dir().join(format!("ironmem-bench-{}.db", uuid::Uuid::new_v4()));
    let database = Database::new(&temp_db.to_string_lossy()).await?;
    database.migrate().await?;

    let embedder = crate::embedder::resolve_embedder(cfg).await;
    let store: std::sync::Arc<dyn VectorStore> = match &embedder {
        Some(e) => crate::vectorstore::make_vector_store(&database, e.dim()).await,
        None => std::sync::Arc::new(BruteForceStore),
    };
    let embedder_desc = embedder
        .as_ref()
        .map(|e| e.id().to_string())
        .unwrap_or_else(|| "none (FTS + graph only)".to_string());

    let mut mode = if opts.full_context {
        format!("full-context (first {} chars)", opts.context_chars)
    } else {
        "memory (direct turn-level ingest + hybrid retrieval)".to_string()
    };
    if opts.dry_run {
        mode.push_str(" [dry-run: unscored]");
    }

    let mut results: Vec<QuestionResult> = Vec::with_capacity(questions.len());
    for (idx, question) in questions.iter().enumerate() {
        let result = run_question(
            cfg,
            &database,
            embedder.as_deref(),
            store.as_ref(),
            question,
            opts,
            &answer_model,
            &judge_model,
        )
        .await;
        match result {
            Ok(r) => {
                tracing::info!(
                    "bench {}/{} {} [{}] -> {}",
                    idx + 1,
                    questions.len(),
                    r.question_id,
                    r.ability,
                    if r.correct { "correct" } else { "incorrect" }
                );
                results.push(r);
            }
            Err(e) => {
                // A failed question is scored as incorrect rather than
                // aborting a long run; the error is preserved in the record.
                tracing::warn!("bench question {} failed: {e:#}", question.question_id);
                results.push(QuestionResult {
                    question_id: question.question_id.clone(),
                    ability: question.ability().to_string(),
                    question: question.question.clone(),
                    gold: question.gold_answer(),
                    hypothesis: format!("[harness error: {e:#}]"),
                    correct: false,
                    retrieved: 0,
                    answer_ms: 0,
                });
            }
        }
    }

    let mut per_ability: BTreeMap<String, AbilityScore> = BTreeMap::new();
    for r in &results {
        let entry = per_ability
            .entry(r.ability.clone())
            .or_insert(AbilityScore {
                total: 0,
                correct: 0,
            });
        entry.total += 1;
        if r.correct {
            entry.correct += 1;
        }
    }
    let correct = results.iter().filter(|r| r.correct).count();

    let report = BenchReport {
        suite: "longmemeval".to_string(),
        generated_at: Utc::now().to_rfc3339(),
        commit: git_commit(),
        dataset: opts.data.display().to_string(),
        mode,
        answer_model,
        judge_model,
        embedder: embedder_desc,
        retrieve_k: opts.retrieve_k,
        total: results.len(),
        correct,
        per_ability,
        results,
    };

    std::fs::create_dir_all(&opts.out_dir)?;
    let stamp = report.generated_at.replace([':', '.'], "-");
    let md_path = opts.out_dir.join(format!("{stamp}-longmemeval.md"));
    std::fs::write(&md_path, report.to_markdown())?;
    let jsonl_path = opts.out_dir.join(format!("{stamp}-longmemeval.jsonl"));
    let mut jsonl = String::new();
    for r in &report.results {
        jsonl.push_str(&serde_json::to_string(r)?);
        jsonl.push('\n');
    }
    std::fs::write(&jsonl_path, jsonl)?;
    tracing::info!(
        "LongMemEval report written to {} (per-question log: {})",
        md_path.display(),
        jsonl_path.display()
    );

    let _ = std::fs::remove_file(&temp_db);
    Ok(report)
}

#[allow(clippy::too_many_arguments)]
async fn run_question(
    cfg: &Config,
    db: &Database,
    embedder: Option<&dyn Embedder>,
    store: &dyn VectorStore,
    question: &LmeQuestion,
    opts: &BenchOptions,
    answer_model: &str,
    judge_model: &str,
) -> Result<QuestionResult> {
    let started = std::time::Instant::now();
    let (context, retrieved) = if opts.full_context {
        (full_context(question, opts.context_chars), 0)
    } else {
        // Fresh project per question keeps haystacks isolated inside the
        // shared bench database.
        let project = format!("/bench/longmemeval/{}", question.question_id);
        ingest_direct(db, embedder, store, &project, question).await?;
        let hits = retrieval::hybrid_search(
            db,
            embedder,
            store,
            Some(&project),
            &question.question,
            opts.retrieve_k,
        )
        .await?;
        let retrieved = hits.len();
        let mut context = String::new();
        for memory in &hits {
            context.push_str(&memory.summary);
            context.push('\n');
        }
        (context, retrieved)
    };

    let (hypothesis, correct) = if opts.dry_run {
        ("[dry-run: no LLM call]".to_string(), false)
    } else {
        let hypothesis = answer_question(cfg, question, &context, answer_model).await?;
        let correct = judge_answer(cfg, question, &hypothesis, judge_model).await?;
        (hypothesis, correct)
    };
    let answer_ms = started.elapsed().as_millis();

    Ok(QuestionResult {
        question_id: question.question_id.clone(),
        ability: question.ability().to_string(),
        question: question.question.clone(),
        gold: question.gold_answer(),
        hypothesis,
        correct,
        retrieved,
        answer_ms,
    })
}

/// Turn-level ingestion: each turn becomes one memory stamped with its
/// session's date. This exercises the real write path (FTS, meta, event time)
/// without LLM extraction cost; LLM-extraction ingest arrives with the Phase 2
/// observer and will be a second `--ingest` mode.
async fn ingest_direct(
    db: &Database,
    embedder: Option<&dyn Embedder>,
    store: &dyn VectorStore,
    project: &str,
    question: &LmeQuestion,
) -> Result<()> {
    for (i, session) in question.haystack_sessions.iter().enumerate() {
        let date = question.haystack_dates.get(i).cloned().unwrap_or_default();
        let session_id = db::create_session(db, project).await?;
        for turn in session {
            let text = if date.is_empty() {
                format!("{}: {}", turn.role, turn.content)
            } else {
                format!("[{}] {}: {}", date, turn.role, turn.content)
            };
            let memory_id = db::insert_memory(db, project, &session_id, &text, None).await?;
            if let Some(event_date) = date.split_whitespace().next() {
                if !event_date.is_empty() {
                    let _ = db::set_memory_event_time(db, memory_id, event_date).await;
                }
            }
            if let Some(emb) = embedder {
                if let Ok(mut vecs) = emb.embed(&[text]).await {
                    if let Some(vec) = vecs.drain(..).next() {
                        let _ = store.upsert(db, memory_id, emb.id(), emb.dim(), &vec).await;
                    }
                }
            }
        }
    }
    Ok(())
}

fn full_context(question: &LmeQuestion, max_chars: usize) -> String {
    let mut out = String::new();
    for (i, session) in question.haystack_sessions.iter().enumerate() {
        let date = question.haystack_dates.get(i).cloned().unwrap_or_default();
        out.push_str(&format!("--- session {} ({date}) ---\n", i + 1));
        for turn in session {
            out.push_str(&format!("{}: {}\n", turn.role, turn.content));
        }
        if out.len() >= max_chars {
            break;
        }
    }
    if out.len() > max_chars {
        out.truncate(max_chars);
    }
    out
}

async fn answer_question(
    cfg: &Config,
    question: &LmeQuestion,
    context: &str,
    answer_model: &str,
) -> Result<String> {
    let today = question
        .question_date
        .clone()
        .unwrap_or_else(|| "unknown".to_string());
    let prompt = format!(
        "You are answering a question about a user's past chat sessions using retrieved memory.\n\
         Today's date: {today}.\n\n\
         Retrieved memory context:\n{context}\n\n\
         Question: {question}\n\n\
         Answer concisely using only the context above. If the context does not \
         contain the information needed, reply exactly: \"{ANSWER_ABSTAIN_MARKER}\".",
        question = question.question,
    );
    provider::complete_with(&prompt, answer_model, cfg).await
}

async fn judge_answer(
    cfg: &Config,
    question: &LmeQuestion,
    hypothesis: &str,
    judge_model: &str,
) -> Result<bool> {
    let gold = question.gold_answer();
    let prompt = if question.ability() == "abstention" {
        format!(
            "A memory assistant was asked a question whose answer is NOT present in its \
             history; the correct behaviour is to say it does not know.\n\
             Question: {q}\n\
             Assistant response: {hypothesis}\n\n\
             Did the assistant correctly abstain (decline to answer / say it does not \
             know) instead of inventing an answer? Reply with exactly CORRECT or INCORRECT.",
            q = question.question,
        )
    } else {
        format!(
            "Grade a memory assistant's answer strictly against the gold answer.\n\
             Question: {q}\n\
             Gold answer: {gold}\n\
             Assistant answer: {hypothesis}\n\n\
             The assistant answer is correct only if it contains the same specific \
             information as the gold answer (same entity, date, quantity, or decision); \
             a vague or topically-adjacent answer is INCORRECT. Reply with exactly \
             CORRECT or INCORRECT.",
            q = question.question,
        )
    };
    let verdict = provider::complete_with(&prompt, judge_model, cfg).await?;
    let verdict = verdict.trim().to_uppercase();
    Ok(verdict.starts_with("CORRECT"))
}

fn git_commit() -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"[
        {
            "question_id": "q1",
            "question_type": "single-session-user",
            "question": "What instrument does the user play?",
            "answer": "the cello",
            "question_date": "2023/05/20 (Sat) 02:21",
            "haystack_dates": ["2023/05/01 (Mon) 10:00"],
            "haystack_sessions": [[
                {"role": "user", "content": "I started cello lessons last week."},
                {"role": "assistant", "content": "That is wonderful."}
            ]]
        },
        {
            "question_id": "q2_abs",
            "question_type": "single-session-user",
            "question": "What car does the user drive?",
            "answer": "unanswerable",
            "haystack_dates": [],
            "haystack_sessions": [[]]
        },
        {
            "question_id": "q3",
            "question_type": "knowledge-update",
            "question": "Where does the user live now?",
            "answer": "Lisbon",
            "haystack_dates": [],
            "haystack_sessions": [[]]
        }
    ]"#;

    #[test]
    fn parses_dataset_and_maps_abilities() {
        let questions: Vec<LmeQuestion> = serde_json::from_str(SAMPLE).unwrap();
        assert_eq!(questions.len(), 3);
        assert_eq!(questions[0].ability(), "information-extraction");
        assert_eq!(questions[1].ability(), "abstention");
        assert_eq!(questions[2].ability(), "knowledge-update");
        assert_eq!(questions[0].gold_answer(), "the cello");
        assert_eq!(questions[0].haystack_sessions[0].len(), 2);
    }

    #[test]
    fn report_markdown_includes_per_ability_breakdown() {
        let mut per_ability = BTreeMap::new();
        per_ability.insert(
            "abstention".to_string(),
            AbilityScore {
                total: 4,
                correct: 3,
            },
        );
        per_ability.insert(
            "temporal-reasoning".to_string(),
            AbilityScore {
                total: 10,
                correct: 7,
            },
        );
        let report = BenchReport {
            suite: "longmemeval".to_string(),
            generated_at: "2026-07-12T00:00:00Z".to_string(),
            commit: "abc1234".to_string(),
            dataset: "longmemeval_s.json".to_string(),
            mode: "memory".to_string(),
            answer_model: "test-answerer".to_string(),
            judge_model: "test-judge".to_string(),
            embedder: "none".to_string(),
            retrieve_k: 10,
            total: 14,
            correct: 10,
            per_ability,
            results: Vec::new(),
        };
        let md = report.to_markdown();
        assert!(md.contains("| abstention | 3 | 4 | 75.0% |"));
        assert!(md.contains("| temporal-reasoning | 7 | 10 | 70.0% |"));
        assert!(md.contains("71.4%"), "overall accuracy line: {md}");
        assert!(md.contains("test-judge"));
    }

    #[test]
    fn full_context_truncates_and_labels_sessions() {
        let questions: Vec<LmeQuestion> = serde_json::from_str(SAMPLE).unwrap();
        let ctx = full_context(&questions[0], 10_000);
        assert!(ctx.contains("session 1"));
        assert!(ctx.contains("cello lessons"));
        let tiny = full_context(&questions[0], 20);
        assert!(tiny.len() <= 20);
    }
}
