//! Rerank error type.

/// Result alias for reranking operations.
pub type Result<T> = std::result::Result<T, RerankError>;

/// Errors raised by rerankers.
#[derive(Debug, thiserror::Error)]
pub enum RerankError {
    /// The requested reranker model key is unknown.
    #[error("unknown reranker model '{0}'")]
    UnknownModel(String),

    /// The cross-encoder backend failed (model load / inference).
    #[error("reranker backend: {0}")]
    Backend(String),
}
