//! Indexing error type.

/// Result alias for indexing operations.
pub type Result<T> = std::result::Result<T, IndexError>;

/// Errors raised by the indexing pipeline.
#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    /// A `git` invocation failed.
    #[error("git {0} failed: {1}")]
    Git(String, String),

    /// Spawning `git` failed (not installed, etc.).
    #[error("failed to run git: {0}")]
    GitSpawn(#[from] std::io::Error),

    /// `--branch` named a branch this repository does not have.
    ///
    /// Reported rather than created: `index` reads a repository, and a read
    /// command that quietly makes a branch is a surprise to undo by hand.
    #[error(
        "this repository has no branch `{0}` — create it with git first, or pass one that exists"
    )]
    UnknownBranch(String),

    /// A store operation failed.
    #[error(transparent)]
    Store(#[from] devctx_store::StoreError),

    /// A parse operation failed.
    #[error(transparent)]
    Parse(#[from] devctx_parse::ParseError),

    /// An embedding operation failed.
    #[error(transparent)]
    Embed(#[from] devctx_embed::EmbedError),

    /// The embedder's dimension did not match the store's.
    #[error("embedder dimension {embedder} != store dimension {store}")]
    DimensionMismatch {
        /// Embedder dimension.
        embedder: usize,
        /// Store dimension.
        store: usize,
    },
}
