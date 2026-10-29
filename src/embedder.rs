//! Pluggable embedding layer. Local-first / no-egress by default:
//! Ollama, then in-process ONNX (feature), then keyword search.
//! `auto` calls OpenAI only when cloud fallback is explicitly enabled.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use std::sync::Arc;

use crate::config::Config;
use crate::embedding_codec::normalize;
use crate::provider::{resolve_api_key, Provider};

/// "text → unit-normalized vector".
#[async_trait]
pub trait Embedder: Send + Sync {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
    /// Model identity stored alongside each vector (so we never mix models).
    fn id(&self) -> &str;
    fn dim(&self) -> usize;
}

/// Known embedding dimensions for the default models (avoids a probe call).
fn known_dim(model: &str) -> Option<usize> {
    match model {
        "nomic-embed-text" => Some(768),
        "mxbai-embed-large" => Some(1024),
        "all-minilm" => Some(384),
        "bge-small-en-v1.5" => Some(384),
        "text-embedding-3-small" => Some(1536),
        "text-embedding-3-large" => Some(3072),
        "text-embedding-004" => Some(768),
        _ => None,
    }
}

// ── Ollama (local, no egress — the default) ─────────────────────────

pub struct OllamaEmbedder {
    client: reqwest::Client,
    base: String,
    model: String,
    dim: usize,
}

impl OllamaEmbedder {
    pub fn new(base: String, model: String, dim: usize) -> Self {
        Self {
            client: reqwest::Client::new(),
            base,
            model,
            dim,
        }
    }

    pub async fn reachable(base: &str) -> bool {
        reqwest::Client::new()
            .get(format!("{base}/api/tags"))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }
}

#[async_trait]
impl Embedder for OllamaEmbedder {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        #[derive(serde::Serialize)]
        struct Req<'a> {
            model: &'a str,
            input: &'a [String],
        }
        #[derive(serde::Deserialize)]
        struct Resp {
            embeddings: Vec<Vec<f32>>,
        }
        let resp = self
            .client
            .post(format!("{}/api/embed", self.base))
            .json(&Req {
                model: &self.model,
                input: texts,
            })
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(anyhow!(
                "Ollama embed error {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }
        let data: Resp = resp.json().await?;
        Ok(data.embeddings.iter().map(|v| normalize(v)).collect())
    }
    fn id(&self) -> &str {
        &self.model
    }
    fn dim(&self) -> usize {
        self.dim
    }
}

// ── API embedders (OpenAI / Google) — opt-in, sends text out ────────

#[derive(Clone, Copy)]
pub enum ApiKind {
    OpenAi,
    Google,
}

pub struct ApiEmbedder {
    client: reqwest::Client,
    kind: ApiKind,
    model: String,
    api_key: String,
    dim: usize,
}

impl ApiEmbedder {
    pub fn new(kind: ApiKind, model: String, api_key: String, dim: usize) -> Self {
        Self {
            client: reqwest::Client::new(),
            kind,
            model,
            api_key,
            dim,
        }
    }
}

#[async_trait]
impl Embedder for ApiEmbedder {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        match self.kind {
            ApiKind::OpenAi => {
                #[derive(serde::Serialize)]
                struct Req<'a> {
                    model: &'a str,
                    input: &'a [String],
                }
                #[derive(serde::Deserialize)]
                struct Item {
                    embedding: Vec<f32>,
                }
                #[derive(serde::Deserialize)]
                struct Resp {
                    data: Vec<Item>,
                }
                let resp = self
                    .client
                    .post("https://api.openai.com/v1/embeddings")
                    .header("Authorization", format!("Bearer {}", self.api_key))
                    .json(&Req {
                        model: &self.model,
                        input: texts,
                    })
                    .send()
                    .await?;
                if !resp.status().is_success() {
                    return Err(anyhow!(
                        "OpenAI embed error {}: {}",
                        resp.status(),
                        resp.text().await.unwrap_or_default()
                    ));
                }
                let data: Resp = resp.json().await?;
                Ok(data.data.iter().map(|i| normalize(&i.embedding)).collect())
            }
            ApiKind::Google => {
                // Per-text embedContent (keeps payloads simple and order exact).
                #[derive(serde::Serialize)]
                struct Part<'a> {
                    text: &'a str,
                }
                #[derive(serde::Serialize)]
                struct Content<'a> {
                    parts: Vec<Part<'a>>,
                }
                #[derive(serde::Serialize)]
                struct Req<'a> {
                    content: Content<'a>,
                }
                #[derive(serde::Deserialize)]
                struct Values {
                    values: Vec<f32>,
                }
                #[derive(serde::Deserialize)]
                struct Resp {
                    embedding: Values,
                }
                let client = crate::provider::google_api_client()?;
                let mut out = Vec::with_capacity(texts.len());
                for t in texts {
                    let resp = crate::provider::google_request(
                        &client,
                        &self.model,
                        "embedContent",
                        &self.api_key,
                    )?
                    .json(&Req {
                        content: Content {
                            parts: vec![Part { text: t }],
                        },
                    })
                    .send()
                    .await?;
                    if !resp.status().is_success() {
                        return Err(anyhow!(
                            "Google embed error {}: {}",
                            resp.status(),
                            resp.text().await.unwrap_or_default()
                        ));
                    }
                    let data: Resp = resp.json().await?;
                    out.push(normalize(&data.embedding.values));
                }
                Ok(out)
            }
        }
    }
    fn id(&self) -> &str {
        &self.model
    }
    fn dim(&self) -> usize {
        self.dim
    }
}

// ── In-process ONNX (opt-in build, no egress, self-contained) ───────

#[cfg(feature = "local-onnx")]
pub struct OnnxEmbedder {
    model: std::sync::Mutex<fastembed::TextEmbedding>,
    id: String,
    dim: usize,
}

#[cfg(feature = "local-onnx")]
impl OnnxEmbedder {
    pub fn new() -> Result<Self> {
        // Pin the model cache under ~/.ironmem so it loads regardless of the
        // process cwd. A launchd agent runs from "/", where the fastembed default
        // (./.fastembed_cache) is unwritable — which silently downgraded retrieval
        // to keyword-only. Seeded once, the model then loads fully offline (no egress).
        let cache_dir = crate::config::ironmem_dir().join("fastembed_cache");
        let model = fastembed::TextEmbedding::try_new(
            fastembed::InitOptions::new(fastembed::EmbeddingModel::BGESmallENV15)
                .with_cache_dir(cache_dir)
                .with_show_download_progress(false),
        )?;
        Ok(Self {
            model: std::sync::Mutex::new(model),
            id: "bge-small-en-v1.5".to_string(),
            dim: 384,
        })
    }
}

#[cfg(feature = "local-onnx")]
#[async_trait]
impl Embedder for OnnxEmbedder {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let docs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
        let raw = {
            let m = self
                .model
                .lock()
                .map_err(|_| anyhow!("onnx model poisoned"))?;
            m.embed(docs, None)?
        };
        Ok(raw.iter().map(|v| normalize(v)).collect())
    }
    fn id(&self) -> &str {
        &self.id
    }
    fn dim(&self) -> usize {
        self.dim
    }
}

// ── Resolution chain ────────────────────────────────────────────────

async fn ollama_dim(base: &str, model: &str) -> Option<usize> {
    if let Some(d) = known_dim(model) {
        return Some(d);
    }
    let tmp = OllamaEmbedder::new(base.to_string(), model.to_string(), 1);
    tmp.embed(&["dimension probe".to_string()])
        .await
        .ok()?
        .first()
        .map(|v| v.len())
}

fn build_api(
    kind: ApiKind,
    default_model: &str,
    default_dim: usize,
    cfg: &Config,
) -> Option<Arc<dyn Embedder>> {
    let provider = match kind {
        ApiKind::OpenAi => Provider::Openai,
        ApiKind::Google => Provider::Google,
    };
    let key = resolve_api_key(provider).ok()?;
    let model = cfg
        .embedding
        .model
        .clone()
        .unwrap_or_else(|| default_model.to_string());
    let dim = known_dim(&model).unwrap_or(default_dim);
    Some(Arc::new(ApiEmbedder::new(kind, model, key, dim)))
}

/// Resolve the active embedder per config. `None` ⇒ FTS-only (no semantic path).
pub async fn resolve_embedder(cfg: &Config) -> Option<Arc<dyn Embedder>> {
    let ec = &cfg.embedding;
    let result: Option<Arc<dyn Embedder>> = match ec.provider.as_str() {
        "none" => None,
        "ollama" => {
            let model = ec
                .model
                .clone()
                .unwrap_or_else(|| "nomic-embed-text".to_string());
            let dim = ollama_dim(&ec.ollama_url, &model).await?;
            Some(Arc::new(OllamaEmbedder::new(
                ec.ollama_url.clone(),
                model,
                dim,
            )))
        }
        "openai" => {
            let embedder = build_api(ApiKind::OpenAi, "text-embedding-3-small", 1536, cfg);
            if embedder.is_some() {
                tracing::warn!(
                    "Embedder: provider is openai. Memory text will be sent to api.openai.com."
                );
            }
            embedder
        }
        "google" => {
            let embedder = build_api(ApiKind::Google, "text-embedding-004", 768, cfg);
            if embedder.is_some() {
                tracing::warn!(
                    "Embedder: provider is google. Memory text will be sent to generativelanguage.googleapis.com."
                );
            }
            embedder
        }
        "onnx" => build_onnx(),
        _ => {
            // "auto": prefer local/no-egress. Cloud is opt-in.
            let model = ec
                .model
                .clone()
                .unwrap_or_else(|| "nomic-embed-text".to_string());
            if OllamaEmbedder::reachable(&ec.ollama_url).await {
                if let Some(dim) = ollama_dim(&ec.ollama_url, &model).await {
                    Some(Arc::new(OllamaEmbedder::new(
                        ec.ollama_url.clone(),
                        model,
                        dim,
                    )))
                } else {
                    None
                }
            } else if let Some(onnx) = build_onnx() {
                Some(onnx)
            } else if select_auto_cloud(
                cfg.embedding_cloud_fallback_enabled(),
                resolve_api_key(Provider::Openai).is_ok(),
            ) {
                build_api(ApiKind::OpenAi, "text-embedding-3-small", 1536, cfg)
            } else {
                None
            }
        }
    };

    match &result {
        Some(e) => tracing::info!("Embedder: {} (dim {})", e.id(), e.dim()),
        None => tracing::info!("Embedder: none (keyword/FTS-only retrieval)"),
    }
    result
}

#[cfg(feature = "local-onnx")]
fn build_onnx() -> Option<Arc<dyn Embedder>> {
    match OnnxEmbedder::new() {
        Ok(e) => Some(Arc::new(e)),
        Err(e) => {
            tracing::warn!("ONNX embedder unavailable: {}", e);
            None
        }
    }
}

#[cfg(not(feature = "local-onnx"))]
fn build_onnx() -> Option<Arc<dyn Embedder>> {
    None
}

/// Whether `auto` should use OpenAI after local options are exhausted.
/// Logs the choice. A present API key is not enough: `allow` must be true.
pub(crate) fn select_auto_cloud(allow: bool, openai_key_present: bool) -> bool {
    if allow && openai_key_present {
        tracing::warn!(
            "Embedder: no local model is available. Cloud fallback is enabled, so memory text will be sent to OpenAI (embedding.allow_cloud_fallback or IRONMEM_EMBED_ALLOW_CLOUD). Set embedding.provider to \"none\" to keep text on this machine."
        );
        return true;
    }
    if allow {
        tracing::info!(
            "Embedder: cloud fallback is enabled, but OPENAI_API_KEY is not set. Using keyword search only."
        );
        return false;
    }
    if openai_key_present {
        tracing::warn!(
            "Embedder: OPENAI_API_KEY is set, but auto will not send memory text to OpenAI. Using keyword search only. Opt in with embedding.allow_cloud_fallback true or IRONMEM_EMBED_ALLOW_CLOUD=1, or set embedding.provider to \"openai\"."
        );
    } else {
        tracing::info!("Embedder: no local model is available. Using keyword search only.");
    }
    false
}

// ── Test double ─────────────────────────────────────────────────────

#[cfg(test)]
pub struct FakeEmbedder {
    dim: usize,
}

#[cfg(test)]
impl FakeEmbedder {
    pub fn new(dim: usize) -> Self {
        Self { dim }
    }
}

#[cfg(test)]
#[async_trait]
impl Embedder for FakeEmbedder {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|t| {
                let mut v = vec![0.0f32; self.dim];
                for (i, b) in t.bytes().enumerate() {
                    v[i % self.dim] += b as f32;
                }
                normalize(&v)
            })
            .collect())
    }
    fn id(&self) -> &str {
        "fake"
    }
    fn dim(&self) -> usize {
        self.dim
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fake_embedder_is_deterministic_and_normalized() {
        let e = FakeEmbedder::new(8);
        let a = e.embed(&["hello".into()]).await.unwrap();
        let b = e.embed(&["hello".into()]).await.unwrap();
        assert_eq!(a, b);
        assert_eq!(a[0].len(), 8);
        let mag: f32 = a[0].iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((mag - 1.0).abs() < 1e-5);
    }

    #[tokio::test]
    async fn resolve_embedder_none_returns_none() {
        let mut cfg = Config::default();
        cfg.embedding.provider = "none".to_string();
        assert!(resolve_embedder(&cfg).await.is_none());
    }

    #[test]
    fn auto_cloud_fallback_is_opt_in() {
        assert!(!select_auto_cloud(false, false));
        assert!(!select_auto_cloud(false, true));
        assert!(!select_auto_cloud(true, false));
        assert!(select_auto_cloud(true, true));
    }
}
