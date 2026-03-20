use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

use crate::db::Observation;

#[derive(Serialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(Serialize)]
struct CompressRequest {
    model: String,
    max_tokens: u32,
    messages: Vec<Message>,
}

#[derive(Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    block_type: String,
    text: Option<String>,
}

#[derive(Deserialize)]
struct CompressResponse {
    content: Vec<ContentBlock>,
}

#[derive(Debug)]
pub struct CompressionResult {
    pub summary: String,
    pub tags: String,
}

pub async fn compress_session(
    observations: &[Observation],
    model: &str,
    api_key: &str,
) -> Result<CompressionResult> {
    if observations.is_empty() {
        return Err(anyhow!("No observations to compress"));
    }

    let prompt = build_prompt(observations);

    let client = reqwest::Client::new();
    let req = CompressRequest {
        model: model.to_string(),
        max_tokens: 1024,
        messages: vec![Message {
            role: "user".to_string(),
            content: prompt,
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
        return Err(anyhow!("Claude API error {}: {}", status, body));
    }

    let data: CompressResponse = resp.json().await?;

    let text = data
        .content
        .into_iter()
        .find(|b| b.block_type == "text")
        .and_then(|b| b.text)
        .ok_or_else(|| anyhow!("No text content in Claude response"))?;

    parse_compression_response(&text)
}

fn build_prompt(observations: &[Observation]) -> String {
    let mut lines = vec![
        "You are a technical memory system. Analyze these tool calls from a coding session and produce a concise memory entry.".to_string(),
        String::new(),
        "TOOL CALLS:".to_string(),
    ];

    for (i, obs) in observations.iter().enumerate() {
        lines.push(format!("{}. Tool: {}", i + 1, obs.tool));
        if let Some(input) = &obs.input {
            // Truncate long inputs in the prompt
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
    lines.push("TAGS: [8-12 space-separated lowercase keywords: technologies, file names, concepts]".to_string());

    lines.join("\n")
}

fn parse_compression_response(text: &str) -> Result<CompressionResult> {
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
        // Fallback: use the whole response as summary
        summary = text.trim().to_string();
        tags = "session coding".to_string();
    }

    Ok(CompressionResult { summary, tags })
}
