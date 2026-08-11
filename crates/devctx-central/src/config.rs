//! The central config (`config.yaml`): the global memory model plus the
//! defaults new projects inherit.
//!
//! Precedence for anything a project can also set is always
//! `.devctx/config.yaml` › these defaults › the built-in defaults. The one
//! exception is [`Memory`], which is not a default but a constraint: it pins the
//! vector space every globally-scoped memory lives in, and cannot vary per
//! project.

use std::path::Path;

use devctx_core::config::{Embeddings, Reranking};
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
}

impl Default for Memory {
    fn default() -> Self {
        Self {
            provider: default_provider(),
            model: default_model(),
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
    use super::*;

    #[test]
    fn defaults_pin_a_384_space() {
        let cfg = CentralConfig::default();
        assert_eq!(cfg.memory.model, "minilm-l6");
        assert_eq!(cfg.memory_dimension(), 384);
        assert_eq!(cfg.defaults.embeddings.provider, "local");
        assert!(cfg.defaults.reranking.enabled);
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
