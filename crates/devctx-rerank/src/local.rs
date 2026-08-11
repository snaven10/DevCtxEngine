//! fastembed-backed cross-encoder reranker.
//!
//! fastembed has no `ms-marco-MiniLM-L-12-v2` (the legacy FlashRank default), so
//! DevCtxEngine uses fastembed's BGE rerankers instead; `bge-v2-m3` is multilingual and
//! pairs well with multilingual embedders (e.g. Granite).

use fastembed::{RerankInitOptions, RerankerModel, TextRerank};

use crate::error::{RerankError, Result};
use crate::provider::{Ranked, Reranker};

/// Default reranker model key.
pub const DEFAULT_MODEL: &str = "bge-base";

/// A fastembed cross-encoder reranker.
pub struct LocalReranker {
    model: TextRerank,
    name: String,
}

impl LocalReranker {
    /// Load the reranker named by `key` (downloads/caches on first use).
    pub fn load(key: &str) -> Result<Self> {
        let model = model_for(key)?;
        let mut opts = RerankInitOptions::new(model).with_show_download_progress(false);
        // Shared with the embedding models: see `devctx_core::dirs`.
        if let Some(cache) = devctx_core::dirs::model_cache_dir() {
            opts = opts.with_cache_dir(cache);
        }
        let text_rerank =
            TextRerank::try_new(opts).map_err(|e| RerankError::Backend(e.to_string()))?;
        Ok(Self {
            model: text_rerank,
            name: key.to_string(),
        })
    }
}

impl Reranker for LocalReranker {
    fn rerank(&self, query: &str, candidates: &[String], top_k: usize) -> Result<Vec<Ranked>> {
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        let docs: Vec<&str> = candidates.iter().map(String::as_str).collect();
        let mut results = self
            .model
            .rerank(query, docs, false, None)
            .map_err(|e| RerankError::Backend(e.to_string()))?;
        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(top_k);
        Ok(results
            .into_iter()
            .map(|r| Ranked {
                index: r.index,
                score: r.score,
            })
            .collect())
    }

    fn name(&self) -> &str {
        &self.name
    }
}

fn model_for(key: &str) -> Result<RerankerModel> {
    Ok(match key {
        "bge-base" | "" => RerankerModel::BGERerankerBase,
        "bge-v2-m3" => RerankerModel::BGERerankerV2M3,
        "jina-turbo" => RerankerModel::JINARerankerV1TurboEn,
        "jina-v2" => RerankerModel::JINARerankerV2BaseMultiligual,
        other => return Err(RerankError::UnknownModel(other.to_string())),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_models_map() {
        for k in ["bge-base", "bge-v2-m3", "jina-turbo", "jina-v2", ""] {
            assert!(model_for(k).is_ok(), "unmapped {k}");
        }
        assert!(matches!(
            model_for("nope"),
            Err(RerankError::UnknownModel(_))
        ));
    }

    /// Real reranking: downloads a BGE reranker and checks ordering.
    /// Ignored by default (network + model download).
    #[test]
    #[ignore = "downloads a model from HuggingFace"]
    fn reranks_by_relevance() {
        let r = LocalReranker::load("bge-base").unwrap();
        let cands: Vec<String> = vec![
            "the cat sat on the mat".into(),
            "how to connect to a postgres database".into(),
            "a recipe for chocolate cake".into(),
        ];
        let out = r.rerank("database connection", &cands, 3).unwrap();
        assert_eq!(out.len(), 3);
        // The database sentence (index 1) should rank first.
        assert_eq!(out[0].index, 1);
    }
}
