//! `devai-rerank` — cross-encoder reranking for DevAI.
//!
//! A [`Reranker`] reorders search candidates by relevance to the query. The
//! local backend (default `local` feature) uses fastembed BGE cross-encoders;
//! when disabled or turned off it falls back to [`NoopReranker`] (identity order),
//! mirroring the legacy FlashRank→noop degradation. See rewrite plan §6.

pub mod error;
pub mod provider;

#[cfg(feature = "local")]
pub mod local;

pub use error::{RerankError, Result};
pub use provider::{NoopReranker, Ranked, Reranker};

/// Settings for constructing a reranker.
#[derive(Debug, Clone)]
pub struct RerankSettings {
    /// Whether reranking is enabled (else a no-op reranker is used).
    pub enabled: bool,
    /// Model key (`bge-base` default, `bge-v2-m3` multilingual, `jina-*`).
    pub model: String,
}

impl Default for RerankSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            model: default_model().to_string(),
        }
    }
}

#[cfg(feature = "local")]
fn default_model() -> &'static str {
    local::DEFAULT_MODEL
}

#[cfg(not(feature = "local"))]
fn default_model() -> &'static str {
    "noop"
}

/// Construct a reranker from settings. Disabled settings, or a build without the
/// `local` feature, yield a [`NoopReranker`].
pub fn create_reranker(settings: &RerankSettings) -> Result<Box<dyn Reranker>> {
    if !settings.enabled {
        return Ok(Box::new(NoopReranker));
    }
    create_local(&settings.model)
}

#[cfg(feature = "local")]
fn create_local(model: &str) -> Result<Box<dyn Reranker>> {
    Ok(Box::new(local::LocalReranker::load(model)?))
}

#[cfg(not(feature = "local"))]
fn create_local(_model: &str) -> Result<Box<dyn Reranker>> {
    Ok(Box::new(NoopReranker))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_yields_noop() {
        let r = create_reranker(&RerankSettings {
            enabled: false,
            model: "bge-base".into(),
        })
        .unwrap();
        assert_eq!(r.name(), "noop");
    }
}
