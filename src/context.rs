//! Session-start context signal. Derives a free-text query from the project's
//! recent git activity (commit subjects, dirty files, staged-vs-HEAD diff) so
//! injection can rank memories by what the user is actually working on. Returns
//! `None` when the path isn't a git repo, git is missing, or there's no signal.

use std::process::Command;

/// Cap on the returned signal so we don't feed an enormous blob to the embedder.
const MAX_LEN: usize = 2000;

/// Build a query string from the project's git state, or `None` if unavailable.
pub fn git_query(project: &str) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();

    if let Some(subjects) = run_git(project, &["log", "-n", "20", "--format=%s"]) {
        let trimmed = subjects.trim();
        if !trimmed.is_empty() {
            parts.push(trimmed.to_string());
        }
    }
    if let Some(status) = run_git(project, &["status", "--porcelain"]) {
        let trimmed = status.trim();
        if !trimmed.is_empty() {
            parts.push(trimmed.to_string());
        }
    }
    if let Some(diff) = run_git(project, &["diff", "--name-only", "HEAD"]) {
        let trimmed = diff.trim();
        if !trimmed.is_empty() {
            parts.push(trimmed.to_string());
        }
    }

    if parts.is_empty() {
        return None;
    }
    let mut joined = parts.join("\n");
    if joined.len() > MAX_LEN {
        // Truncate on a char boundary so we never split a UTF-8 sequence.
        let mut end = MAX_LEN;
        while end > 0 && !joined.is_char_boundary(end) {
            end -= 1;
        }
        joined.truncate(end);
    }
    Some(joined)
}

/// Run `git -C <project> <args...>`, returning stdout on success only.
fn run_git(project: &str, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(project)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_query_none_for_nonexistent_path() {
        assert!(git_query("/nonexistent/path/should/not/exist").is_none());
    }

    #[test]
    fn git_query_returns_signal_for_this_repo() {
        // The crate itself is a git repo with history, so we expect a signal.
        let here = env!("CARGO_MANIFEST_DIR");
        let q = git_query(here);
        assert!(q.is_some());
        assert!(q.unwrap().len() <= MAX_LEN);
    }
}
