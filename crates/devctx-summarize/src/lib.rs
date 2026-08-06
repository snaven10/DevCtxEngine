//! `devctx-summarize` — content summarization for DevCtxEngine.
//!
//! Providers: `extractive` (default; ranks sentences with the embedding model,
//! offline, $0), `openai` (abstractive, cloud), and `noop` (truncate). A local
//! abstractive backend (flan-t5) is a follow-up. A privacy guard (`require_local`)
//! blocks non-local providers by default. See `docs/rust-rewrite-plan.md` §7.

pub mod error;
mod extractive;
mod openai;
mod provider;

#[cfg(feature = "flan-t5")]
mod flan_t5;

use std::sync::Arc;

use devctx_embed::EmbeddingProvider;

pub use error::{Result, SummarizeError};
pub use extractive::ExtractiveSummarizer;
pub use openai::OpenAiSummarizer;
pub use provider::{NoopSummarizer, Summarizer};

/// Settings for constructing a summarizer.
#[derive(Debug, Clone)]
pub struct SummarizeSettings {
    /// Provider: `extractive` (default), `openai`, `noop`.
    pub provider: String,
    /// Block non-local providers when set (privacy guard).
    pub require_local: bool,
    /// Target summary length in tokens.
    pub target_tokens: usize,
    /// Model id for API providers.
    pub model: String,
    /// API key (falls back to `OPENAI_API_KEY`).
    pub api_key: Option<String>,
}

impl Default for SummarizeSettings {
    fn default() -> Self {
        Self {
            provider: "extractive".to_string(),
            require_local: true,
            target_tokens: 200,
            model: "gpt-4o-mini".to_string(),
            api_key: None,
        }
    }
}

/// Construct a summarizer. The extractive provider needs `embedder`.
pub fn create_summarizer(
    settings: &SummarizeSettings,
    embedder: Option<Arc<dyn EmbeddingProvider>>,
) -> Result<Box<dyn Summarizer>> {
    match settings.provider.as_str() {
        "noop" => Ok(Box::new(NoopSummarizer)),
        "extractive" | "" => {
            let emb = embedder.ok_or(SummarizeError::MissingEmbedder)?;
            Ok(Box::new(ExtractiveSummarizer::new(emb)))
        }
        "openai" => {
            if settings.require_local {
                return Err(SummarizeError::NonLocalBlocked("openai".into()));
            }
            let key = settings
                .api_key
                .clone()
                .or_else(|| std::env::var("OPENAI_API_KEY").ok())
                .filter(|k| !k.is_empty())
                .ok_or_else(|| SummarizeError::MissingConfig("OPENAI_API_KEY".into()))?;
            Ok(Box::new(OpenAiSummarizer::new(settings.model.clone(), key)))
        }
        "flan-t5" => create_flan_t5(),
        other => Err(SummarizeError::UnknownProvider(other.to_string())),
    }
}

#[cfg(feature = "flan-t5")]
fn create_flan_t5() -> Result<Box<dyn Summarizer>> {
    Ok(Box::new(flan_t5::FlanT5Summarizer::load()?))
}

#[cfg(not(feature = "flan-t5"))]
fn create_flan_t5() -> Result<Box<dyn Summarizer>> {
    Err(SummarizeError::UnknownProvider(
        "flan-t5 (not compiled in; enable the `flan-t5` feature)".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_selected_and_summarizes() {
        let s = create_summarizer(
            &SummarizeSettings {
                provider: "noop".into(),
                ..Default::default()
            },
            None,
        )
        .unwrap();
        assert_eq!(s.name(), "noop");
    }

    #[test]
    fn openai_blocked_when_require_local() {
        assert!(matches!(
            create_summarizer(
                &SummarizeSettings {
                    provider: "openai".into(),
                    require_local: true,
                    ..Default::default()
                },
                None,
            ),
            Err(SummarizeError::NonLocalBlocked(_))
        ));
    }

    #[test]
    fn extractive_needs_embedder() {
        assert!(matches!(
            create_summarizer(&SummarizeSettings::default(), None),
            Err(SummarizeError::MissingEmbedder)
        ));
    }

    /// Real local abstractive summary: downloads flan-t5-small and generates.
    /// Ignored by default (network + model download).
    #[cfg(feature = "flan-t5")]
    #[test]
    #[ignore = "downloads flan-t5-small from HuggingFace"]
    fn flan_t5_summarizes() {
        let s = create_summarizer(
            &SummarizeSettings {
                provider: "flan-t5".into(),
                require_local: true,
                ..Default::default()
            },
            None,
        )
        .unwrap();
        let summary = s
            .summarize(
                "The quick brown fox jumps over the lazy dog. \
                 Paris is the capital city of France. \
                 Rust is a systems programming language focused on safety.",
                None,
                40,
            )
            .unwrap();
        assert!(!summary.is_empty(), "empty summary");
    }

    #[test]
    fn unknown_provider_errors() {
        assert!(matches!(
            create_summarizer(
                &SummarizeSettings {
                    provider: "banana".into(),
                    ..Default::default()
                },
                None,
            ),
            Err(SummarizeError::UnknownProvider(_))
        ));
    }
}
