//! Errors raised by the central store.

use std::path::PathBuf;

/// Result alias for central-store operations.
pub type Result<T> = std::result::Result<T, CentralError>;

/// What can go wrong opening or using the central store.
#[derive(Debug, thiserror::Error)]
pub enum CentralError {
    /// Neither `$HOME` nor `$USERPROFILE` is set, so XDG paths cannot be derived.
    #[error(
        "cannot determine the home directory; set DEVCTX_HOME to choose a \
         central store location"
    )]
    NoHome,

    /// The central database was created with a different embedding model, so its
    /// `FLOAT[n]` vector column no longer matches the configured one.
    #[error(
        "central store at {path} holds {found}-dimensional vectors but `memory.model` \
         resolves to {expected}; changing the central memory model requires re-creating \
         the store (its existing vectors cannot be compared against the new model)"
    )]
    DimensionMismatch {
        /// The central database.
        path: PathBuf,
        /// Dimension recorded on disk.
        found: usize,
        /// Dimension the current config resolves to.
        expected: usize,
    },

    /// The repository has no `.devctx/config.yaml` and creating one was not requested.
    #[error("{0} is not a DevCtxEngine project (run `devctx init` there, or pass --init)")]
    NotInitialized(PathBuf),

    /// Another repository is already registered under this name.
    #[error("project name `{name}` is already taken by {path}; pass --name to choose another")]
    NameTaken {
        /// The contested name.
        name: String,
        /// Where the existing project lives.
        path: String,
    },

    /// No project is registered under this name.
    #[error("no registered project named `{0}`")]
    UnknownProject(String),

    /// Reading or writing a file failed.
    #[error("{1}: {0}")]
    Io(#[source] std::io::Error, PathBuf),

    /// The central config could not be parsed.
    #[error("parsing {1}: {0}")]
    ConfigParse(#[source] serde_yaml::Error, PathBuf),

    /// The central config could not be serialized.
    #[error("serializing config: {0}")]
    ConfigWrite(#[from] serde_yaml::Error),

    /// A project config could not be read or parsed.
    #[error(transparent)]
    ProjectConfig(#[from] devctx_core::Error),

    /// The underlying store failed.
    #[error(transparent)]
    Store(#[from] devctx_store::StoreError),
}
