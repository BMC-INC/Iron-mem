//! Cross-encoder reranker (roadmap Wave 4): on-device ONNX reranking via
//! fastembed, as an alternative to the LLM reranker.
//!
//! The LLM reranker reorders by asking a generative model to emit a candidate
//! order; its scores are unstable run-to-run (the documented rerank-stage leak).
//! A cross-encoder scores each (query, candidate) pair directly with a small
//! ONNX model, giving stable, calibrated relevance — exactly what the funnel's
//! rerank-kept leak needs.
//!
//! Feature-gated on `local-onnx` (same as the embedder). When the model is
//! unavailable (feature off, not initialized, or a load failure), `rerank_order`
//! returns `None` and the caller falls back to the LLM reranker, so selecting the
//! cross-encoder can only help, never break retrieval.

#[cfg(feature = "local-onnx")]
use std::sync::OnceLock;

#[cfg(feature = "local-onnx")]
mod onnx {
    use anyhow::Result;

    fn model_for(name: &str) -> fastembed::RerankerModel {
        use fastembed::RerankerModel::*;
        match name.trim().to_ascii_lowercase().as_str() {
            "bge-reranker-base" => BGERerankerBase,
            "bge-reranker-v2-m3" | "bge-reranker-v2m3" => BGERerankerV2M3,
            "jina-reranker-v1-turbo-en" => JINARerankerV1TurboEn,
            "jina-reranker-v2-base-multilingual" => JINARerankerV2BaseMultiligual,
            _ => BGERerankerV2M3, // strongest general default
        }
    }

    /// Wraps `fastembed::TextRerank`. No `Mutex`: `rerank` takes `&self` and the
    /// ort `Session` is `Sync`, so concurrent requests rerank in parallel — a
    /// lock here would serialize every `/context?rerank=1` call into one lane.
    pub struct CrossEncoder {
        model: fastembed::TextRerank,
    }

    impl CrossEncoder {
        pub fn load(name: &str) -> Result<Self> {
            // Reuse the embedder's writable cache (a launchd agent runs from "/",
            // where fastembed's default ./.fastembed_cache is unwritable).
            let cache_dir = crate::config::ironmem_dir().join("fastembed_cache");
            let model = fastembed::TextRerank::try_new(
                fastembed::RerankInitOptions::new(model_for(name))
                    .with_cache_dir(cache_dir)
                    .with_show_download_progress(false),
            )?;
            Ok(Self { model })
        }

        /// Rank `docs` against `query`, returning document indices best-first.
        pub fn order(&self, query: &str, docs: &[String]) -> Result<Vec<usize>> {
            let refs: Vec<&str> = docs.iter().map(String::as_str).collect();
            let mut results = self.model.rerank(query, refs, false, None)?;
            results.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            Ok(results.into_iter().map(|r| r.index).collect())
        }
    }
}

#[cfg(feature = "local-onnx")]
static RERANKER: OnceLock<Option<onnx::CrossEncoder>> = OnceLock::new();

/// Load the cross-encoder once at startup. No-op without the `local-onnx`
/// feature or if already initialized. A load failure is logged and leaves the
/// reranker unset so callers fall back to the LLM reranker.
pub fn init(model_name: &str) {
    #[cfg(feature = "local-onnx")]
    {
        if RERANKER.get().is_some() {
            return;
        }
        let loaded = match onnx::CrossEncoder::load(model_name) {
            Ok(ce) => {
                tracing::info!("cross-encoder reranker loaded: {model_name}");
                Some(ce)
            }
            Err(e) => {
                tracing::warn!("cross-encoder load failed ({e}); LLM rerank will be used");
                None
            }
        };
        let _ = RERANKER.set(loaded);
    }
    #[cfg(not(feature = "local-onnx"))]
    {
        let _ = model_name;
    }
}

/// True if a cross-encoder is loaded and ready.
pub fn is_ready() -> bool {
    #[cfg(feature = "local-onnx")]
    {
        matches!(RERANKER.get(), Some(Some(_)))
    }
    #[cfg(not(feature = "local-onnx"))]
    {
        false
    }
}

/// Reranked order (indices into `docs`, best-first) or `None` when the
/// cross-encoder is unavailable — the caller then keeps the LLM reranker.
pub fn rerank_order(query: &str, docs: &[String]) -> Option<Vec<usize>> {
    #[cfg(feature = "local-onnx")]
    {
        let ce = RERANKER.get()?.as_ref()?;
        match ce.order(query, docs) {
            Ok(order) => Some(order),
            Err(e) => {
                tracing::warn!("cross-encoder rerank failed ({e}); falling back to LLM rerank");
                None
            }
        }
    }
    #[cfg(not(feature = "local-onnx"))]
    {
        let _ = (query, docs);
        None
    }
}

#[cfg(test)]
mod tests {
    /// Without `init`, no cross-encoder is loaded, so `rerank_order` returns None
    /// and callers keep the LLM reranker. This is the safety contract the rerank
    /// dispatch relies on.
    #[test]
    fn uninitialized_falls_back() {
        assert!(super::rerank_order("when did X happen", &["a fact".to_string()]).is_none());
    }
}
