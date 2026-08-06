//! Summarizer error type.

/// Result alias for summarization.
pub type Result<T> = std::result::Result<T, SummarizeError>;

/// Errors raised by summarizers.
#[derive(Debug, thiserror::Error)]
pub enum SummarizeError {
    /// An embedding operation failed (extractive summarizer).
    #[error(transparent)]
    Embed(#[from] devctx_embed::EmbedError),

    /// The extractive summarizer was requested without an embedder.
    #[error("the extractive summarizer requires an embedder")]
    MissingEmbedder,

    /// A cloud provider was requested while `require_local` is set.
    #[error("provider '{0}' is non-local but require_local is set")]
    NonLocalBlocked(String),

    /// A required credential was missing.
    #[error("missing configuration: {0}")]
    MissingConfig(String),

    /// An HTTP request failed.
    #[error("http request failed: {0}")]
    Http(String),

    /// The API returned an unparseable response.
    #[error("invalid API response: {0}")]
    BadResponse(String),

    /// The requested provider is unknown.
    #[error("unknown summarizer provider '{0}'")]
    UnknownProvider(String),

    /// A local model backend failed (load / tokenize / generate).
    #[error("summarizer backend: {0}")]
    Backend(String),
}
