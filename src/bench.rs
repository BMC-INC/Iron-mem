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
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct CheckpointIdentity {
    schema: u32,
    dataset_sha256: String,
    commit: String,
    answer_model: String,
    judge_model: String,
    embedder: String,
    retrieve_k: usize,
    limit: Option<usize>,
    full_context: bool,
    context_chars: usize,
    dry_run: bool,
}

fn checkpoint_file(checkpoint_dir: &Path, question_id: &str) -> PathBuf {
    let digest = Sha256::digest(question_id.as_bytes());
    checkpoint_dir.join(format!("{digest:x}.json"))
}

/// Persist one paid question result independently. The temporary file and
/// rename keep readers from observing a partial JSON document after a crash.
fn persist_checkpoint_result(checkpoint_dir: &Path, result: &QuestionResult) -> Result<()> {
    std::fs::create_dir_all(checkpoint_dir).with_context(|| {
        format!(
            "creating LongMemEval checkpoint directory {}",
            checkpoint_dir.display()
        )
    })?;
    let destination = checkpoint_file(checkpoint_dir, &result.question_id);
    let temporary = checkpoint_dir.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let bytes = serde_json::to_vec(result)?;
    let mut file = std::fs::File::create(&temporary)
        .with_context(|| format!("creating checkpoint {}", temporary.display()))?;
    file.write_all(&bytes)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    std::fs::rename(&temporary, &destination)
        .with_context(|| format!("committing question checkpoint {}", destination.display()))?;
    // Directory syncing is supported on Unix and makes the rename durable.
    // Other platforms still retain the atomic rename guarantee.
    if let Ok(directory) = std::fs::File::open(checkpoint_dir) {
        let _ = directory.sync_all();
    }
    Ok(())
}

fn load_checkpoint_results(checkpoint_dir: &Path) -> Result<Vec<QuestionResult>> {
    if !checkpoint_dir.exists() {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(checkpoint_dir)
        .with_context(|| format!("reading checkpoint directory {}", checkpoint_dir.display()))?
    {
        let path = entry?.path();
        if path.file_name().and_then(|value| value.to_str()) != Some("manifest.json")
            && path.extension().and_then(|value| value.to_str()) == Some("json")
        {
            paths.push(path);
        }
    }
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let bytes = std::fs::read(&path)
                .with_context(|| format!("reading checkpoint {}", path.display()))?;
            serde_json::from_slice(&bytes)
                .with_context(|| format!("parsing checkpoint {}", path.display()))
        })
        .collect()
}

fn prepare_checkpoint_dir(checkpoint_dir: &Path, identity: &CheckpointIdentity) -> Result<()> {
    std::fs::create_dir_all(checkpoint_dir)?;
    let manifest = checkpoint_dir.join("manifest.json");
    if manifest.exists() {
        let existing: CheckpointIdentity = serde_json::from_slice(&std::fs::read(&manifest)?)
            .with_context(|| format!("parsing checkpoint manifest {}", manifest.display()))?;
        if existing != *identity {
            bail!(
                "checkpoint identity mismatch in {}; use the original benchmark configuration or a new --out directory",
                manifest.display()
            );
        }
        return Ok(());
    }
    let temporary = checkpoint_dir.join(format!(".manifest-{}.tmp", uuid::Uuid::new_v4()));
    let mut file = std::fs::File::create(&temporary)?;
    file.write_all(&serde_json::to_vec_pretty(identity)?)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    std::fs::rename(&temporary, &manifest)?;
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("hashing benchmark dataset {}", path.display()))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
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

    let checkpoint_dir = opts.out_dir.join("longmemeval-checkpoint");
    let checkpoint_identity = CheckpointIdentity {
        schema: 1,
        dataset_sha256: sha256_file(&opts.data)?,
        commit: git_commit(),
        answer_model: answer_model.clone(),
        judge_model: judge_model.clone(),
        embedder: embedder_desc.clone(),
        retrieve_k: opts.retrieve_k,
        limit: opts.limit,
        full_context: opts.full_context,
        context_chars: opts.context_chars,
        dry_run: opts.dry_run,
    };
    prepare_checkpoint_dir(&checkpoint_dir, &checkpoint_identity)?;
    let resumed: HashMap<String, QuestionResult> = load_checkpoint_results(&checkpoint_dir)?
        .into_iter()
        .map(|result| (result.question_id.clone(), result))
        .collect();
    if !resumed.is_empty() {
        tracing::info!(
            "resuming LongMemEval from {} durable question checkpoints in {}",
            resumed.len(),
            checkpoint_dir.display()
        );
    }
    tracing::info!(
        "LongMemEval lifecycle pid={} parent_pid={} checkpoint_dir={}",
        std::process::id(),
        parent_process_id(),
        checkpoint_dir.display()
    );

    let mut results: Vec<QuestionResult> = Vec::with_capacity(questions.len());
    for (idx, question) in questions.iter().enumerate() {
        if let Some(result) = resumed.get(&question.question_id) {
            tracing::info!(
                "bench {}/{} {} [{}] -> resumed",
                idx + 1,
                questions.len(),
                result.question_id,
                result.ability
            );
            results.push(result.clone());
            continue;
        }
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
        let result = match result {
            Ok(r) => r,
            Err(e) => {
                // A failed question is scored as incorrect rather than
                // aborting a long run; the error is preserved in the record.
                tracing::warn!("bench question {} failed: {e:#}", question.question_id);
                QuestionResult {
                    question_id: question.question_id.clone(),
                    ability: question.ability().to_string(),
                    question: question.question.clone(),
                    gold: question.gold_answer(),
                    hypothesis: format!("[harness error: {e:#}]"),
                    correct: false,
                    retrieved: 0,
                    answer_ms: 0,
                }
            }
        };
        persist_checkpoint_result(&checkpoint_dir, &result)?;
        tracing::info!(
            "bench {}/{} {} [{}] -> {} (checkpointed)",
            idx + 1,
            questions.len(),
            result.question_id,
            result.ability,
            if result.correct {
                "correct"
            } else {
                "incorrect"
            }
        );
        results.push(result);
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
///
/// Turns embed one at a time on purpose. Measured on the 5-question canary
/// (2026-07-16, CPU ONNX bge-small): serial 32s/q, naive batch-32 79s/q,
/// length-sorted batch-32 35s/q — ONNX pads each batch to its longest text,
/// so batching never beat serial here. Embedding failures are fatal rather
/// than silently skipped: a partially-embedded store degrades retrieval
/// scores without any visible error.
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
                let mut vecs = emb
                    .embed(&[text])
                    .await
                    .with_context(|| format!("embedding ingested turn (memory {memory_id})"))?;
                let Some(vec) = vecs.pop() else {
                    bail!("embedder returned no vector for ingested turn (memory {memory_id})");
                };
                let _ = store.upsert(db, memory_id, emb.id(), emb.dim(), &vec).await;
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

fn parent_process_id() -> u32 {
    std::process::Command::new("ps")
        .args(["-o", "ppid=", "-p", &std::process::id().to_string()])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(0)
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

    #[test]
    fn checkpoint_round_trip_survives_an_interrupted_run() {
        let root =
            std::env::temp_dir().join(format!("ironmem-checkpoint-test-{}", uuid::Uuid::new_v4()));
        let result = QuestionResult {
            question_id: "q1".to_string(),
            ability: "information-extraction".to_string(),
            question: "What instrument?".to_string(),
            gold: "cello".to_string(),
            hypothesis: "cello".to_string(),
            correct: true,
            retrieved: 3,
            answer_ms: 42,
        };

        persist_checkpoint_result(&root, &result).unwrap();
        let resumed = load_checkpoint_results(&root).unwrap();

        assert_eq!(resumed.len(), 1);
        assert_eq!(resumed[0].question_id, "q1");
        assert!(resumed[0].correct);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rewriting_a_question_checkpoint_is_idempotent() {
        let root =
            std::env::temp_dir().join(format!("ironmem-checkpoint-test-{}", uuid::Uuid::new_v4()));
        let result = QuestionResult {
            question_id: "q1".to_string(),
            ability: "abstention".to_string(),
            question: "Unknown?".to_string(),
            gold: "unanswerable".to_string(),
            hypothesis: "I don't know".to_string(),
            correct: true,
            retrieved: 0,
            answer_ms: 7,
        };

        persist_checkpoint_result(&root, &result).unwrap();
        persist_checkpoint_result(&root, &result).unwrap();
        let resumed = load_checkpoint_results(&root).unwrap();

        assert_eq!(resumed.len(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn checkpoint_rejects_a_different_run_identity() {
        let root =
            std::env::temp_dir().join(format!("ironmem-checkpoint-test-{}", uuid::Uuid::new_v4()));
        let first = CheckpointIdentity {
            schema: 1,
            dataset_sha256: "dataset-a".to_string(),
            commit: "abc1234".to_string(),
            answer_model: "answerer".to_string(),
            judge_model: "judge".to_string(),
            embedder: "embedder".to_string(),
            retrieve_k: 10,
            limit: Some(5),
            full_context: false,
            context_chars: 400_000,
            dry_run: false,
        };
        let mut changed = first.clone();
        changed.judge_model = "different-judge".to_string();

        prepare_checkpoint_dir(&root, &first).unwrap();
        let error = prepare_checkpoint_dir(&root, &changed).unwrap_err();

        assert!(error.to_string().contains("checkpoint identity mismatch"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
