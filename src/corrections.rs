//! Correction miner — the optional feedback loop. Scans a session's tool
//! observations for an error→fix signal (a command that fails, then the *same*
//! command passing after intervening edits) and distills each into a
//! `kind=error_solution` memory. These are project-scoped and ride the kind
//! boost in injection ranking, so a past fix resurfaces when the same work
//! comes around again. Local-only and reversible (just memories).

use anyhow::Result;

use crate::db::{self, Database, Observation};
use crate::embedder::Embedder;
use crate::vectorstore::VectorStore;

/// The command tool whose failures/successes we track.
const COMMAND_TOOL: &str = "Bash";
/// Tools whose observations represent a code change (the "fix").
const EDIT_TOOLS: &[&str] = &["Edit", "Write", "MultiEdit", "NotebookEdit"];
/// Substrings (lowercased) that strongly indicate a failed command. Chosen to
/// be specific enough not to fire on success output (e.g. cargo's
/// "test result: ok. 0 failed" must NOT match).
const FAIL_MARKERS: &[&str] = &[
    "error:",
    "error[",
    "fatal:",
    "panicked at",
    "traceback (most recent call last)",
    "command not found",
    "no such file or directory",
    "could not compile",
    "build failed",
    "test result: failed",
    "exit code 1",
    "exit status 1",
    "segmentation fault",
    "assertion failed",
    "cannot find",
];
/// Cap mined corrections per session to avoid flooding memory with noise.
const MAX_CORRECTIONS_PER_SESSION: usize = 5;

/// A mined error→fix pair.
#[derive(Debug, Clone, PartialEq)]
pub struct Correction {
    pub problem: String,
    pub solution: String,
}

fn is_command(o: &Observation) -> bool {
    o.tool == COMMAND_TOOL
}

fn is_edit(o: &Observation) -> bool {
    EDIT_TOOLS.contains(&o.tool.as_str())
}

fn cmd_input(o: &Observation) -> &str {
    o.input.as_deref().unwrap_or("").trim()
}

/// Leading command token (`cargo` from `cargo test --bin ...`), used to match a
/// failing command against its later passing re-run.
fn head(o: &Observation) -> Option<&str> {
    cmd_input(o).split_whitespace().next()
}

fn is_fail_line(line: &str) -> bool {
    let s = line.to_ascii_lowercase();
    FAIL_MARKERS.iter().any(|m| s.contains(m))
}

fn output_failed(o: &Observation) -> bool {
    o.output
        .as_deref()
        .map(|out| out.lines().any(is_fail_line))
        .unwrap_or(false)
}

/// The most informative line of a failing command's output.
fn error_snippet(o: &Observation) -> String {
    let out = o.output.as_deref().unwrap_or("");
    let line = out
        .lines()
        .find(|l| is_fail_line(l))
        .or_else(|| out.lines().find(|l| !l.trim().is_empty()))
        .unwrap_or("");
    crate::strutil::safe_truncate(line.trim(), 200)
}

fn solution_text(succ: &Observation, edited: &[String]) -> String {
    let cmd = crate::strutil::safe_truncate(cmd_input(succ), 120);
    if edited.is_empty() {
        format!("re-ran `{cmd}` → passed")
    } else {
        format!("edited {}; re-ran `{cmd}` → passed", edited.join(", "))
    }
}

/// Mine error→fix corrections from an ordered session transcript. For each
/// failing command, look ahead for the next same-command success, recording any
/// intervening edits as the fix. A success only counts when the command is
/// identical to the failure or at least one edit happened in between (so an
/// unrelated later command is never mistaken for the fix).
pub fn mine_session(observations: &[Observation]) -> Vec<Correction> {
    let mut out: Vec<Correction> = Vec::new();

    for (i, fail) in observations.iter().enumerate() {
        if !is_command(fail) || !output_failed(fail) {
            continue;
        }
        let fhead = match head(fail) {
            Some(h) => h,
            None => continue,
        };

        let mut edited: Vec<String> = Vec::new();
        let mut solved: Option<&Observation> = None;
        for succ in &observations[i + 1..] {
            if is_edit(succ) {
                let target = crate::strutil::safe_truncate(cmd_input(succ), 80);
                if !target.is_empty() && !edited.contains(&target) {
                    edited.push(target);
                }
                continue;
            }
            if is_command(succ) && head(succ) == Some(fhead) {
                if output_failed(succ) {
                    continue; // still failing — keep scanning for the eventual pass
                }
                if succ.input == fail.input || !edited.is_empty() {
                    solved = Some(succ);
                }
                break;
            }
        }

        if let Some(succ) = solved {
            let c = Correction {
                problem: format!(
                    "`{}` failed: {}",
                    crate::strutil::safe_truncate(cmd_input(fail), 200),
                    error_snippet(fail)
                ),
                solution: solution_text(succ, &edited),
            };
            if !out.contains(&c) {
                out.push(c);
            }
        }
        if out.len() >= MAX_CORRECTIONS_PER_SESSION {
            break;
        }
    }
    out
}

/// Persist one mined correction as a project-scoped `error_solution` memory.
async fn store_correction(
    db: &Database,
    embedder: Option<&dyn Embedder>,
    store: &dyn VectorStore,
    project: &str,
    session_id: &str,
    c: &Correction,
) -> Result<i64> {
    let text = format!("Error: {}\nFix: {}", c.problem, c.solution);
    let id = db::insert_memory(db, project, session_id, &text, Some("error_solution fix")).await?;
    db::upsert_memory_meta(db, id, 0.8).await?; // fixes are valuable to recall
    db::set_memory_scope_kind(db, id, "project", "error_solution").await?;

    if let Some(emb) = embedder {
        match emb.embed(std::slice::from_ref(&text)).await {
            Ok(mut v) => {
                if let Some(vec) = v.drain(..).next() {
                    if let Err(e) = store.upsert(db, id, emb.id(), emb.dim(), &vec).await {
                        tracing::warn!("correction embed upsert failed (memory {id}): {e}");
                    }
                }
            }
            Err(e) => tracing::warn!("correction embed failed (memory {id}): {e}"),
        }
    }
    Ok(id)
}

/// Mine + persist corrections for a session. Returns how many were stored.
pub async fn mine_and_store(
    db: &Database,
    embedder: Option<&dyn Embedder>,
    store: &dyn VectorStore,
    project: &str,
    session_id: &str,
    observations: &[Observation],
) -> Result<usize> {
    let corrections = mine_session(observations);
    for c in &corrections {
        store_correction(db, embedder, store, project, session_id, c).await?;
    }
    if !corrections.is_empty() {
        tracing::info!(
            "Mined {} correction(s) from session {session_id}",
            corrections.len()
        );
    }
    Ok(corrections.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::create_session;
    use crate::vectorstore::BruteForceStore;

    fn obs(tool: &str, input: &str, output: &str) -> Observation {
        Observation {
            id: 0,
            session_id: "s".into(),
            project: "/tmp/p".into(),
            tool: tool.into(),
            input: Some(input.into()),
            output: Some(output.into()),
            created_at: 0,
        }
    }

    #[test]
    fn mines_fail_edit_pass_loop() {
        let transcript = vec![
            obs("Bash", "cargo build", "error[E0425]: cannot find value `incref_blob`"),
            obs("Edit", "src/db.rs", "ok"),
            obs("Bash", "cargo build", "Finished `dev` profile"),
        ];
        let c = mine_session(&transcript);
        assert_eq!(c.len(), 1);
        assert!(c[0].problem.contains("cargo build"));
        assert!(c[0].problem.contains("E0425"));
        assert!(c[0].solution.contains("src/db.rs"));
        assert!(c[0].solution.contains("passed"));
    }

    #[test]
    fn passing_test_output_is_not_a_failure() {
        // cargo's success line contains "0 failed" — must NOT be mined.
        let transcript = vec![
            obs("Bash", "cargo test", "test result: ok. 97 passed; 0 failed"),
            obs("Bash", "cargo test", "test result: ok. 97 passed; 0 failed"),
        ];
        assert!(mine_session(&transcript).is_empty());
    }

    #[test]
    fn unresolved_failure_yields_no_correction() {
        let transcript = vec![
            obs("Bash", "cargo build", "error: could not compile `ironmem`"),
            obs("Edit", "src/db.rs", "ok"),
            // never re-run successfully
        ];
        assert!(mine_session(&transcript).is_empty());
    }

    #[test]
    fn unrelated_success_without_edits_is_not_a_fix() {
        let transcript = vec![
            obs("Bash", "cargo build", "error[E0425]: cannot find value"),
            obs("Bash", "ls", "src/ target/"),
            obs("Bash", "git status", "clean"),
        ];
        assert!(mine_session(&transcript).is_empty());
    }

    #[tokio::test]
    async fn mine_and_store_persists_error_solution_memories() {
        let path = std::env::temp_dir().join(format!("ironmem-corr-{}.db", uuid::Uuid::new_v4()));
        let db = Database::new(&path.to_string_lossy()).await.unwrap();
        db.migrate().await.unwrap();
        let store = BruteForceStore;
        let s = create_session(&db, "/tmp/p").await.unwrap();

        let transcript = vec![
            obs("Bash", "pytest", "E   AssertionError: assertion failed"),
            obs("Write", "tests/test_x.py", "ok"),
            obs("Bash", "pytest", "5 passed"),
        ];
        let n = mine_and_store(&db, None, &store, "/tmp/p", &s, &transcript)
            .await
            .unwrap();
        assert_eq!(n, 1);

        // Stored as a project-scoped error_solution memory.
        let mems = db::get_recent_memories_scoped(&db, "project", Some("/tmp/p"), 10)
            .await
            .unwrap();
        let m = mems.iter().find(|m| m.summary.starts_with("Error:")).unwrap();
        let info = db::get_memory_meta_full(&db, m.id).await.unwrap();
        assert_eq!((info.scope.as_str(), info.kind.as_str()), ("project", "error_solution"));

        let _ = std::fs::remove_file(path);
    }
}
