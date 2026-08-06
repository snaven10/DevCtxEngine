//! Embedding error type.

/// Result alias for embedding operations.
pub type Result<T> = std::result::Result<T, EmbedError>;

/// Errors raised by embedding providers.
#[derive(Debug, thiserror::Error)]
pub enum EmbedError {
    /// The requested model key is not in the registry.
    #[error("unknown model '{0}' for provider '{1}'")]
    UnknownModel(String, String),

    /// The requested provider is not supported.
    #[error("unknown embedding provider '{0}'")]
    UnknownProvider(String),

    /// A required credential or endpoint was missing (e.g. API key env var).
    #[error("missing configuration: {0}")]
    MissingConfig(String),

    /// An HTTP request to an embedding API failed.
    #[error("http request failed: {0}")]
    Http(String),

    /// The API returned a body we could not parse into embeddings.
    #[error("invalid API response: {0}")]
    BadResponse(String),

    /// The local embedding backend failed (model load / inference).
    #[error("local embedding backend: {0}")]
    Backend(String),

    /// The `local` feature is disabled but a local model was requested.
    #[error("local embeddings are not compiled in (enable the `local` feature)")]
    LocalDisabled,
}
