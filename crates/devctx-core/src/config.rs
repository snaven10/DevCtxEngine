//! Project configuration: the `.devctx/config.yaml` schema and discovery.
//!
//! This is the Rust-native, solo-local config for the DuckDB rewrite. It mirrors
//! the fields of the legacy Go `ProjectConfig` that still apply, and drops the
//! Qdrant / shared-mode / Python-runtime fields (see docs/rust-rewrite-plan.md §7).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Relative path of the config file inside a project.
pub const CONFIG_FILE_NAME: &str = ".devctx/config.yaml";

/// UI / summary language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    /// English (default).
    #[default]
    En,
    /// Spanish.
    Es,
}

/// Offline policy for model/network access.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Offline {
    /// Decide automatically based on availability (default).
    #[default]
    Auto,
    /// Force offline.
    True,
    /// Force online.
    False,
}

/// `project:` section.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Project {
    /// Human-friendly project name.
    #[serde(default)]
    pub name: String,
    /// Absolute path to the project root.
    #[serde(default)]
    pub path: String,
}

/// `embeddings:` section.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Embeddings {
    /// Provider key: `local` (default), `openai`, `voyage`, `custom`.
    #[serde(default = "default_provider")]
    pub provider: String,
    /// Model key from the registry (e.g. `minilm-l6`, `ml-granite`).
    #[serde(default = "default_model")]
    pub model: String,
    /// Directory holding a user-defined ONNX model (e.g. Granite): the ONNX file
    /// plus `tokenizer.json`/`config.json`. Overrides `DEVCTX_MODEL_DIR`; empty
    /// falls back to that env var. Lets you pin the model path in the config so
    /// no shell export is needed.
    #[serde(default)]
    pub model_dir: String,
    /// Offline policy.
    #[serde(default)]
    pub offline: Offline,
}

impl Default for Embeddings {
    fn default() -> Self {
        Self {
            provider: default_provider(),
            model: default_model(),
            model_dir: String::new(),
            offline: Offline::default(),
        }
    }
}

fn default_provider() -> String {
    "local".to_string()
}

fn default_model() -> String {
    "minilm-l6".to_string()
}

/// `storage:` section. Solo-local: a single DuckDB file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Storage {
    /// Path to the DuckDB database file. Empty => derived from `state_dir`.
    #[serde(default)]
    pub db_path: String,
    /// Build a VSS HNSW index for approximate nearest-neighbor search after
    /// indexing (requires the DuckDB VSS extension). Off => brute-force cosine.
    #[serde(default)]
    pub hnsw: bool,
    /// Build a BM25 full-text index over chunk text after indexing, enabling
    /// `search --keyword` (requires the DuckDB FTS extension).
    #[serde(default)]
    pub fts: bool,
}

/// `indexing:` section.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Indexing {
    /// Paths to keep out of the index, written as `.gitignore` patterns
    /// (`target/`, `*.generated.ts`, `docs/vendor/**`).
    ///
    /// Same syntax and same matcher as `.gitignore`, so a pattern behaves the
    /// same here as it would there — and identically whether a file arrives via
    /// `index`, the post-commit hook, or `watch`. This is for code git *does*
    /// track but that is not worth searching; anything already git-ignored is
    /// excluded without needing a rule here.
    #[serde(default)]
    pub exclude: Vec<String>,
}

/// `reranking:` section.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reranking {
    /// Whether to rerank search results with a cross-encoder.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Reranker model key (`bge-base` default, `bge-v2-m3` multilingual,
    /// `jina-turbo` fastest of the built-ins), or `custom` to load your own
    /// from `model_dir`.
    #[serde(default = "default_reranker")]
    pub model: String,
    /// Directory holding a user-supplied cross-encoder: the ONNX file plus
    /// `tokenizer.json`/`config.json`.
    ///
    /// The built-in choices are all large — `bge-reranker-base` is an
    /// XLM-RoBERTa carrying a 250k-token vocabulary, over a gigabyte on disk —
    /// because fastembed ships no lightweight cross-encoder. This is the way to
    /// use one anyway: point it at, say, an ONNX export of
    /// `ms-marco-MiniLM-L-12-v2`, which is an order of magnitude smaller.
    #[serde(default)]
    pub model_dir: String,
    /// How many candidates the cross-encoder is shown.
    ///
    /// This is the ceiling on everything reranking could ever fix: it can only
    /// reorder what it is handed, so an answer ranked below the pool is
    /// invisible to it however good the model is. It is also the whole cost —
    /// the cross-encoder is the slowest stage by two orders of magnitude, and
    /// this multiplies it. Deep pool with a small fast model, or shallow pool
    /// with a large one; deep and large is unusably slow.
    #[serde(default = "default_rerank_pool")]
    pub pool: usize,
}

impl Default for Reranking {
    fn default() -> Self {
        Self {
            enabled: true,
            model: default_reranker(),
            model_dir: String::new(),
            pool: default_rerank_pool(),
        }
    }
}

fn default_true() -> bool {
    true
}

/// Candidates shown to the cross-encoder by default.
fn default_rerank_pool() -> usize {
    100
}

fn default_reranker() -> String {
    "bge-base".to_string()
}

/// `summarization:` section.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Summarization {
    /// Provider: `extractive` (default), `openai`, `noop`.
    #[serde(default = "default_summarizer")]
    pub provider: String,
    /// Block non-local providers (privacy guard).
    #[serde(default = "default_true")]
    pub require_local: bool,
    /// Target summary length in tokens.
    #[serde(default = "default_target_tokens")]
    pub target_tokens: usize,
    /// Model id for API providers.
    #[serde(default = "default_summ_model")]
    pub model: String,
}

impl Default for Summarization {
    fn default() -> Self {
        Self {
            provider: default_summarizer(),
            require_local: true,
            target_tokens: default_target_tokens(),
            model: default_summ_model(),
        }
    }
}

fn default_summarizer() -> String {
    "extractive".to_string()
}

fn default_target_tokens() -> usize {
    200
}

fn default_summ_model() -> String {
    "gpt-4o-mini".to_string()
}

/// The full project configuration, mirroring `.devctx/config.yaml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectConfig {
    /// `project:` section.
    #[serde(default)]
    pub project: Project,
    /// Directory holding DevCtxEngine state (DuckDB, caches). Empty => default.
    #[serde(default)]
    pub state_dir: String,
    /// UI / summary language.
    #[serde(default)]
    pub language: Language,
    /// `embeddings:` section.
    #[serde(default)]
    pub embeddings: Embeddings,
    /// `storage:` section.
    #[serde(default)]
    pub storage: Storage,
    /// `indexing:` section.
    #[serde(default)]
    pub indexing: Indexing,
    /// `reranking:` section.
    #[serde(default)]
    pub reranking: Reranking,
    /// `summarization:` section.
    #[serde(default)]
    pub summarization: Summarization,
}

impl ProjectConfig {
    /// Parse a config from a YAML string.
    pub fn from_yaml(yaml: &str) -> std::result::Result<Self, serde_yaml::Error> {
        serde_yaml::from_str(yaml)
    }

    /// Load and parse the config at `path`.
    pub fn load(path: &Path) -> Result<Self> {
        let raw =
            std::fs::read_to_string(path).map_err(|e| Error::ConfigRead(path.to_path_buf(), e))?;
        Self::from_yaml(&raw).map_err(|e| Error::ConfigParse(path.to_path_buf(), e))
    }

    /// Discover the config by walking up from `start_dir`, then load it.
    pub fn discover(start_dir: &Path) -> Result<(PathBuf, Self)> {
        let path = find_config_file(start_dir)
            .ok_or_else(|| Error::ConfigNotFound(start_dir.to_path_buf()))?;
        let cfg = Self::load(&path)?;
        Ok((path, cfg))
    }

    /// Resolve the effective DuckDB database file path.
    ///
    /// Priority: explicit `storage.db_path` > `{state_dir}/index.duckdb` >
    /// `.devctx/state/index.duckdb` relative to `project.path`.
    pub fn db_path(&self) -> PathBuf {
        if !self.storage.db_path.is_empty() {
            return PathBuf::from(&self.storage.db_path);
        }
        if !self.state_dir.is_empty() {
            return Path::new(&self.state_dir).join("index.duckdb");
        }
        let base = if self.project.path.is_empty() {
            PathBuf::from(".")
        } else {
            PathBuf::from(&self.project.path)
        };
        base.join(".devctx").join("state").join("index.duckdb")
    }
}

/// Walk up from `start_dir` looking for `.devctx/config.yaml`.
///
/// Returns the absolute path to the config file, or `None` if the filesystem
/// root is reached without finding one.
pub fn find_config_file(start_dir: &Path) -> Option<PathBuf> {
    let mut dir = std::fs::canonicalize(start_dir).unwrap_or_else(|_| start_dir.to_path_buf());
    loop {
        let candidate = dir.join(CONFIG_FILE_NAME);
        if candidate.is_file() {
            return Some(candidate);
        }
        match dir.parent() {
            Some(parent) => dir = parent.to_path_buf(),
            None => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sensible() {
        let cfg = ProjectConfig::from_yaml("{}").unwrap();
        assert_eq!(cfg.embeddings.provider, "local");
        assert_eq!(cfg.embeddings.model, "minilm-l6");
        assert_eq!(cfg.embeddings.offline, Offline::Auto);
        assert_eq!(cfg.language, Language::En);
        assert!(cfg.indexing.exclude.is_empty());
    }

    #[test]
    fn parses_a_full_config() {
        let yaml = r#"
project:
  name: myproj
  path: /home/u/myproj
state_dir: /home/u/myproj/.devctx/state
language: es
embeddings:
  provider: local
  model: ml-granite
  offline: "true"
storage:
  db_path: /tmp/custom.duckdb
indexing:
  exclude:
    - "target/**"
    - "*.log"
"#;
        let cfg = ProjectConfig::from_yaml(yaml).unwrap();
        assert_eq!(cfg.project.name, "myproj");
        assert_eq!(cfg.language, Language::Es);
        assert_eq!(cfg.embeddings.model, "ml-granite");
        assert_eq!(cfg.embeddings.offline, Offline::True);
        assert_eq!(cfg.db_path(), PathBuf::from("/tmp/custom.duckdb"));
        assert_eq!(cfg.indexing.exclude.len(), 2);
    }

    #[test]
    fn db_path_falls_back_to_state_dir() {
        let cfg = ProjectConfig {
            state_dir: "/var/state".to_string(),
            ..Default::default()
        };
        assert_eq!(cfg.db_path(), PathBuf::from("/var/state/index.duckdb"));
    }

    #[test]
    fn db_path_falls_back_to_project_path() {
        let cfg = ProjectConfig {
            project: Project {
                path: "/home/u/proj".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(
            cfg.db_path(),
            PathBuf::from("/home/u/proj/.devctx/state/index.duckdb")
        );
    }
}
