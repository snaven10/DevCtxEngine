//! The central config (`config.yaml`): the global memory model plus the
//! defaults new projects inherit.
//!
//! Precedence for anything a project can also set is always
//! `.devctx/config.yaml` › these defaults › the built-in defaults. The one
//! exception is [`Memory`], which is not a default but a constraint: it pins the
//! vector space every globally-scoped memory lives in, and cannot vary per
//! project.

use std::path::Path;

use devctx_core::config::{Embeddings, Language, Reranking, Storage};
use serde::{Deserialize, Serialize};

use crate::error::{CentralError, Result};

/// `memory:` section — pins the central vector space.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
    /// Provider used to embed global memories.
    #[serde(default = "default_provider")]
    pub provider: String,
    /// Model key used to embed global memories.
    #[serde(default = "default_model")]
    pub model: String,
    /// Directory holding a user-defined ONNX model (e.g. Granite). Empty falls
    /// back to `defaults.embeddings.model_dir`, then to `DEVCTX_MODEL_DIR`.
    ///
    /// Without this the central embedder could only be pointed at a
    /// user-defined model through the environment, so a daemon auto-spawned by
    /// a process that lacked the variable silently fell back to the default
    /// model — embedding queries in a different vector space than the stored
    /// memories, with no error because the dimensions matched.
    #[serde(default)]
    pub model_dir: String,
}

impl Default for Memory {
    fn default() -> Self {
        Self {
            provider: default_provider(),
            model: default_model(),
            model_dir: String::new(),
        }
    }
}

fn default_provider() -> String {
    "local".to_string()
}

fn default_model() -> String {
    "minilm-l6".to_string()
}

/// `defaults:` section — what a newly registered project inherits.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Defaults {
    /// Default `embeddings:` for new projects.
    #[serde(default)]
    pub embeddings: Embeddings,
    /// Default `reranking:` for new projects.
    #[serde(default)]
    pub reranking: Reranking,
    /// Default `language:` for new projects.
    ///
    /// `Option` rather than a bare value, because the two are not the same
    /// question: an absent key means "this machine has no preference, use the
    /// built-in", while a present one means "every project here starts in
    /// Spanish". Storing the type's own default would make those
    /// indistinguishable and silently override a copied config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<Language>,
    /// Default `storage:` for new projects. `Option` for the same reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<Storage>,
}

/// `reindex:` section — the daemon's optional background refresh.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Reindex {
    /// Seconds between sweeps. `0` (the default) disables it entirely.
    ///
    /// Off by default on purpose: silently indexing every repository you have
    /// ever registered is surprising, and on a laptop it is expensive. Turn it
    /// on when you want the index warm without thinking about it.
    #[serde(default)]
    pub every_seconds: u64,
}

/// The full central configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CentralConfig {
    /// `memory:` section.
    #[serde(default)]
    pub memory: Memory,
    /// `defaults:` section.
    #[serde(default)]
    pub defaults: Defaults,
    /// `reindex:` section.
    #[serde(default)]
    pub reindex: Reindex,
}

impl CentralConfig {
    /// Parse from a YAML string.
    pub fn from_yaml(yaml: &str) -> std::result::Result<Self, serde_yaml::Error> {
        serde_yaml::from_str(yaml)
    }

    /// Load the config at `path`, or return defaults when it does not exist.
    ///
    /// A missing central config is the normal first-run state, not an error —
    /// the defaults are what `devctx init` would have written anyway.
    pub fn load_or_default(path: &Path) -> Result<Self> {
        let raw = match std::fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(CentralError::Io(e, path.to_path_buf())),
        };
        Self::from_yaml(&raw).map_err(|e| CentralError::ConfigParse(e, path.to_path_buf()))
    }

    /// Write the config to `path`, creating parent directories as needed.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| CentralError::Io(e, parent.to_path_buf()))?;
        }
        let yaml = serde_yaml::to_string(self)?;
        std::fs::write(path, yaml).map_err(|e| CentralError::Io(e, path.to_path_buf()))
    }

    /// Vector dimension of the central memory space, resolved without loading
    /// the model.
    pub fn memory_dimension(&self) -> usize {
        devctx_embed::dimension_for(&self.memory.provider, &self.memory.model)
    }
}

#[cfg(test)]
mod tests {
    /// `language` and `storage` under `defaults:` used to be dropped on the
    /// floor: a project created afterwards took the built-in and gave no sign
    /// that the setting had been ignored. `Option` is what lets "unset" and
    /// "set to the default value" stay different questions.
    #[test]
    fn defaults_carry_language_and_storage_only_when_set() {
        let bare: CentralConfig = serde_yaml::from_str("defaults: {}\n").unwrap();
        assert!(bare.defaults.language.is_none());
        assert!(bare.defaults.storage.is_none());

        let set: CentralConfig = serde_yaml::from_str("defaults:\n  language: es\n").unwrap();
        assert!(
            set.defaults.language.is_some(),
            "an explicit language survives"
        );

        // And an unset one is not written back out, so a round trip does not
        // turn "no preference" into a stated one.
        let round = serde_yaml::to_string(&bare).unwrap();
        assert!(!round.contains("language:"), "{round}");
    }

    use super::*;

    #[test]
    fn defaults_pin_a_384_space() {
        let cfg = CentralConfig::default();
        assert_eq!(cfg.memory.model, "minilm-l6");
        assert_eq!(cfg.memory_dimension(), 384);
        assert_eq!(cfg.defaults.embeddings.provider, "local");
        // Reranking is opt-in: a cross-encoder pass costs seconds and gigabytes
        // to reorder a list the retriever already ranked well. See
        // `devctx_core::config::Reranking::enabled` for the measurements.
        assert!(!cfg.defaults.reranking.enabled);
    }

    #[test]
    fn parses_a_full_config() {
        let cfg = CentralConfig::from_yaml(
            r#"
memory:
  provider: local
  model: bge-base
defaults:
  embeddings:
    provider: local
    model: ml-granite
  reranking:
    enabled: false
    model: bge-v2-m3
"#,
        )
        .unwrap();
        assert_eq!(cfg.memory.model, "bge-base");
        assert_eq!(cfg.memory_dimension(), 768);
        assert_eq!(cfg.defaults.embeddings.model, "ml-granite");
        assert!(!cfg.defaults.reranking.enabled);
    }

    #[test]
    fn background_reindex_is_off_unless_asked_for() {
        assert_eq!(CentralConfig::default().reindex.every_seconds, 0);
        let cfg = CentralConfig::from_yaml("reindex:\n  every_seconds: 900\n").unwrap();
        assert_eq!(cfg.reindex.every_seconds, 900);
    }

    #[test]
    fn missing_file_is_not_an_error() {
        let cfg = CentralConfig::load_or_default(Path::new("/nonexistent/devctx.yaml")).unwrap();
        assert_eq!(cfg.memory.model, "minilm-l6");
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = std::env::temp_dir().join("devctx_central_cfg_rt");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("config.yaml");

        let mut cfg = CentralConfig::default();
        cfg.memory.model = "bge-base".into();
        cfg.save(&path).unwrap();

        let back = CentralConfig::load_or_default(&path).unwrap();
        assert_eq!(back.memory.model, "bge-base");
        assert_eq!(back.memory_dimension(), 768);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
