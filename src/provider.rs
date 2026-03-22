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
}

// ── Shared prompt builder ───────────────────────────────────────────

fn build_prompt(observations: &[Observation]) -> String {
    let mut lines = vec![
        "You are a technical memory system. Analyze these tool calls from a coding session and produce a concise memory entry.".to_string(),
        String::new(),
        "TOOL CALLS:".to_string(),
    ];

    for (i, obs) in observations.iter().enumerate() {
        lines.push(format!("{}. Tool: {}", i + 1, obs.tool));
        if let Some(input) = &obs.input {
            let display = if input.len() > 500 {
                format!("{}... [truncated]", &input[..500])
            } else {
                input.clone()
            };
            lines.push(format!("   Input: {}", display));
        }
        if let Some(output) = &obs.output {
            let display = if output.len() > 300 {
                format!("{}... [truncated]", &output[..300])
            } else {
                output.clone()
            };
            lines.push(format!("   Output: {}", display));
        }
    }

    lines.push(String::new());
    lines.push("Respond with EXACTLY this format, nothing else:".to_string());
    lines.push("SUMMARY: [3-5 sentences describing what was built, changed, or decided. Include specific file names, key decisions, errors resolved, and patterns established.]".to_string());
    lines.push(
        "TAGS: [8-12 space-separated lowercase keywords: technologies, file names, concepts]"
            .to_string(),
    );

    lines.join("\n")
}

fn parse_response(text: &str) -> CompressionResult {
    let mut summary = String::new();
    let mut tags = String::new();

    for line in text.lines() {
        if let Some(s) = line.strip_prefix("SUMMARY:") {
            summary = s.trim().to_string();
        } else if let Some(t) = line.strip_prefix("TAGS:") {
            tags = t.trim().to_string();
        }
    }

    if summary.is_empty() {
        summary = text.trim().to_string();
        tags = "session coding".to_string();
    }

    CompressionResult { summary, tags }
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
    let text = data
        .content
        .into_iter()
        .find(|b| b.block_type == "text")
        .and_then(|b| b.text)
        .ok_or_else(|| anyhow!("No text content in Anthropic response"))?;

    Ok(parse_response(&text))
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
    let text = data
        .choices
        .into_iter()
        .next()
        .and_then(|c| c.message.content)
        .ok_or_else(|| anyhow!("No content in OpenAI response"))?;

    Ok(parse_response(&text))
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
    let text = data
        .candidates
        .and_then(|c| c.into_iter().next())
        .and_then(|c| c.content.parts.into_iter().next())
        .and_then(|p| p.text)
        .ok_or_else(|| anyhow!("No content in Gemini response"))?;

    Ok(parse_response(&text))
}
