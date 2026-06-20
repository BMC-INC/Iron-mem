use anyhow::{anyhow, Result};
use chrono::NaiveDate;
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
    /// The event time/date(-range) the session describes, if it states one
    /// (`WHEN:`). Stored on the narrative memory's `event_time` to power the
    /// time-aware retrieval boost. `None` when the session is undated.
    pub event_time: Option<String>,
    /// Proper nouns named in the session (`ENTITIES:`) — people, places,
    /// organizations, products. Indexed in `memory_entities` so name-anchored
    /// questions resolve by direct lookup regardless of keyword/vector rank.
    pub entities: Vec<String>,
    /// Structured relation edges extracted from the session (`RELATIONS:`).
    /// Persisted as temporal graph edges with source-memory provenance.
    pub relations: Vec<MemoryRelation>,
    /// Durable operating rules / workflow instructions extracted from the
    /// session. Persisted as separate `kind=procedural` memories.
    pub procedures: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MemoryRelation {
    pub source: String,
    pub relation: String,
    pub target: String,
    pub valid_from: Option<String>,
    pub valid_until: Option<String>,
    pub confidence: f64,
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
            event_time: None,
            entities: Vec::new(),
            relations: Vec::new(),
            procedures: Vec::new(),
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
    lines.push("FACTS: [one atomic fact per line, each starting with \"- \". Be EXHAUSTIVE — extract EVERY concrete fact stated; do not summarize, merge, or skip. Each fact must stand completely on its own: name the person or subject explicitly (never a bare \"she/he/they/it\"), and carry any date, place, quantity, or proper noun it involves, e.g. \"- Caroline researched adoption agencies\" or \"- Melanie painted a sunrise in 2022\". Omit only greetings and filler. If there are genuinely no concrete facts, write \"- none\".]".to_string());
    lines.push(
        "TAGS: [8-12 space-separated lowercase keywords: technologies, file names, concepts]"
            .to_string(),
    );
    lines.push(
        "IMPORTANCE: [single integer 1-10 — how important this session is to remember long-term: 1 trivial/exploratory, 10 critical decisions or lasting changes]"
            .to_string(),
    );
    lines.push(
        "KIND: [single word classifying this session: session | error_solution | preference | procedural | architecture | learned_pattern | project_config — default 'session']"
            .to_string(),
    );
    lines.push(
        "WHEN: [if the session describes events on a specific date or date range, give it as YYYY-MM-DD (or a short range like 2023-05-07..2023-05-09); otherwise write 'none'.]"
            .to_string(),
    );
    lines.push(
        "ENTITIES: [comma-separated proper nouns named in the session — people, places, organizations, products. Omit common words; write 'none' if there are no proper nouns.]"
            .to_string(),
    );
    lines.push(
        "RELATIONS: [one structured relation per line, each starting with \"- \". Format exactly: source | relation | target | valid_from | valid_until | confidence. Use concise relation names like works_at, lives_at, status, depends_on, decided, owns, uses, part_of. Put 'none' for unknown dates. Confidence is 0.0-1.0. Example: \"- Caroline | status | approved | 2026-06-05 | none | 0.9\". If there are no durable relationships, write \"- none\".]"
            .to_string(),
    );
    lines.push(
        "PROCEDURES: [one durable workflow rule or operating instruction per line, each starting with \"- \". Only include reusable rules about how future work should be done, not one-off facts. Examples: \"- For Operator OS, keep tenant isolation explicit before shared/team memory\" or \"- Use env files for secrets; never print raw keys\". If none, write \"- none\".]"
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
    let mut event_time: Option<String> = None;
    let mut entities: Vec<String> = Vec::new();
    let mut relations: Vec<MemoryRelation> = Vec::new();
    let mut procedures: Vec<String> = Vec::new();
    // FACTS is a multi-line block: once the marker is seen, subsequent "- "
    // bullet lines are facts until the next known marker ends the block.
    let mut in_facts = false;
    let mut in_relations = false;
    let mut in_procedures = false;

    for line in text.lines() {
        if let Some(s) = line.strip_prefix("SUMMARY:") {
            summary = s.trim().to_string();
            in_facts = false;
            in_relations = false;
            in_procedures = false;
        } else if let Some(rest) = line.strip_prefix("FACTS:") {
            in_facts = true;
            in_relations = false;
            in_procedures = false;
            // Tolerate a first fact placed on the marker line itself.
            push_fact(&mut facts, rest);
        } else if let Some(t) = line.strip_prefix("TAGS:") {
            tags = t.trim().to_string();
            in_facts = false;
            in_relations = false;
            in_procedures = false;
        } else if let Some(i) = line.strip_prefix("IMPORTANCE:") {
            importance = parse_importance(i);
            in_facts = false;
            in_relations = false;
            in_procedures = false;
        } else if let Some(k) = line.strip_prefix("KIND:") {
            // Clamp to the known set; unrecognized values collapse to `session`.
            kind = Some(crate::db::clamp_kind(k).to_string());
            in_facts = false;
            in_relations = false;
            in_procedures = false;
        } else if let Some(w) = line.strip_prefix("WHEN:") {
            let w = w.trim();
            // Treat blank / "none" / "unknown" as undated.
            if !w.is_empty()
                && !w.eq_ignore_ascii_case("none")
                && !w.eq_ignore_ascii_case("unknown")
                && is_valid_memory_date_or_range(w)
            {
                event_time = Some(w.to_string());
            }
            in_facts = false;
            in_relations = false;
            in_procedures = false;
        } else if let Some(e) = line.strip_prefix("ENTITIES:") {
            for ent in e.split(',') {
                let ent = ent.trim();
                if !ent.is_empty() && !ent.eq_ignore_ascii_case("none") {
                    entities.push(ent.to_string());
                }
            }
            in_facts = false;
            in_relations = false;
            in_procedures = false;
        } else if let Some(rest) = line.strip_prefix("RELATIONS:") {
            in_facts = false;
            in_relations = true;
            in_procedures = false;
            push_relation(&mut relations, rest);
        } else if let Some(rest) = line.strip_prefix("PROCEDURES:") {
            in_facts = false;
            in_relations = false;
            in_procedures = true;
            push_procedure(&mut procedures, rest);
        } else if in_facts {
            push_fact(&mut facts, line);
        } else if in_relations {
            push_relation(&mut relations, line);
        } else if in_procedures {
            push_procedure(&mut procedures, line);
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
        event_time,
        entities,
        relations,
        procedures,
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

fn optional_relation_date(s: &str) -> Option<String> {
    let s = s.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("none") || s.eq_ignore_ascii_case("unknown") {
        None
    } else if is_valid_memory_date(s) {
        Some(s.to_string())
    } else {
        None
    }
}

pub fn is_valid_memory_date(s: &str) -> bool {
    s.len() == 10 && NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok()
}

pub fn is_valid_memory_date_or_range(s: &str) -> bool {
    if is_valid_memory_date(s) {
        return true;
    }
    let Some((start, end)) = s.split_once("..") else {
        return false;
    };
    let Ok(start) = NaiveDate::parse_from_str(start.trim(), "%Y-%m-%d") else {
        return false;
    };
    let Ok(end) = NaiveDate::parse_from_str(end.trim(), "%Y-%m-%d") else {
        return false;
    };
    start <= end
}

fn push_relation(relations: &mut Vec<MemoryRelation>, line: &str) {
    let trimmed = line.trim();
    let bullet = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix('-'));
    let Some(raw) = bullet.map(str::trim) else {
        return;
    };
    if raw.is_empty() || raw.eq_ignore_ascii_case("none") {
        return;
    }

    let parts = raw.split('|').map(str::trim).collect::<Vec<_>>();
    if parts.len() != 6 {
        return;
    }
    let source = parts[0];
    let relation = parts[1];
    let target = parts[2];
    if source.is_empty() || relation.is_empty() || target.is_empty() {
        return;
    }
    let confidence = parts[5].parse::<f64>().unwrap_or(0.5).clamp(0.0, 1.0);
    relations.push(MemoryRelation {
        source: source.to_string(),
        relation: relation.to_string(),
        target: target.to_string(),
        valid_from: optional_relation_date(parts[3]),
        valid_until: optional_relation_date(parts[4]),
        confidence,
    });
}

fn push_procedure(procedures: &mut Vec<String>, line: &str) {
    let trimmed = line.trim();
    let bullet = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix('-'));
    if let Some(p) = bullet {
        let p = p.trim();
        if !p.is_empty() && !p.eq_ignore_ascii_case("none") {
            procedures.push(p.to_string());
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
    digits.parse::<i64>().ok().map(|n| n.clamp(1, 10) as u8)
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

pub async fn compress(observations: &[Observation], config: &Config) -> Result<CompressionResult> {
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
    complete_with(prompt, &config.model, config).await
}

/// Like [`complete`] but against an explicit model rather than the configured
/// compression model — used by retrieval reranking, which wants a fast, cheap
/// model independent of the compression model.
pub async fn complete_with(prompt: &str, model: &str, config: &Config) -> Result<String> {
    let api_key = resolve_api_key(config.provider)?;
    match config.provider {
        Provider::Anthropic => anthropic_text(prompt, model, &api_key).await,
        Provider::Openai => openai_text(prompt, model, &api_key).await,
        Provider::Google => google_text(prompt, model, &api_key).await,
    }
}

/// Extract only structured relation lines from an existing memory summary. Used
/// by graph backfill so older memories can gain graph edges without rewriting
/// their summaries or facts.
pub async fn extract_relations_from_memory_text(
    summary: &str,
    tags: Option<&str>,
    config: &Config,
) -> Result<Vec<MemoryRelation>> {
    let prompt = format!(
        "You are backfilling a temporal memory graph from an existing compressed memory.\n\
         Extract durable relationships only. Do not rewrite the memory.\n\n\
         MEMORY SUMMARY:\n{}\n\nTAGS:\n{}\n\n\
         Respond with EXACTLY this format:\n\
         RELATIONS: [one structured relation per line, each starting with \"- \". Format exactly: source | relation | target | valid_from | valid_until | confidence. Use concise relation names like works_at, lives_at, status, depends_on, decided, owns, uses, part_of. Put 'none' for unknown dates. Confidence is 0.0-1.0. If there are no durable relationships, write \"- none\".]",
        summary,
        tags.unwrap_or("")
    );
    let reply = complete(&prompt, config).await?;
    Ok(parse_response(&reply).relations)
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
    Ok(parse_response(
        &anthropic_text(prompt, model, api_key).await?,
    ))
}

async fn anthropic_text(prompt: &str, model: &str, api_key: &str) -> Result<String> {
    let client = reqwest::Client::new();
    let req = AnthropicRequest {
        model: model.to_string(),
        max_tokens: 2048,
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
        max_tokens: 2048,
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
    fn parse_response_extracts_when_as_event_time() {
        let r = parse_response("SUMMARY: s\nKIND: session\nWHEN: 2023-05-07");
        assert_eq!(r.event_time.as_deref(), Some("2023-05-07"));
    }

    #[test]
    fn parse_response_treats_when_none_as_undated() {
        assert!(parse_response("SUMMARY: s\nWHEN: none")
            .event_time
            .is_none());
        assert!(parse_response("SUMMARY: s").event_time.is_none());
    }

    #[test]
    fn prompt_emits_when_section() {
        assert!(
            build_prompt(&[]).contains("WHEN:"),
            "prompt must request WHEN"
        );
    }

    #[test]
    fn parse_response_extracts_entities_csv() {
        let r = parse_response("SUMMARY: s\nENTITIES: Caroline, Melanie, New York\nKIND: session");
        assert_eq!(r.entities, vec!["Caroline", "Melanie", "New York"]);
    }

    #[test]
    fn parse_response_entities_none_is_empty() {
        assert!(parse_response("SUMMARY: s\nENTITIES: none")
            .entities
            .is_empty());
        assert!(parse_response("SUMMARY: s").entities.is_empty());
    }

    #[test]
    fn prompt_emits_entities_section() {
        assert!(
            build_prompt(&[]).contains("ENTITIES:"),
            "prompt must request ENTITIES"
        );
    }

    #[test]
    fn parse_response_extracts_relations_block() {
        let r = parse_response(
            "SUMMARY: s\nRELATIONS:\n- Caroline | status | approved | 2026-06-05 | none | 0.9\n- Operator OS | depends_on | Iron Mem | none | none | 0.75\nTAGS: a b",
        );
        assert_eq!(r.relations.len(), 2);
        assert_eq!(
            r.relations[0],
            MemoryRelation {
                source: "Caroline".into(),
                relation: "status".into(),
                target: "approved".into(),
                valid_from: Some("2026-06-05".into()),
                valid_until: None,
                confidence: 0.9,
            }
        );
        assert_eq!(r.relations[1].source, "Operator OS");
        assert_eq!(r.relations[1].target, "Iron Mem");
        assert_eq!(r.relations[1].confidence, 0.75);
    }

    #[test]
    fn parse_response_validates_temporal_dates() {
        let r = parse_response(
            "SUMMARY: s\nWHEN: 2026-02-30\nRELATIONS:\n- Caroline | status | approved | 2026-13-05 | none | 0.9",
        );
        assert!(r.event_time.is_none());
        assert_eq!(r.relations.len(), 1);
        assert!(r.relations[0].valid_from.is_none());

        let ok = parse_response(
            "SUMMARY: s\nWHEN: 2026-06-01..2026-06-05\nRELATIONS:\n- Caroline | status | approved | 2026-06-05 | none | 0.9",
        );
        assert_eq!(ok.event_time.as_deref(), Some("2026-06-01..2026-06-05"));
        assert_eq!(ok.relations[0].valid_from.as_deref(), Some("2026-06-05"));
    }

    #[test]
    fn parse_response_ignores_malformed_or_none_relations() {
        let r = parse_response(
            "SUMMARY: s\nRELATIONS:\n- none\n- missing | fields\n-  | status | draft | none | none | 0.8\n- James | role | operator | none | none | 1.4",
        );
        assert_eq!(r.relations.len(), 1);
        assert_eq!(r.relations[0].source, "James");
        assert_eq!(r.relations[0].confidence, 1.0);
    }

    #[test]
    fn parse_response_extracts_procedures_block() {
        let r = parse_response(
            "SUMMARY: s\nPROCEDURES:\n- Keep tenant isolation explicit before shared memory.\n- Use env files for secrets.\n- none",
        );
        assert_eq!(
            r.procedures,
            vec![
                "Keep tenant isolation explicit before shared memory.".to_string(),
                "Use env files for secrets.".to_string()
            ]
        );
    }

    #[test]
    fn prompt_emits_relations_section() {
        assert!(
            build_prompt(&[]).contains("RELATIONS:"),
            "prompt must request RELATIONS"
        );
    }

    #[test]
    fn prompt_emits_procedures_section() {
        assert!(
            build_prompt(&[]).contains("PROCEDURES:"),
            "prompt must request PROCEDURES"
        );
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
