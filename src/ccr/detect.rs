// Private helpers are used by tests now and by later CCR tasks at runtime.
#![allow(dead_code)]

/// Content-type categories used by the CCR codec selection logic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentType {
    Json,
    Code,
    Log,
    Diff,
    Text,
    Binary,
}

/// Detect the content type of `bytes`.
///
/// `path_hint` is an optional file extension (without the leading dot, e.g.
/// `"rs"`, `"py"`) that improves Code detection.
///
/// Detection order (first match wins):
/// 1. Binary  — contains a NUL byte **or** is not valid UTF-8
/// 2. JSON    — entire trimmed input parses as a JSON value
/// 3. Diff    — leading unified-diff markers or high +/- line ratio
/// 4. Log     — high ratio of lines that look like structured log lines
/// 5. Code    — known source extension via `path_hint`, or brace/semicolon density
/// 6. Text    — everything else
pub fn detect(bytes: &[u8], path_hint: Option<&str>) -> ContentType {
    // 1. Binary: NUL byte or invalid UTF-8
    if bytes.contains(&0u8) {
        return ContentType::Binary;
    }
    let text = match std::str::from_utf8(bytes) {
        Ok(s) => s,
        Err(_) => return ContentType::Binary,
    };

    // 2. JSON: serde_json parses the trimmed input AND it's a structured value.
    // Bare scalars (`null`, `42`, `true`, `false`) parse fine but aren't docs.
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(text.trim()) {
        if matches!(
            v,
            serde_json::Value::Object(_) | serde_json::Value::Array(_)
        ) {
            return ContentType::Json;
        }
    }

    // 3. Diff detection
    if is_diff(text) {
        return ContentType::Diff;
    }

    // 4. Log detection
    if is_log(text) {
        return ContentType::Log;
    }

    // 5. Code detection
    if is_code(text, path_hint) {
        return ContentType::Code;
    }

    // 6. Fallback
    ContentType::Text
}

// ── Diff heuristics ──────────────────────────────────────────────────────────

fn is_diff(text: &str) -> bool {
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return false;
    }

    // Strong signal: first non-empty line is a well-known unified-diff header
    for line in &lines {
        let t = line.trim_start();
        if t.is_empty() {
            continue;
        }
        if t.starts_with("@@ ")
            || t.starts_with("--- ")
            || t.starts_with("+++ ")
            || t.starts_with("diff ")
        {
            return true;
        }
        // First non-empty line checked — stop looking for strong signal
        break;
    }

    // Weaker signal: high ratio of +/- / @@ lines. To avoid false positives on
    // signed numeric/CSV data, also require a real unified-diff hunk header
    // (a line starting with `@@`); genuine diffs always contain one.
    let non_empty: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if non_empty.is_empty() {
        return false;
    }
    let has_hunk_header = non_empty.iter().any(|l| l.starts_with("@@"));
    if !has_hunk_header {
        return false;
    }
    let diff_lines = non_empty
        .iter()
        .filter(|l| matches!(l.as_bytes().first(), Some(b'+') | Some(b'-') | Some(b'@')))
        .count();
    // Require at least 3 diff-looking lines AND ≥60% ratio
    diff_lines >= 3 && diff_lines * 10 >= non_empty.len() * 6
}

// ── Log heuristics ───────────────────────────────────────────────────────────

/// Returns true if `line` looks like a structured log line.
///
/// Criteria (any one suffices):
/// - Starts with a 4-digit year followed by `-`  (e.g. `2026-06-07T…`)
/// - Contains a level token surrounded by spaces or brackets:
///   ` INFO `, ` WARN `, ` ERROR `, ` DEBUG `, ` TRACE `,
///   `[INFO]`, `[WARN]`, `[ERROR]`, `[DEBUG]`, `[TRACE]`
fn line_looks_like_log(line: &str) -> bool {
    // Timestamp prefix: starts with YYYY-
    let b = line.as_bytes();
    if b.len() >= 5
        && b[0].is_ascii_digit()
        && b[1].is_ascii_digit()
        && b[2].is_ascii_digit()
        && b[3].is_ascii_digit()
        && b[4] == b'-'
    {
        return true;
    }

    // Level tokens (padded with space or brackets)
    const SPACE_LEVELS: &[&str] = &[
        " INFO ", " WARN ", " ERROR ", " DEBUG ", " TRACE ",
        " INFO\t", " WARN\t", " ERROR\t", " DEBUG\t", " TRACE\t",
    ];
    const BRACKET_LEVELS: &[&str] =
        &["[INFO]", "[WARN]", "[ERROR]", "[DEBUG]", "[TRACE]"];

    for tok in SPACE_LEVELS {
        if line.contains(tok) {
            return true;
        }
    }
    for tok in BRACKET_LEVELS {
        if line.contains(tok) {
            return true;
        }
    }

    false
}

fn is_log(text: &str) -> bool {
    let non_empty: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    // A single line is too ambiguous to classify as a log.
    if non_empty.len() < 2 {
        return false;
    }
    let log_lines = non_empty.iter().filter(|l| line_looks_like_log(l)).count();
    // ≥50% of non-empty lines must look like log lines
    log_lines * 2 >= non_empty.len()
}

// ── Code heuristics ──────────────────────────────────────────────────────────

const CODE_EXTENSIONS: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "py", "go", "c", "h", "cpp", "hpp", "java", "rb", "kt",
    "swift", "php", "cs", "sh", "bash", "zsh", "fish", "lua", "r", "jl", "ex", "exs", "erl",
    "hs", "ml", "mli", "clj", "cljs", "scala", "groovy", "dart", "vim", "tf", "toml", "yaml",
    "yml",
];

fn is_code(text: &str, path_hint: Option<&str>) -> bool {
    // Strong signal: known source extension
    if let Some(ext) = path_hint {
        let ext_lower = ext.to_lowercase();
        if CODE_EXTENSIONS.contains(&ext_lower.as_str()) {
            return true;
        }
    }

    // Density heuristic: count braces and semicolons
    let chars: usize = text.chars().count();
    if chars == 0 {
        return false;
    }
    let braces = text.chars().filter(|&c| matches!(c, '{' | '}' | '(' | ')')).count();
    let semis = text.chars().filter(|&c| c == ';').count();
    let density = (braces + semis) as f64 / chars as f64;
    // >2% brace/semicolon density is a strong code signal
    density > 0.02
}

// ── Tests (TDD — written before implementation) ───────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_object() {
        assert_eq!(detect(b"{\"a\":1}", None), ContentType::Json);
    }

    #[test]
    fn json_array() {
        assert_eq!(detect(b"[1, 2, 3]", None), ContentType::Json);
    }

    #[test]
    fn diff_unified() {
        let input = b"@@ -1 +1 @@\n-x\n+y\n";
        assert_eq!(detect(input, None), ContentType::Diff);
    }

    #[test]
    fn diff_with_headers() {
        let input = b"--- a/foo.rs\n+++ b/foo.rs\n@@ -1,3 +1,3 @@\n-old\n+new\n context\n";
        assert_eq!(detect(input, None), ContentType::Diff);
    }

    #[test]
    fn log_with_timestamps() {
        let input =
            b"2026-06-07T00:00:00Z INFO server started\n2026-06-07T00:00:01Z DEBUG listening\n2026-06-07T00:00:02Z WARN slow query\n";
        assert_eq!(detect(input, None), ContentType::Log);
    }

    #[test]
    fn log_with_level_tokens() {
        let input = b"[2026-06-07] INFO foo bar\n[2026-06-07] ERROR baz\n[2026-06-07] WARN qux\n";
        assert_eq!(detect(input, None), ContentType::Log);
    }

    #[test]
    fn code_with_rs_hint() {
        assert_eq!(detect(b"fn main(){}", Some("rs")), ContentType::Code);
    }

    #[test]
    fn code_with_py_hint() {
        let input = b"def hello():\n    print('world')\n";
        assert_eq!(detect(input, Some("py")), ContentType::Code);
    }

    #[test]
    fn code_via_brace_density() {
        // No hint; rely on brace/semicolon density
        let input = b"fn main() { let x = 1; let y = 2; println!(\"{}\", x + y); }";
        assert_eq!(detect(input, None), ContentType::Code);
    }

    #[test]
    fn binary_nul_byte() {
        let input = b"\x00\x01\x02binary data";
        assert_eq!(detect(input, None), ContentType::Binary);
    }

    #[test]
    fn binary_invalid_utf8() {
        // 0xFF is never valid in UTF-8
        let input = &[0xFF, 0xFE, 0x41, 0x42];
        assert_eq!(detect(input, None), ContentType::Binary);
    }

    #[test]
    fn plain_text() {
        let input = b"This is a plain English sentence without any special structure.\nAnother sentence here.";
        assert_eq!(detect(input, None), ContentType::Text);
    }

    #[test]
    fn empty_is_text() {
        assert_eq!(detect(b"", None), ContentType::Text);
    }

    // ── Negative cases (false-positive traps) ────────────────────────────────

    #[test]
    fn json_scalars_are_not_json() {
        // Bare JSON scalars parse successfully but are not structured docs.
        assert_eq!(detect(b"null", None), ContentType::Text);
        assert_eq!(detect(b"42", None), ContentType::Text);
        assert_eq!(detect(b"true", None), ContentType::Text);
        assert_eq!(detect(b"false", None), ContentType::Text);
    }

    #[test]
    fn signed_numeric_data_is_not_diff() {
        // +/- prefixed numeric/CSV data has no hunk header → not a diff.
        let csv = b"+1.5,+2.3\n-3.7,+4.1\n+5.0,-6.2\n";
        assert_ne!(detect(csv, None), ContentType::Diff);
    }

    #[test]
    fn single_timestamp_line_is_not_log() {
        // A single line is too ambiguous to classify as a log.
        assert_ne!(
            detect(b"2026-06-07T00:00:00Z something happened", None),
            ContentType::Log
        );
    }
}
