//! Store error type.

/// Result alias for store operations.
pub type Result<T> = std::result::Result<T, StoreError>;

/// Errors raised by the DuckDB-backed store.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// A DuckDB operation failed.
    #[error("duckdb: {0}")]
    Duck(#[from] duckdb::Error),

    /// A vector's length did not match the store's configured dimension.
    #[error("vector dimension mismatch: expected {expected}, got {got} (id={id})")]
    DimensionMismatch {
        /// The store's configured dimension.
        expected: usize,
        /// The offending vector's length.
        got: usize,
        /// The point id.
        id: String,
    },

    /// A stored value could not be decoded into the expected Rust type.
    #[error("decode error: {0}")]
    Decode(String),

    /// The database was frozen for the process to end (see `Store::freeze`):
    /// no write may reach the write-ahead log any more.
    #[error("the database is frozen: the server is shutting down and accepts no more writes")]
    Frozen,

    /// An I/O failure (e.g. creating the database directory).
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}
