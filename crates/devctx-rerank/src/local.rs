//! fastembed-backed cross-encoder reranker.
//!
//! fastembed ships four rerankers and none of them is lightweight: the
//! `ms-marco-MiniLM-L-12-v2` class of cross-encoder is absent entirely. The
//! built-ins are all heavyweight — `bge-reranker-base` is an XLM-RoBERTa whose
//! 250k-token vocabulary alone puts it past a gigabyte on disk — and reranking
//! is already the most expensive stage of a search by two orders of magnitude.
//!
//! So a user-supplied ONNX is a first-class option here, exactly as it is for
//! embeddings: set `reranking.model_dir` and an `ms-marco`-class model can be
//! used instead.

use std::path::Path;

use fastembed::{
    RerankInitOptions, RerankInitOptionsUserDefined, RerankerModel, TextRerank, TokenizerFiles,
    UserDefinedRerankingModel,
};

use crate::error::{RerankError, Result};
use crate::provider::{Ranked, Reranker};

/// Default reranker model key.
pub const DEFAULT_MODEL: &str = "bge-base";

/// Model key meaning "load whatever is in `model_dir`".
pub const CUSTOM_MODEL: &str = "custom";

/// Conventional ONNX file names, tried before falling back to any `.onnx`.
const ONNX_CANDIDATES: &[&str] = &[
    "model.onnx",
    "onnx/model.onnx",
    "model_quantized.onnx",
    "onnx/model_quantized.onnx",
];

/// Locate the ONNX weights in a model directory.
///
/// Conventional names first, then any `.onnx` in the directory or an `onnx/`
/// subdirectory — exports in the wild rarely agree on a name (FlashRank ships
/// `flashrank-MultiBERT-L12_Q.onnx`), and refusing to look would make the whole
/// feature useless for the models people actually want to bring.
fn find_onnx(dir: &Path) -> Option<std::path::PathBuf> {
    if let Some(known) = ONNX_CANDIDATES
        .iter()
        .map(|c| dir.join(c))
        .find(|p| p.is_file())
    {
        return Some(known);
    }
    for candidate_dir in [dir.to_path_buf(), dir.join("onnx")] {
        let mut found: Vec<std::path::PathBuf> = std::fs::read_dir(&candidate_dir)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "onnx"))
            .collect();
        found.sort();
        if let Some(first) = found.into_iter().next() {
            return Some(first);
        }
    }
    None
}

/// Read the tokenizer files fastembed needs beside the ONNX.
fn read_tokenizer_files(dir: &Path) -> Result<TokenizerFiles> {
    let required = |name: &str| -> Result<Vec<u8>> {
        std::fs::read(dir.join(name))
            .map_err(|e| RerankError::MissingConfig(format!("{name} in {}: {e}", dir.display())))
    };
    let optional = |name: &str| std::fs::read(dir.join(name)).unwrap_or_default();
    Ok(TokenizerFiles {
        tokenizer_file: required("tokenizer.json")?,
        config_file: required("config.json")?,
        special_tokens_map_file: optional("special_tokens_map.json"),
        tokenizer_config_file: optional("tokenizer_config.json"),
    })
}

/// A fastembed cross-encoder reranker.
/// Candidates shown to the cross-encoder unless configured otherwise.
const DEFAULT_POOL: usize = 100;

/// Candidates scored per forward pass.
///
/// fastembed defaults to 256, which puts a whole pool in one pass: the batch is
/// padded to its longest member, so peak memory is `pool × 512 tokens` of
/// activations through every layer. Measured here, a server reranking 100
/// candidates with `bge-reranker-base` — XLM-RoBERTa, whose 250k-token
/// vocabulary alone is most of a gigabyte — reached 5.7 GB resident and helped
/// push a 15 GB machine into the OOM killer.
///
/// Batching trades a little speed for a bound on that. The text itself is not
/// the problem: fastembed already truncates each candidate to the model's
/// maximum length, so trimming it first would change nothing.
const BATCH: usize = 16;

pub struct LocalReranker {
    model: TextRerank,
    name: String,
    pool: usize,
}

impl LocalReranker {
    /// Set how many candidates this reranker asks to be shown.
    pub fn with_pool(mut self, pool: usize) -> Self {
        self.pool = pool;
        self
    }

    /// Load the reranker named by `key` (downloads/caches on first use).
    pub fn load(key: &str, model_dir: Option<&Path>) -> Result<Self> {
        // A directory wins over the key: it is the only way to run a
        // cross-encoder fastembed does not ship, which is the whole point of it.
        if let Some(dir) = model_dir {
            return Self::load_user_defined(key, dir);
        }
        if key == CUSTOM_MODEL {
            return Err(RerankError::MissingConfig(
                "reranking.model is `custom` but reranking.model_dir is empty".to_string(),
            ));
        }

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
            pool: DEFAULT_POOL,
        })
    }

    /// Load a cross-encoder from a directory of ONNX + tokenizer files.
    fn load_user_defined(key: &str, dir: &Path) -> Result<Self> {
        let onnx_path = find_onnx(dir).ok_or_else(|| {
            RerankError::MissingConfig(format!("no .onnx file in {}", dir.display()))
        })?;
        let onnx = std::fs::read(&onnx_path)
            .map_err(|e| RerankError::Backend(format!("reading {}: {e}", onnx_path.display())))?;

        let udm = UserDefinedRerankingModel::new(onnx, read_tokenizer_files(dir)?);
        let text_rerank =
            TextRerank::try_new_from_user_defined(udm, RerankInitOptionsUserDefined::default())
                .map_err(|e| RerankError::Backend(e.to_string()))?;
        Ok(Self {
            model: text_rerank,
            name: key.to_string(),
            pool: DEFAULT_POOL,
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
            .rerank(query, docs, false, Some(BATCH))
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

    fn pool(&self) -> usize {
        self.pool
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
        let r = LocalReranker::load("bge-base", None).unwrap();
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
