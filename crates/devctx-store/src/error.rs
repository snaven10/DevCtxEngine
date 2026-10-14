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

    /// A checkpoint was refused because it would invalidate the database: it
    /// holds an HNSW index and the VSS extension is not loaded (an offline
    /// machine that never installed it), which DuckDB fails fatally. The WAL
    /// is left for an open that can load VSS.
    #[error(
        "checkpoint skipped: the database has an HNSW index and the VSS extension is not \
         loaded, so DuckDB would invalidate it; the write-ahead log is kept"
    )]
    CheckpointUnsafe,

    /// The database is being compacted (PLAN-009 TASK-018): its compaction
    /// lock is held, and nothing opens it until the compaction ends.
    #[error(
        "the index at {} is being compacted (`devctx compact`); nothing can open it until that \
         finishes — retry in a moment",
        .0.display()
    )]
    Compacting(std::path::PathBuf),

    /// A compaction's temporary copy sits next to a database that is not
    /// there. It may be the only copy left, so nothing deletes it and no empty
    /// database is created over it (PLAN-009 TASK-018, invariant I5).
    #[error(
        "{} is left from a compaction that did not finish, and the database it was copying \
         ({}) is missing; nothing was deleted and no new index was created. Put the database \
         back, or move the temporary aside once you know it is not needed",
        tmp.display(),
        db.display()
    )]
    OrphanCompaction {
        /// The missing database.
        db: std::path::PathBuf,
        /// The temporary found next to it.
        tmp: std::path::PathBuf,
    },

    /// An I/O failure (e.g. creating the database directory).
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}
