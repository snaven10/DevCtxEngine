//! Error types shared across DevAI crates.

use std::path::PathBuf;

/// Result alias used throughout DevAI.
pub type Result<T> = std::result::Result<T, Error>;

/// Top-level error type for DevAI core operations.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// No `.devai/config.yaml` was found walking up from the start directory.
    #[error("no DevAI project found (looked for .devai/config.yaml walking up from {0})")]
    ConfigNotFound(PathBuf),

    /// The config file exists but could not be read.
    #[error("failed to read config {0}: {1}")]
    ConfigRead(PathBuf, #[source] std::io::Error),

    /// The config file exists but is not valid YAML / does not match the schema.
    #[error("failed to parse config {0}: {1}")]
    ConfigParse(PathBuf, #[source] serde_yaml::Error),

    /// Catch-all for other failures.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
