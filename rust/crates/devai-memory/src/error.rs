//! Memory-engine error type.

/// Result alias for memory operations.
pub type Result<T> = std::result::Result<T, MemoryError>;

/// Errors raised by the memory engine.
#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    /// A store operation failed.
    #[error(transparent)]
    Store(#[from] devai_store::StoreError),

    /// An embedding operation failed.
    #[error(transparent)]
    Embed(#[from] devai_embed::EmbedError),

    /// The embedder's dimension did not match the store's.
    #[error("embedder dimension {embedder} != store dimension {store}")]
    DimensionMismatch {
        /// Embedder dimension.
        embedder: usize,
        /// Store dimension.
        store: usize,
    },
}
