//! Local embedding provider backed by `fastembed`/`ort` (ONNX Runtime).
//!
//! Built-in models are downloaded/cached from HuggingFace on first use. Models
//! with no fastembed built-in (Granite) are loaded as user-defined ONNX from a
//! local directory (`EmbedSettings::model_dir`), which must contain the ONNX
//! file plus the tokenizer JSON files.

use std::path::{Path, PathBuf};

use devctx_core::config::Device;
use devctx_core::CudaFallback;
use fastembed::{
    EmbeddingModel, ExecutionProviderDispatch, InitOptions, InitOptionsUserDefined, Pooling,
    TextEmbedding, TokenizerFiles, UserDefinedEmbeddingModel,
};

use crate::error::{EmbedError, Result};
use crate::provider::{l2_normalize, EmbeddingProvider};
use crate::registry::{self, LocalModelSpec, DEFAULT_LOCAL_MODEL};
use crate::EmbedSettings;

/// Default per-text character cap (RAM guard), overridable via env.
const DEFAULT_MAX_CHARS: usize = 4096;
/// Default embedding batch size, overridable via env. Small on purpose: peak
/// memory scales with the batch (activations of `batch x seq_len`), and with the
/// old parallel split 8 peaked at 6.5 GB where 32 peaked at 18.2 GB.
const DEFAULT_BATCH_SIZE: usize = 8;
/// Candidate ONNX filenames inside a user-defined model directory.
const ONNX_CANDIDATES: &[&str] = &[
    "onnx/model_quint8_avx2.onnx",
    "onnx/model.onnx",
    "model_quint8_avx2.onnx",
    "model.onnx",
];

/// A fastembed-backed embedding provider.
pub struct LocalProvider {
    /// Falls back to a CPU rebuild if inference fails while on CUDA.
    model: CudaFallback<TextEmbedding, EmbedError>,
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

        let (model, on_cuda) = load_model(spec, settings.model_dir.as_deref(), settings.device)?;
        let model_dir = settings.model_dir.clone();
        let model = CudaFallback::new("embeddings", model, on_cuda, move || {
            load_model(spec, model_dir.as_deref(), Device::Cpu).map(|(m, _)| m)
        });

        Ok(Self {
            model,
            dimension: spec.dimension,
            name: spec.key.to_string(),
            max_chars: env_usize("DEVCTX_EMBED_MAX_CHARS", DEFAULT_MAX_CHARS),
            batch_size: env_usize("DEVCTX_EMBED_BATCH_SIZE", DEFAULT_BATCH_SIZE),
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
        let mut out = self.model.run(|m| {
            embed_in_series(&capped, self.batch_size, |chunk| {
                // `Some(len)`: fastembed splits its input in `par_chunks` over
                // rayon, so any larger input runs up to one batch per core at
                // once. A chunk of exactly one batch keeps a single in flight.
                m.embed(
                    chunk.iter().map(String::as_str).collect(),
                    Some(chunk.len()),
                )
                .map_err(|e| EmbedError::Backend(e.to_string()))
            })
        })?;
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

/// Embed `texts` one batch of at most `batch_size` at a time, strictly in
/// order, so at most one batch is ever in flight. Output order and length match
/// the input.
fn embed_in_series(
    texts: &[String],
    batch_size: usize,
    mut embed_batch: impl FnMut(&[String]) -> Result<Vec<Vec<f32>>>,
) -> Result<Vec<Vec<f32>>> {
    let mut out = Vec::with_capacity(texts.len());
    for chunk in texts.chunks(batch_size.max(1)) {
        out.extend(embed_batch(chunk)?);
    }
    Ok(out)
}

/// The CUDA execution provider, or `None` when built without `--features gpu`.
///
/// `error_on_failure` makes a missing driver/toolkit fail the session instead of
/// ort silently continuing on CPU, so [`init_on`] can say so.
#[cfg(feature = "gpu")]
fn cuda_providers() -> Option<Vec<ExecutionProviderDispatch>> {
    use ort::execution_providers::CUDAExecutionProvider;
    Some(vec![CUDAExecutionProvider::default()
        .build()
        .error_on_failure()])
}

#[cfg(not(feature = "gpu"))]
fn cuda_providers() -> Option<Vec<ExecutionProviderDispatch>> {
    None
}

/// Run `init` with the execution providers for `device`, falling back to CPU
/// (with a warning) when CUDA was asked for but is unavailable. The flag says
/// whether the result is actually on CUDA.
fn init_on<T>(
    device: Device,
    mut init: impl FnMut(Vec<ExecutionProviderDispatch>) -> Result<T>,
) -> Result<(T, bool)> {
    if device == Device::Cuda {
        match cuda_providers() {
            Some(providers) => {
                eprintln!("devctx: embeddings requested CUDA (device: cuda)");
                match init(providers) {
                    Ok(model) => return Ok((model, true)),
                    // A stalled download is not a CUDA problem, and retrying
                    // on CPU would only queue behind the stuck loader's lock
                    // (see `devctx_core::modelload`): report it as it is.
                    Err(e) if devctx_core::modelload::is_stall_error(&e.to_string()) => {
                        return Err(e)
                    }
                    Err(e) => eprintln!(
                        "devctx: warning: CUDA failed for embeddings ({e}); falling back to CPU. \
                         Check the NVIDIA driver, CUDA 12 toolkit and cuDNN 9."
                    ),
                }
            }
            None => eprintln!(
                "devctx: warning: device is cuda but this binary was built without GPU \
                 support (--features gpu); embeddings run on CPU"
            ),
        }
    }
    init(Vec::new()).map(|model| (model, false))
}

/// Load `spec` from fastembed's catalog or from `model_dir`, on `device`.
fn load_model(
    spec: &LocalModelSpec,
    model_dir: Option<&Path>,
    device: Device,
) -> Result<(TextEmbedding, bool)> {
    match spec.builtin {
        Some(builtin) => load_builtin(builtin, spec, device),
        None => load_user_defined(spec, model_dir, device),
    }
}

fn load_builtin(
    builtin: &str,
    spec: &LocalModelSpec,
    device: Device,
) -> Result<(TextEmbedding, bool)> {
    let model = builtin_model(builtin)?;
    let mut opts = InitOptions::new(model).with_show_download_progress(false);
    // Without this fastembed caches relative to the working directory, so the
    // models land in whatever repository happened to be current — hundreds of
    // megabytes of them, re-downloaded per checkout.
    if let Some(cache) = devctx_core::dirs::model_cache_dir() {
        opts = opts.with_cache_dir(cache);
    }
    if let Some(max) = spec.max_input_tokens {
        opts = opts.with_max_length(max);
    }
    init_on(device, |eps| {
        // hf-hub's download has no read timeout: guard it (see `modelload`).
        let opts = opts.clone().with_execution_providers(eps);
        devctx_core::modelload::guard_load(spec.key, move || TextEmbedding::try_new(opts))
            .map_err(EmbedError::Backend)?
            .map_err(|e| EmbedError::Backend(e.to_string()))
    })
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

fn load_user_defined(
    spec: &LocalModelSpec,
    model_dir: Option<&Path>,
    device: Device,
) -> Result<(TextEmbedding, bool)> {
    let dir = model_dir.ok_or_else(|| {
        EmbedError::MissingConfig(format!(
            "model_dir (DEVCTX_MODEL_DIR) for user-defined model '{}' ({})",
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
    init_on(device, |eps| {
        let opts = InitOptionsUserDefined::new().with_execution_providers(eps);
        TextEmbedding::try_new_from_user_defined(udm.clone(), opts)
            .map_err(|e| EmbedError::Backend(e.to_string()))
    })
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn batches_run_in_series_and_keep_order_and_length() {
        let texts: Vec<String> = (0..23).map(|i| i.to_string()).collect();
        let in_flight = AtomicUsize::new(0);
        let max_in_flight = AtomicUsize::new(0);
        let sizes = std::sync::Mutex::new(Vec::new());
        let out = super::embed_in_series(&texts, 8, |chunk| {
            let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            max_in_flight.fetch_max(now, Ordering::SeqCst);
            sizes.lock().unwrap().push(chunk.len());
            std::thread::sleep(std::time::Duration::from_millis(2));
            let v = chunk
                .iter()
                .map(|t| vec![t.parse::<f32>().unwrap()])
                .collect();
            in_flight.fetch_sub(1, Ordering::SeqCst);
            Ok(v)
        })
        .unwrap();
        assert_eq!(max_in_flight.load(Ordering::SeqCst), 1);
        assert_eq!(*sizes.lock().unwrap(), vec![8, 8, 7]);
        assert_eq!(out.len(), 23);
        for (i, v) in out.iter().enumerate() {
            assert_eq!(v[0], i as f32, "order lost at {i}");
        }
        // A zero batch size cannot loop forever or panic.
        assert_eq!(
            super::embed_in_series(&texts, 0, |c| Ok(vec![vec![0.0]; c.len()]))
                .unwrap()
                .len(),
            23
        );
    }

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
            load_user_defined(spec, None, Device::Cpu),
            Err(EmbedError::MissingConfig(_))
        ));
    }

    #[test]
    fn env_usize_parses_and_guards() {
        std::env::set_var("DEVCTX_TEST_BATCH", "64");
        assert_eq!(env_usize("DEVCTX_TEST_BATCH", 32), 64);
        std::env::set_var("DEVCTX_TEST_BATCH", "0");
        assert_eq!(env_usize("DEVCTX_TEST_BATCH", 32), 32);
        std::env::remove_var("DEVCTX_TEST_BATCH");
        assert_eq!(env_usize("DEVCTX_TEST_BATCH", 32), 32);
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
