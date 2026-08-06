//! Parse error type.

/// Result alias for parse operations.
pub type Result<T> = std::result::Result<T, ParseError>;

/// Errors raised while building or running a language parser.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    /// A tree-sitter query failed to compile (node kind mismatch, etc.).
    #[error("query compile error for {lang}: {source}")]
    Query {
        /// Language name.
        lang: &'static str,
        /// Underlying tree-sitter query error.
        #[source]
        source: tree_sitter::QueryError,
    },

    /// Setting the grammar on the parser failed.
    #[error("failed to set grammar for {0}")]
    Grammar(&'static str),

    /// tree-sitter returned no tree for the source.
    #[error("failed to parse source as {0}")]
    NoTree(&'static str),
}
