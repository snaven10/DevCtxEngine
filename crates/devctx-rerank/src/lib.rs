//! `devctx-rerank` — cross-encoder reranking for DevCtxEngine.
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
    /// Model key (`bge-base` default, `bge-v2-m3` multilingual, `jina-*`), or
    /// `custom` to load `model_dir`.
    pub model: String,
    /// How many candidates to show the cross-encoder (the pool it reorders).
    pub pool: usize,
    /// Directory of a user-supplied cross-encoder (ONNX + tokenizer files).
    pub model_dir: Option<std::path::PathBuf>,
}

impl Default for RerankSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            pool: 100,
            model: default_model().to_string(),
            model_dir: None,
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
    create_local(
        &settings.model,
        settings.model_dir.as_deref(),
        settings.pool,
    )
}

#[cfg(feature = "local")]
fn create_local(
    model: &str,
    model_dir: Option<&std::path::Path>,
    pool: usize,
) -> Result<Box<dyn Reranker>> {
    Ok(Box::new(
        local::LocalReranker::load(model, model_dir)?.with_pool(pool),
    ))
}

#[cfg(not(feature = "local"))]
fn create_local(
    _model: &str,
    _model_dir: Option<&std::path::Path>,
    _pool: usize,
) -> Result<Box<dyn Reranker>> {
    Ok(Box::new(NoopReranker))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_yields_noop() {
        let r = create_reranker(&RerankSettings {
            pool: 100,
            model_dir: None,
            enabled: false,
            model: "bge-base".into(),
        })
        .unwrap();
        assert_eq!(r.name(), "noop");
    }
}
