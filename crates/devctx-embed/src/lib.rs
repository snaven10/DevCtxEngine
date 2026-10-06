//! `devctx-embed` — embedding providers for DevCtxEngine.
//!
//! Local models run via `fastembed`/`ort` (behind the default `local` feature);
//! OpenAI/Voyage/custom run over HTTP. All providers implement
//! [`EmbeddingProvider`] and expose a fixed [`dimension`](EmbeddingProvider::dimension).
//! See `docs/architecture-spec.md` §5.

use std::path::PathBuf;

use devctx_core::config::Device;

pub mod api;
pub mod error;
pub mod provider;
pub mod registry;

#[cfg(feature = "local")]
pub mod local;

pub use error::{EmbedError, Result};
pub use provider::{l2_normalize, EmbeddingProvider, LazyEmbedder};

/// Resolved settings for constructing an embedding provider.
#[derive(Debug, Clone, Default)]
pub struct EmbedSettings {
    /// `local` | `openai` | `voyage` | `custom`.
    pub provider: String,
    /// Model key within the provider.
    pub model: String,
    /// Force offline (no network model download).
    pub offline: bool,
    /// API key (API providers) — falls back to provider-specific env vars.
    pub api_key: Option<String>,
    /// Base endpoint for the `custom` provider.
    pub endpoint: Option<String>,
    /// Vector dimension for the `custom` provider (unknown to us otherwise).
    pub custom_dimension: Option<usize>,
    /// Directory holding a user-defined ONNX model + tokenizer (e.g. Granite).
    pub model_dir: Option<PathBuf>,
    /// Where the local model runs (`cpu` | `cuda`).
    pub device: Device,
}

impl EmbedSettings {
    /// Build settings from the project config, reading credentials/paths from
    /// the environment where appropriate.
    pub fn from_config(cfg: &devctx_core::config::Embeddings) -> Self {
        use devctx_core::config::Offline;
        let offline = matches!(cfg.offline, Offline::True);
        Self {
            provider: cfg.provider.clone(),
            model: cfg.model.clone(),
            offline,
            api_key: None,
            endpoint: std::env::var("DEVCTX_EMBED_ENDPOINT").ok(),
            custom_dimension: std::env::var("DEVCTX_EMBED_DIMENSION")
                .ok()
                .and_then(|s| s.parse().ok()),
            // Config `model_dir` wins; otherwise fall back to the env var.
            model_dir: if cfg.model_dir.is_empty() {
                std::env::var("DEVCTX_MODEL_DIR").ok().map(PathBuf::from)
            } else {
                Some(PathBuf::from(&cfg.model_dir))
            },
            // `DEVCTX_DEVICE` overrides the config.
            device: cfg.device.with_env_override(),
        }
    }
}

/// The vector dimension for a provider/model pair, resolved from the registry
/// **without loading the model**.
///
/// This is what lets a store be opened (its `FLOAT[dim]` column is fixed at
/// creation) before deciding whether the embedder is even needed — the lazy-load
/// path every server takes. The `custom` provider has no registry entry, so its
/// dimension comes from `DEVCTX_EMBED_DIMENSION`.
pub fn dimension_for(provider: &str, model: &str) -> usize {
    match provider {
        "openai" => registry::openai_dimension(model).unwrap_or(1536),
        "voyage" => registry::voyage_dimension(model).unwrap_or(1024),
        "custom" => std::env::var("DEVCTX_EMBED_DIMENSION")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(384),
        _ => registry::find_local(model)
            .map(|m| m.dimension)
            .unwrap_or(384),
    }
}

/// The engine that runs local models. Whatever replaces it (the `ort` engine of
/// PLAN-010) must change this string until it is shown to produce the same
/// vectors bit for bit: a vector reused across engines would be a silent lie.
const LOCAL_ENGINE: &str = "fastembed-4/ort-2.0.0-rc.9";

/// Everything that decides the numbers a model produces for a text: provider,
/// model, width, engine, the per-text character cap and the normalization.
///
/// Two runs with equal fingerprints produce interchangeable vectors; vectors may
/// only be reused across runs when the fingerprints are equal. Model name and
/// dimension alone are not enough: the same name behind another engine, another
/// provider or another truncation gives different vectors.
pub fn embedding_fingerprint(s: &EmbedSettings, dimension: usize) -> String {
    let provider = if s.provider.is_empty() {
        "local"
    } else {
        s.provider.as_str()
    };
    let model = if s.model.is_empty() && provider == "local" {
        registry::DEFAULT_LOCAL_MODEL
    } else {
        s.model.as_str()
    };
    match provider {
        "local" => {
            let max_chars = std::env::var("DEVCTX_EMBED_MAX_CHARS")
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .filter(|&n| n > 0)
                .unwrap_or(4096);
            format!("local|{model}|{dimension}|{LOCAL_ENGINE}|max_chars={max_chars}|l2")
        }
        other => format!("{other}|{model}|{dimension}|http|l2"),
    }
}

/// Construct an embedding provider from resolved settings.
pub fn create_provider(s: &EmbedSettings) -> Result<Box<dyn EmbeddingProvider>> {
    match s.provider.as_str() {
        "local" | "" => create_local(s),
        "openai" => {
            let key = api_key(s, "OPENAI_API_KEY")?;
            let model_id = registry::openai_model_id(&s.model).to_string();
            let dim = registry::openai_dimension(&s.model)
                .ok_or_else(|| EmbedError::UnknownModel(s.model.clone(), "openai".into()))?;
            Ok(Box::new(api::OpenAiProvider::new(model_id, dim, key)))
        }
        "voyage" => {
            let key = api_key(s, "VOYAGE_API_KEY")?;
            let model_id = registry::voyage_model_id(&s.model).to_string();
            let dim = registry::voyage_dimension(&s.model)
                .ok_or_else(|| EmbedError::UnknownModel(s.model.clone(), "voyage".into()))?;
            Ok(Box::new(api::VoyageProvider::new(model_id, dim, key)))
        }
        "custom" => {
            let endpoint = s
                .endpoint
                .clone()
                .ok_or_else(|| EmbedError::MissingConfig("custom endpoint".into()))?;
            let dim = s
                .custom_dimension
                .ok_or_else(|| EmbedError::MissingConfig("custom dimension".into()))?;
            let key = s.api_key.clone().unwrap_or_default();
            Ok(Box::new(api::CustomProvider::new(
                endpoint, &s.model, dim, key,
            )))
        }
        other => Err(EmbedError::UnknownProvider(other.to_string())),
    }
}

fn api_key(s: &EmbedSettings, env: &str) -> Result<String> {
    s.api_key
        .clone()
        .or_else(|| std::env::var(env).ok())
        .filter(|k| !k.is_empty())
        .ok_or_else(|| EmbedError::MissingConfig(env.to_string()))
}

#[cfg(feature = "local")]
fn create_local(s: &EmbedSettings) -> Result<Box<dyn EmbeddingProvider>> {
    Ok(Box::new(local::LocalProvider::load(s)?))
}

#[cfg(not(feature = "local"))]
fn create_local(_s: &EmbedSettings) -> Result<Box<dyn EmbeddingProvider>> {
    Err(EmbedError::LocalDisabled)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fingerprint_changes_with_anything_that_changes_the_vectors() {
        let base = EmbedSettings {
            provider: "local".into(),
            model: "minilm-l6".into(),
            ..Default::default()
        };
        let fp = embedding_fingerprint(&base, 384);
        assert_eq!(fp, embedding_fingerprint(&base, 384));
        assert_ne!(fp, embedding_fingerprint(&base, 768), "width");
        let other = EmbedSettings {
            model: "bge-small".into(),
            ..base.clone()
        };
        assert_ne!(fp, embedding_fingerprint(&other, 384), "model");
        let http = EmbedSettings {
            provider: "openai".into(),
            ..base.clone()
        };
        assert_ne!(fp, embedding_fingerprint(&http, 384), "provider");
        assert!(fp.contains(LOCAL_ENGINE) && fp.contains("max_chars="));
    }

    #[test]
    fn openai_provider_selected_with_key() {
        let s = EmbedSettings {
            provider: "openai".into(),
            model: "small".into(),
            api_key: Some("sk-test".into()),
            ..Default::default()
        };
        let p = create_provider(&s).unwrap();
        assert_eq!(p.dimension(), 1536);
        assert_eq!(p.model_name(), "text-embedding-3-small");
    }

    #[test]
    fn openai_without_key_errors() {
        let s = EmbedSettings {
            provider: "openai".into(),
            model: "small".into(),
            ..Default::default()
        };
        // Ensure no ambient key leaks in.
        std::env::remove_var("OPENAI_API_KEY");
        assert!(matches!(
            create_provider(&s),
            Err(EmbedError::MissingConfig(_))
        ));
    }

    #[test]
    fn unknown_provider_errors() {
        let s = EmbedSettings {
            provider: "banana".into(),
            ..Default::default()
        };
        assert!(matches!(
            create_provider(&s),
            Err(EmbedError::UnknownProvider(_))
        ));
    }

    #[test]
    fn custom_requires_endpoint_and_dimension() {
        let s = EmbedSettings {
            provider: "custom".into(),
            model: "my-model".into(),
            ..Default::default()
        };
        assert!(matches!(
            create_provider(&s),
            Err(EmbedError::MissingConfig(_))
        ));
    }
}
