//! Local embedding provider backed by `fastembed`/`ort` (ONNX Runtime).
//!
//! Built-in models are downloaded/cached from HuggingFace on first use. Models
//! with no fastembed built-in (Granite) are loaded as user-defined ONNX from a
//! local directory (`EmbedSettings::model_dir`), which must contain the ONNX
//! file plus the tokenizer JSON files.

use std::path::{Path, PathBuf};

use fastembed::{
    EmbeddingModel, InitOptions, InitOptionsUserDefined, Pooling, TextEmbedding, TokenizerFiles,
    UserDefinedEmbeddingModel,
};

use crate::error::{EmbedError, Result};
use crate::provider::{l2_normalize, EmbeddingProvider};
use crate::registry::{self, LocalModelSpec, DEFAULT_LOCAL_MODEL};
use crate::EmbedSettings;

/// Default per-text character cap (RAM guard), overridable via env.
const DEFAULT_MAX_CHARS: usize = 4096;
/// Default embedding batch size, overridable via env.
const DEFAULT_BATCH_SIZE: usize = 32;
/// Candidate ONNX filenames inside a user-defined model directory.
const ONNX_CANDIDATES: &[&str] = &[
    "onnx/model_quint8_avx2.onnx",
    "onnx/model.onnx",
    "model_quint8_avx2.onnx",
    "model.onnx",
];

/// A fastembed-backed embedding provider.
pub struct LocalProvider {
    model: TextEmbedding,
    dimension: usize,
    name: String,
    max_chars: usize,
    batch_size: usize,
}

impl LocalProvider {
    /// Load the model named by `settings` (falling back to the default key).
    pub fn load(settings: &EmbedSettings) -> Result<Self> {
        let key = if settings.model.is_empty() {
            DEFAULT_LOCAL_MODEL
        } else {
            settings.model.as_str()
        };
        let spec = registry::find_local(key)
            .ok_or_else(|| EmbedError::UnknownModel(key.to_string(), "local".into()))?;

        let model = match spec.builtin {
            Some(builtin) => load_builtin(builtin, spec)?,
            None => load_user_defined(spec, settings.model_dir.as_deref())?,
        };

        Ok(Self {
            model,
            dimension: spec.dimension,
            name: spec.key.to_string(),
            max_chars: env_usize("DEVAI_EMBED_MAX_CHARS", DEFAULT_MAX_CHARS),
            batch_size: env_usize("DEVAI_EMBED_BATCH_SIZE", DEFAULT_BATCH_SIZE),
        })
    }
}

impl EmbeddingProvider for LocalProvider {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let capped: Vec<String> = texts
            .iter()
            .map(|t| t.chars().take(self.max_chars).collect())
            .collect();
        let mut out = self
            .model
            .embed(capped, Some(self.batch_size))
            .map_err(|e| EmbedError::Backend(e.to_string()))?;
        for v in &mut out {
            l2_normalize(v);
        }
        Ok(out)
    }

    fn dimension(&self) -> usize {
        self.dimension
    }

    fn model_name(&self) -> &str {
        &self.name
    }
}

fn load_builtin(builtin: &str, spec: &LocalModelSpec) -> Result<TextEmbedding> {
    let model = builtin_model(builtin)?;
    let mut opts = InitOptions::new(model).with_show_download_progress(false);
    if let Some(max) = spec.max_input_tokens {
        opts = opts.with_max_length(max);
    }
    TextEmbedding::try_new(opts).map_err(|e| EmbedError::Backend(e.to_string()))
}

fn builtin_model(builtin: &str) -> Result<EmbeddingModel> {
    Ok(match builtin {
        "AllMiniLML6V2" => EmbeddingModel::AllMiniLML6V2,
        "AllMiniLML12V2" => EmbeddingModel::AllMiniLML12V2,
        "BGESmallENV15" => EmbeddingModel::BGESmallENV15,
        "BGEBaseENV15" => EmbeddingModel::BGEBaseENV15,
        "ParaphraseMLMiniLML12V2" => EmbeddingModel::ParaphraseMLMiniLML12V2,
        "ParaphraseMLMpnetBaseV2" => EmbeddingModel::ParaphraseMLMpnetBaseV2,
        other => return Err(EmbedError::Backend(format!("unmapped builtin '{other}'"))),
    })
}

fn load_user_defined(spec: &LocalModelSpec, model_dir: Option<&Path>) -> Result<TextEmbedding> {
    let dir = model_dir.ok_or_else(|| {
        EmbedError::MissingConfig(format!(
            "model_dir (DEVAI_MODEL_DIR) for user-defined model '{}' ({})",
            spec.key, spec.hf_repo
        ))
    })?;

    let onnx_path = ONNX_CANDIDATES
        .iter()
        .map(|c| dir.join(c))
        .find(|p| p.is_file())
        .ok_or_else(|| {
            EmbedError::MissingConfig(format!(
                "no ONNX file in {} (looked for {:?})",
                dir.display(),
                ONNX_CANDIDATES
            ))
        })?;

    let onnx = std::fs::read(&onnx_path)
        .map_err(|e| EmbedError::Backend(format!("reading {}: {e}", onnx_path.display())))?;
    let tokenizer_files = read_tokenizer_files(dir)?;

    let udm = UserDefinedEmbeddingModel::new(onnx, tokenizer_files).with_pooling(Pooling::Mean);
    TextEmbedding::try_new_from_user_defined(udm, InitOptionsUserDefined::new())
        .map_err(|e| EmbedError::Backend(e.to_string()))
}

fn read_tokenizer_files(dir: &Path) -> Result<TokenizerFiles> {
    Ok(TokenizerFiles {
        tokenizer_file: read_required(dir, "tokenizer.json")?,
        config_file: read_required(dir, "config.json")?,
        special_tokens_map_file: read_optional(dir, "special_tokens_map.json"),
        tokenizer_config_file: read_optional(dir, "tokenizer_config.json"),
    })
}

fn read_required(dir: &Path, name: &str) -> Result<Vec<u8>> {
    let path: PathBuf = dir.join(name);
    std::fs::read(&path).map_err(|e| EmbedError::MissingConfig(format!("{}: {e}", path.display())))
}

fn read_optional(dir: &Path, name: &str) -> Vec<u8> {
    std::fs::read(dir.join(name)).unwrap_or_default()
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_mapping_covers_registry() {
        // Every builtin key in the registry must map to a fastembed model.
        for spec in registry::LOCAL_MODELS {
            if let Some(b) = spec.builtin {
                assert!(builtin_model(b).is_ok(), "unmapped builtin {b}");
            }
        }
    }

    #[test]
    fn user_defined_requires_model_dir() {
        let spec = registry::find_local("ml-granite").unwrap();
        assert!(matches!(
            load_user_defined(spec, None),
            Err(EmbedError::MissingConfig(_))
        ));
    }

    #[test]
    fn env_usize_parses_and_guards() {
        std::env::set_var("DEVAI_TEST_BATCH", "64");
        assert_eq!(env_usize("DEVAI_TEST_BATCH", 32), 64);
        std::env::set_var("DEVAI_TEST_BATCH", "0");
        assert_eq!(env_usize("DEVAI_TEST_BATCH", 32), 32);
        std::env::remove_var("DEVAI_TEST_BATCH");
        assert_eq!(env_usize("DEVAI_TEST_BATCH", 32), 32);
    }

    /// Real embedding: downloads MiniLM-L6 and checks shape + normalization.
    /// Ignored by default (network + model download).
    #[test]
    #[ignore = "downloads a model from HuggingFace"]
    fn minilm_embeds_and_normalizes() {
        let settings = EmbedSettings {
            provider: "local".into(),
            model: "minilm-l6".into(),
            ..Default::default()
        };
        let p = LocalProvider::load(&settings).unwrap();
        assert_eq!(p.dimension(), 384);
        let out = p
            .embed(&["hello world".into(), "def foo(): pass".into()])
            .unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].len(), 384);
        let norm = out[0].iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-4, "norm was {norm}");
    }
}
