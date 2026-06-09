use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::db::Observation;

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    #[default]
    Anthropic,
    Openai,
    Google,
}

impl Provider {
    pub fn api_key_env(&self) -> &'static str {
        match self {
            Provider::Anthropic => "ANTHROPIC_API_KEY",
            Provider::Openai => "OPENAI_API_KEY",
            Provider::Google => "GOOGLE_API_KEY",
        }
    }

    pub fn default_model(&self) -> &'static str {
        match self {
            Provider::Anthropic => "claude-sonnet-4-6-20250627",
            Provider::Openai => "gpt-4o",
            Provider::Google => "gemini-2.0-flash",
        }
    }
}

// ── Compression result ──────────────────────────────────────────────

#[derive(Debug)]
pub struct CompressionResult {
    pub summary: String,
    pub tags: String,
    /// LLM-estimated importance, 1 (trivial) – 10 (critical). Defaults to 5.
    pub importance: u8,
    /// Typed classification of the session, clamped to [`crate::db::MEMORY_KINDS`].
    /// Defaults to `session`.
    pub kind: String,
    /// Atomic, self-contained facts extracted alongside the narrative summary.
    /// Each is stored as its own searchable `kind=fact` memory so specifics
    /// (dates, names, quantities) survive compression and rank on direct lookup.
    pub facts: Vec<String>,
}

/// Default importance when the model omits or mangles the IMPORTANCE line.
const DEFAULT_IMPORTANCE: u8 = 5;

impl Default for CompressionResult {
    fn default() -> Self {
        Self {
            summary: String::new(),
            tags: String::new(),
            importance: DEFAULT_IMPORTANCE,
            kind: "session".to_string(),
            facts: Vec::new(),
        }
    }
}

// ── Shared prompt builder ───────────────────────────────────────────

fn build_prompt(observations: &[Observation]) -> String {
    let mut lines = vec![
        "You are a memory system. Analyze this session and produce a faithful, compact memory entry. The session may be software development, a conversation, research, planning, or any other activity — adapt to its content and never assume it is code.".to_string(),
        String::new(),
        "SESSION ACTIVITY:".to_string(),
    ];

    for (i, obs) in observations.iter().enumerate() {
        lines.push(format!("{}. Tool: {}", i + 1, obs.tool));
        if let Some(input) = &obs.input {
            lines.push(format!(
                "   Input: {}",
                crate::strutil::safe_truncate(input, 500)
            ));
        }
        if let Some(output) = &obs.output {
            lines.push(format!(
                "   Output: {}",
                crate::strutil::safe_truncate(output, 300)
            ));
        }
    }

    lines.push(String::new());
    lines.push("Respond with EXACTLY this format, nothing else:".to_string());
    lines.push("SUMMARY: [3-6 sentences. PRESERVE every specific: exact dates and times, proper nouns (people, places, organizations, events), quantities, file names, and key quoted statements. Keep causal relationships (X because Y). When the work involves code, still capture what was built/changed/decided, errors resolved, and patterns established. Do not generalize specifics away — write \"attended an LGBTQ support group on 7 May 2023\", never \"attended social events\".]".to_string());
    lines.push("FACTS: [then one atomic fact per line, each starting with \"- \". Each fact must be self-contained and carry its own entity plus any date/quantity, e.g. \"- Caroline joined the LGBTQ support group on 7 May 2023\". Extract every concrete fact stated in the session; omit chit-chat. If there are no concrete facts, write \"- none\".]".to_string());
    lines.push(
        "TAGS: [8-12 space-separated lowercase keywords: technologies, file names, concepts]"
            .to_string(),
    );
    lines.push(
        "IMPORTANCE: [single integer 1-10 — how important this session is to remember long-term: 1 trivial/exploratory, 10 critical decisions or lasting changes]"
            .to_string(),
    );
    lines.push(
        "KIND: [single word classifying this session: session | error_solution | preference | architecture | learned_pattern | project_config — default 'session']"
            .to_string(),
    );

    lines.join("\n")
}

fn parse_response(text: &str) -> CompressionResult {
    let mut summary = String::new();
    let mut tags = String::new();
    let mut importance: Option<u8> = None;
    let mut kind: Option<String> = None;
    let mut facts: Vec<String> = Vec::new();
    // FACTS is a multi-line block: once the marker is seen, subsequent "- "
    // bullet lines are facts until the next known marker ends the block.
    let mut in_facts = false;

    for line in text.lines() {
        if let Some(s) = line.strip_prefix("SUMMARY:") {
            summary = s.trim().to_string();
            in_facts = false;
        } else if let Some(rest) = line.strip_prefix("FACTS:") {
            in_facts = true;
            // Tolerate a first fact placed on the marker line itself.
            push_fact(&mut facts, rest);
        } else if let Some(t) = line.strip_prefix("TAGS:") {
            tags = t.trim().to_string();
            in_facts = false;
        } else if let Some(i) = line.strip_prefix("IMPORTANCE:") {
            importance = parse_importance(i);
            in_facts = false;
        } else if let Some(k) = line.strip_prefix("KIND:") {
            // Clamp to the known set; unrecognized values collapse to `session`.
            kind = Some(crate::db::clamp_kind(k).to_string());
            in_facts = false;
        } else if in_facts {
            push_fact(&mut facts, line);
        }
    }

    if summary.is_empty() {
        summary = text.trim().to_string();
        tags = "session coding".to_string();
    }

    CompressionResult {
        summary,
        tags,
        importance: importance.unwrap_or(DEFAULT_IMPORTANCE),
        kind: kind.unwrap_or_else(|| "session".to_string()),
        facts,
    }
}

/// Append a FACTS-block line as a fact when it is a non-empty "- …" bullet.
/// Skips blanks, lines without bullet syntax, and the "- none" sentinel the
/// prompt asks for when a session has no concrete facts.
fn push_fact(facts: &mut Vec<String>, line: &str) {
    let trimmed = line.trim();
    let bullet = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix('-'));
    if let Some(f) = bullet {
        let f = f.trim();
        if !f.is_empty() && !f.eq_ignore_ascii_case("none") {
            facts.push(f.to_string());
        }
    }
}

/// Parse the IMPORTANCE value, taking the first integer found and clamping to
/// 1..=10. Returns `None` if no integer is present (caller applies the default).
fn parse_importance(s: &str) -> Option<u8> {
    let digits: String = s
        .trim()
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits
        .parse::<i64>()
        .ok()
        .map(|n| n.clamp(1, 10) as u8)
}

// ── API key resolution ──────────────────────────────────────────────

pub fn resolve_api_key(provider: Provider) -> Result<String> {
    let env_var = provider.api_key_env();

    std::env::var(env_var)
        .or_else(|_| {
            if provider == Provider::Anthropic {
                let key_path = crate::config::ironmem_dir().join("api_key");
                std::fs::read_to_string(&key_path)
                    .map(|k| k.trim().to_string())
                    .map_err(|_| std::env::VarError::NotPresent)
            } else {
                Err(std::env::VarError::NotPresent)
            }
        })
        .map_err(|_| anyhow!("{} not set", env_var))
}

// ── Compress dispatcher ─────────────────────────────────────────────

pub async fn compress(
    observations: &[Observation],
    config: &Config,
) -> Result<CompressionResult> {
    if observations.is_empty() {
        return Err(anyhow!("No observations to compress"));
    }

    let api_key = resolve_api_key(config.provider)?;
    let model = &config.model;
    let prompt = build_prompt(observations);

    match config.provider {
        Provider::Anthropic => compress_anthropic(&prompt, model, &api_key).await,
        Provider::Openai => compress_openai(&prompt, model, &api_key).await,
        Provider::Google => compress_google(&prompt, model, &api_key).await,
    }
}

/// Raw single-prompt completion against the configured provider, returning the
/// model's verbatim text. Used by features that need free-form output (e.g. the
/// user-profile generator) rather than the structured compression format.
pub async fn complete(prompt: &str, config: &Config) -> Result<String> {
    let api_key = resolve_api_key(config.provider)?;
    let model = &config.model;
    match config.provider {
        Provider::Anthropic => anthropic_text(prompt, model, &api_key).await,
        Provider::Openai => openai_text(prompt, model, &api_key).await,
        Provider::Google => google_text(prompt, model, &api_key).await,
    }
}

// ── Anthropic ───────────────────────────────────────────────────────

#[derive(Serialize)]
struct AnthropicRequest {
    model: String,
    max_tokens: u32,
    messages: Vec<ChatMessage>,
}

#[derive(Serialize, Deserialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct AnthropicContentBlock {
    #[serde(rename = "type")]
    block_type: String,
    text: Option<String>,
}

#[derive(Deserialize)]
struct AnthropicResponse {
    content: Vec<AnthropicContentBlock>,
}

async fn compress_anthropic(prompt: &str, model: &str, api_key: &str) -> Result<CompressionResult> {
    Ok(parse_response(&anthropic_text(prompt, model, api_key).await?))
}

async fn anthropic_text(prompt: &str, model: &str, api_key: &str) -> Result<String> {
    let client = reqwest::Client::new();
    let req = AnthropicRequest {
        model: model.to_string(),
        max_tokens: 1024,
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: prompt.to_string(),
        }],
    };

    let resp = client
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&req)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("Anthropic API error {}: {}", status, body));
    }

    let data: AnthropicResponse = resp.json().await?;
    data.content
        .into_iter()
        .find(|b| b.block_type == "text")
        .and_then(|b| b.text)
        .ok_or_else(|| anyhow!("No text content in Anthropic response"))
}

// ── OpenAI ──────────────────────────────────────────────────────────

#[derive(Serialize)]
struct OpenAiRequest {
    model: String,
    max_tokens: u32,
    messages: Vec<ChatMessage>,
}

#[derive(Deserialize)]
struct OpenAiChoice {
    message: OpenAiMessage,
}

#[derive(Deserialize)]
struct OpenAiMessage {
    content: Option<String>,
}

#[derive(Deserialize)]
struct OpenAiResponse {
    choices: Vec<OpenAiChoice>,
}

async fn compress_openai(prompt: &str, model: &str, api_key: &str) -> Result<CompressionResult> {
    Ok(parse_response(&openai_text(prompt, model, api_key).await?))
}

async fn openai_text(prompt: &str, model: &str, api_key: &str) -> Result<String> {
    let client = reqwest::Client::new();
    let req = OpenAiRequest {
        model: model.to_string(),
        max_tokens: 1024,
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: prompt.to_string(),
        }],
    };

    let resp = client
        .post("https://api.openai.com/v1/chat/completions")
        .header("Authorization", format!("Bearer {}", api_key))
        .header("content-type", "application/json")
        .json(&req)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("OpenAI API error {}: {}", status, body));
    }

    let data: OpenAiResponse = resp.json().await?;
    data.choices
        .into_iter()
        .next()
        .and_then(|c| c.message.content)
        .ok_or_else(|| anyhow!("No content in OpenAI response"))
}

// ── Google Gemini ───────────────────────────────────────────────────

#[derive(Serialize)]
struct GeminiRequest {
    contents: Vec<GeminiContent>,
}

#[derive(Serialize)]
struct GeminiContent {
    parts: Vec<GeminiPart>,
}

#[derive(Serialize)]
struct GeminiPart {
    text: String,
}

#[derive(Deserialize)]
struct GeminiResponse {
    candidates: Option<Vec<GeminiCandidate>>,
}

#[derive(Deserialize)]
struct GeminiCandidate {
    content: GeminiContentResp,
}

#[derive(Deserialize)]
struct GeminiContentResp {
    parts: Vec<GeminiPartResp>,
}

#[derive(Deserialize)]
struct GeminiPartResp {
    text: Option<String>,
}

async fn compress_google(prompt: &str, model: &str, api_key: &str) -> Result<CompressionResult> {
    Ok(parse_response(&google_text(prompt, model, api_key).await?))
}

async fn google_text(prompt: &str, model: &str, api_key: &str) -> Result<String> {
    let client = reqwest::Client::new();
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
        model, api_key
    );

    let req = GeminiRequest {
        contents: vec![GeminiContent {
            parts: vec![GeminiPart {
                text: prompt.to_string(),
            }],
        }],
    };

    let resp = client
        .post(&url)
        .header("content-type", "application/json")
        .json(&req)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("Google API error {}: {}", status, body));
    }

    let data: GeminiResponse = resp.json().await?;
    data.candidates
        .and_then(|c| c.into_iter().next())
        .and_then(|c| c.content.parts.into_iter().next())
        .and_then(|p| p.text)
        .ok_or_else(|| anyhow!("No content in Gemini response"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_response_extracts_facts_block() {
        let r = parse_response("SUMMARY: s\nFACTS:\n- Caroline joined the LGBTQ group on 7 May 2023\n- Melanie painted a sunrise in 2022\nTAGS: a b\nIMPORTANCE: 6");
        assert_eq!(r.facts.len(), 2);
        assert!(r.facts[0].contains("7 May 2023"));
        // Other fields still parse around the block.
        assert_eq!(r.summary, "s");
        assert_eq!(r.tags, "a b");
        assert_eq!(r.importance, 6);
    }

    #[test]
    fn parse_response_without_facts_yields_empty_vec() {
        let r = parse_response("SUMMARY: s\nTAGS: a b\nIMPORTANCE: 5");
        assert!(r.facts.is_empty());
    }

    #[test]
    fn prompt_emits_facts_section() {
        let p = build_prompt(&[]);
        assert!(p.contains("FACTS:"), "prompt must request a FACTS block");
    }

    #[test]
    fn prompt_preserves_specifics_and_is_domain_agnostic() {
        let p = build_prompt(&[]);
        assert!(p.contains("dates"), "must ask to keep dates");
        assert!(
            p.contains("proper nouns") || p.contains("names"),
            "must ask to keep proper nouns/names"
        );
        assert!(
            !p.contains("coding session"),
            "must not assume the session is coding"
        );
    }

    #[test]
    fn parses_importance_line() {
        let r = parse_response("SUMMARY: did things\nTAGS: a b c\nIMPORTANCE: 8");
        assert_eq!(r.importance, 8);
        assert_eq!(r.summary, "did things");
    }

    #[test]
    fn missing_importance_defaults_to_five() {
        let r = parse_response("SUMMARY: did things\nTAGS: a b c");
        assert_eq!(r.importance, DEFAULT_IMPORTANCE);
    }

    #[test]
    fn importance_clamps_out_of_range() {
        assert_eq!(parse_response("SUMMARY: s\nIMPORTANCE: 0").importance, 1);
        assert_eq!(parse_response("SUMMARY: s\nIMPORTANCE: 42").importance, 10);
    }

    #[test]
    fn importance_tolerates_extra_text() {
        let r = parse_response("SUMMARY: s\nIMPORTANCE: 7 (lasting change)");
        assert_eq!(r.importance, 7);
    }

    #[test]
    fn parses_kind_line() {
        let r = parse_response("SUMMARY: s\nTAGS: a\nIMPORTANCE: 5\nKIND: error_solution");
        assert_eq!(r.kind, "error_solution");
    }

    #[test]
    fn missing_kind_defaults_to_session() {
        let r = parse_response("SUMMARY: s\nTAGS: a b c");
        assert_eq!(r.kind, "session");
    }

    #[test]
    fn invalid_kind_clamps_to_session() {
        assert_eq!(parse_response("SUMMARY: s\nKIND: nonsense").kind, "session");
        // Case-insensitive + whitespace tolerant.
        assert_eq!(
            parse_response("SUMMARY: s\nKIND:   Architecture  ").kind,
            "architecture"
        );
    }
}
