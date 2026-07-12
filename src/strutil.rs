//! Shared string helpers. `safe_truncate` is the single choke point for
//! length-capping untrusted text so a multibyte char landing on the cap
//! boundary can never panic — under `panic = "abort"` (release profile) a
//! panic in an MCP handler kills the whole server process mid-session.

/// Truncate `s` to at most `max_bytes` bytes, backing up to the nearest UTF-8
/// char boundary so the slice can never split a multibyte sequence, and append
/// an ellipsis marker when truncation actually occurred. Returns `s` unchanged
/// when it already fits.
pub fn safe_truncate(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… [truncated]", &s[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_input_is_returned_unchanged() {
        assert_eq!(safe_truncate("abc", 100), "abc");
        assert_eq!(safe_truncate("abc", 3), "abc");
    }

    #[test]
    fn never_panics_across_all_cut_points() {
        let s = "héllo wörld ✓ multibyte ☃ end";
        for n in 0..=s.len() + 4 {
            let _ = safe_truncate(s, n); // must never panic at any byte offset
        }
    }

    #[test]
    fn cut_in_middle_of_multibyte_backs_up_to_boundary() {
        // '✓' occupies bytes 1..4 (3 bytes); cutting at 2 is mid-character.
        let s = "a✓b";
        let out = safe_truncate(s, 2);
        assert!(
            out.starts_with("a… "),
            "kept prefix must end on a char boundary"
        );
        assert!(out.ends_with("… [truncated]"));
    }

    #[test]
    fn marks_truncation_when_over_limit() {
        assert_eq!(safe_truncate("abcdefgh", 4), "abcd… [truncated]");
    }
}
