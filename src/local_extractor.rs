//! Deterministic, extractive fact and procedure mining for offline mode.
//!
//! Every emitted item is either an explicit marker, an extractive sentence
//! from a trusted natural-language observation, or a conservative tool event.
//! The extractor never generates content and never performs network I/O.

use std::collections::HashSet;

use crate::db::Observation;

const MAX_ITEMS: usize = 96;
const MAX_ITEM_BYTES: usize = 600;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalExtraction {
    pub facts: Vec<String>,
    pub procedures: Vec<String>,
}

pub fn extract(observations: &[Observation]) -> LocalExtraction {
    let mut facts = Vec::new();
    let mut procedures = Vec::new();
    let mut seen_facts = HashSet::new();
    let mut seen_procedures = HashSet::new();

    for observation in observations {
        for text in [observation.input.as_deref(), observation.output.as_deref()]
            .into_iter()
            .flatten()
        {
            extract_markers(
                text,
                &mut facts,
                &mut procedures,
                &mut seen_facts,
                &mut seen_procedures,
            );
        }

        if is_successful_command(observation) {
            if let Some(command) = observation.input.as_deref().and_then(clean_command) {
                push_unique(
                    &mut facts,
                    &mut seen_facts,
                    format!("Command `{command}` completed successfully."),
                );
                if is_verification_command(&command) {
                    push_unique(
                        &mut procedures,
                        &mut seen_procedures,
                        format!("Use `{command}` to verify this project."),
                    );
                }
            }
        }

        if is_edit_tool(&observation.tool) {
            if let Some(path) = observation.input.as_deref().and_then(extract_path) {
                push_unique(
                    &mut facts,
                    &mut seen_facts,
                    format!("Modified `{path}` during the session."),
                );
            }
        }

        if is_natural_language_tool(&observation.tool) {
            if let Some(input) = &observation.input {
                extract_signal_sentences(
                    input,
                    &mut facts,
                    &mut procedures,
                    &mut seen_facts,
                    &mut seen_procedures,
                );
            }
        }

        if facts.len() + procedures.len() >= MAX_ITEMS {
            break;
        }
    }

    LocalExtraction { facts, procedures }
}

fn extract_markers(
    text: &str,
    facts: &mut Vec<String>,
    procedures: &mut Vec<String>,
    seen_facts: &mut HashSet<String>,
    seen_procedures: &mut HashSet<String>,
) {
    for line in text.lines() {
        let line = line.trim().trim_start_matches(['-', '*']).trim();
        for prefix in ["FACT:", "MEMORY:"] {
            if let Some(value) = strip_prefix_ascii_case(line, prefix) {
                push_unique(facts, seen_facts, value.to_string());
            }
        }
        for prefix in ["PROCEDURE:", "RULE:"] {
            if let Some(value) = strip_prefix_ascii_case(line, prefix) {
                push_unique(procedures, seen_procedures, value.to_string());
            }
        }
    }
}

fn extract_signal_sentences(
    text: &str,
    facts: &mut Vec<String>,
    procedures: &mut Vec<String>,
    seen_facts: &mut HashSet<String>,
    seen_procedures: &mut HashSet<String>,
) {
    for raw in text.split(['\n', '\r']) {
        let sentence = raw
            .trim()
            .trim_matches(|c| matches!(c, '"' | '\'' | '[' | ']' | '{' | '}' | ','));
        if !is_safe_sentence(sentence) {
            continue;
        }
        let lower = sentence.to_ascii_lowercase();
        if [" must ", " should ", " always ", " never ", " do not "]
            .iter()
            .any(|signal| format!(" {lower} ").contains(signal))
        {
            push_unique(procedures, seen_procedures, sentence.to_string());
        } else if [
            "decided",
            "implemented",
            "completed",
            "fixed",
            "configured",
            "deployed",
            "verified",
            "approved",
            "requires",
            "uses ",
            "agreed",
        ]
        .iter()
        .any(|signal| lower.contains(signal))
        {
            push_unique(facts, seen_facts, sentence.to_string());
        }
    }
}

fn is_safe_sentence(text: &str) -> bool {
    let words = text.split_whitespace().count();
    (4..=80).contains(&words)
        && text.len() <= MAX_ITEM_BYTES
        && !text.starts_with(['$', '#', '<'])
        && !text.contains("-----BEGIN")
        && !text.contains("api_key")
        && !text.contains("token=")
        && !text.contains("password=")
        && text.chars().filter(|c| *c == '{' || *c == '}').count() <= 2
}

fn is_natural_language_tool(tool: &str) -> bool {
    let lower = tool.to_ascii_lowercase();
    lower.contains("task")
        || lower.contains("approvedmemory")
        || lower.contains("askuser")
        || lower.contains("sendmessage")
        || lower == "archive"
        || lower == "remember"
}

fn is_edit_tool(tool: &str) -> bool {
    matches!(
        tool.to_ascii_lowercase().as_str(),
        "edit" | "write" | "multiedit" | "notebookedit" | "apply_patch"
    )
}

fn is_successful_command(observation: &Observation) -> bool {
    if !matches!(
        observation.tool.to_ascii_lowercase().as_str(),
        "bash" | "shell"
    ) {
        return false;
    }
    let output = observation
        .output
        .as_deref()
        .unwrap_or("")
        .to_ascii_lowercase();
    let failure = [
        "test result: failed",
        "build failed",
        "could not compile",
        "traceback (most recent call last)",
        "command not found",
        "exit code 1",
        "exit status 1",
        "fatal:",
    ]
    .iter()
    .any(|marker| output.contains(marker));
    !failure
        && [
            "test result: ok",
            "finished `",
            "build succeeded",
            "tests passed",
            "passed, 0 failed",
            "process exited with code 0",
        ]
        .iter()
        .any(|marker| output.contains(marker))
}

fn clean_command(input: &str) -> Option<String> {
    let command = input.lines().next()?.trim();
    if command.is_empty() || command.len() > 240 || contains_secret_signal(command) {
        return None;
    }
    Some(command.to_string())
}

fn is_verification_command(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    [
        " test",
        "test ",
        "cargo test",
        "cargo clippy",
        "npm run lint",
        "pnpm lint",
        "npm run build",
        "pnpm build",
        "cargo build",
        "pytest",
        "ruff check",
    ]
    .iter()
    .any(|signal| lower.contains(signal))
}

fn extract_path(input: &str) -> Option<String> {
    input
        .split(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ':' | ',' | '{' | '}'))
        .map(|part| part.trim_matches(['(', ')', '[', ']']))
        .find(|part| {
            !part.is_empty()
                && part.len() <= 300
                && (part.contains('/')
                    || [
                        ".rs", ".ts", ".tsx", ".js", ".jsx", ".py", ".md", ".json", ".toml",
                    ]
                    .iter()
                    .any(|suffix| part.ends_with(suffix)))
                && !contains_secret_signal(part)
        })
        .map(str::to_string)
}

fn contains_secret_signal(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    ["api_key", "token=", "password=", "secret="]
        .iter()
        .any(|v| lower.contains(v))
}

fn strip_prefix_ascii_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    text.get(..prefix.len())
        .filter(|head| head.eq_ignore_ascii_case(prefix))
        .map(|_| text[prefix.len()..].trim())
        .filter(|value| !value.is_empty())
}

fn push_unique(target: &mut Vec<String>, seen: &mut HashSet<String>, value: String) {
    let value = value.trim().trim_end_matches(',').trim().to_string();
    if value.is_empty() || value.len() > MAX_ITEM_BYTES || target.len() >= MAX_ITEMS {
        return;
    }
    let key = value.to_ascii_lowercase();
    if seen.insert(key) {
        target.push(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(tool: &str, input: &str, output: &str) -> Observation {
        Observation {
            id: 1,
            session_id: "s".into(),
            project: "/tmp/p".into(),
            tool: tool.into(),
            input: Some(input.into()),
            output: Some(output.into()),
            created_at: 1,
        }
    }

    #[test]
    fn extracts_explicit_markers_and_deduplicates() {
        let result = extract(&[obs(
            "Read",
            "FACT: IronMem stores archives locally\nPROCEDURE: Never send archives online",
            "fact: IronMem stores archives locally",
        )]);
        assert_eq!(result.facts, vec!["IronMem stores archives locally"]);
        assert_eq!(result.procedures, vec!["Never send archives online"]);
    }

    #[test]
    fn extracts_conservative_tool_events_without_secrets() {
        let result = extract(&[
            obs(
                "Bash",
                "cargo test --all-targets",
                "test result: ok. 248 passed",
            ),
            obs("Edit", "src/compress.rs", "updated"),
            obs("Bash", "TOKEN=private cargo test", "test result: ok"),
        ]);
        assert!(result
            .facts
            .contains(&"Command `cargo test --all-targets` completed successfully.".into()));
        assert!(result
            .facts
            .contains(&"Modified `src/compress.rs` during the session.".into()));
        assert_eq!(result.facts.len(), 2);
        assert_eq!(
            result.procedures,
            vec!["Use `cargo test --all-targets` to verify this project."]
        );
    }

    #[test]
    fn extracts_rules_and_decisions_from_natural_language_tools_only() {
        let result = extract(&[
            obs("TaskCreate", "IronMem must remain offline first", ""),
            obs(
                "TaskUpdate",
                "The team decided to use durable extraction receipts",
                "",
            ),
            obs("Read", "An example should not become a procedure", ""),
        ]);
        assert_eq!(result.procedures, vec!["IronMem must remain offline first"]);
        assert_eq!(
            result.facts,
            vec!["The team decided to use durable extraction receipts"]
        );
    }
}
