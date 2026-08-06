//! Shared domain types for vectors, chunks and search.
//!
//! These mirror the canonical vector-record schema used by the legacy
//! LanceDB/Qdrant stores (see `docs/rust-rewrite-plan.md` §4.1) so the DuckDB
//! store can preserve exact parity of IDs, metadata columns and semantics.

use serde::{Deserialize, Serialize};

/// Metadata carried alongside every vector. One-to-one with the columns of the
/// `vectors` table (minus `id`, `text`, `vector`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VectorMetadata {
    /// Short repo name (basename).
    pub repo: String,
    /// Git branch.
    pub branch: String,
    /// Commit the record was indexed at.
    pub commit: String,
    /// Source file path (repo-relative).
    pub file: String,
    /// Symbol name, or memory title for memory rows.
    pub symbol: String,
    /// `function`/`method`/`class`/… or the memory type.
    pub symbol_type: String,
    /// Programming/markup language.
    pub language: String,
    /// 1-based start line.
    pub start_line: i32,
    /// 1-based end line.
    pub end_line: i32,
    /// `file`/`class`/`function`/`block`/`memory`/`memory_chunk`.
    pub chunk_level: String,
    /// sha256[:16] of the chunk text; drives incremental skip + sync LWW.
    pub content_hash: String,
    /// Tombstone marker.
    pub is_deletion: bool,
    /// Memory type (insight/decision/note/bug/architecture/pattern/discovery).
    pub memory_type: String,
    /// Memory scope (`shared`/`local`).
    pub memory_scope: String,
    /// Comma-separated memory tags.
    pub memory_tags: String,
    /// ISO-8601 indexing timestamp.
    pub indexed_at: String,
}

/// A single embedded chunk: id + vector + metadata + source text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorPoint {
    /// Deterministic id: code = `sha256("{repo}:{branch}:{file}:{start_line}")[:32]`;
    /// memory = `mem_<hash[:24]>` (+ `_c{n}` for body chunks).
    pub id: String,
    /// Embedding vector; its length must equal the store's dimension.
    pub vector: Vec<f32>,
    /// Full chunk text (the reranker reads this).
    pub text: String,
    /// Associated metadata.
    pub metadata: VectorMetadata,
}

/// A search hit: the stored point plus a similarity score (higher = closer).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchResult {
    /// The matched point.
    pub point: VectorPoint,
    /// Cosine similarity in `[-1, 1]` (`1 - cosine_distance`).
    pub score: f32,
}

/// Equality filters applied to a vector search (all `Some` fields must match).
///
/// Scalar fields become SQL `=`; `languages` becomes `IN (...)`.
#[derive(Debug, Clone, Default)]
pub struct SearchFilter {
    /// Restrict to a repo.
    pub repo: Option<String>,
    /// Restrict to a branch.
    pub branch: Option<String>,
    /// Restrict to one or more languages.
    pub languages: Vec<String>,
    /// Restrict to a chunk level.
    pub chunk_level: Option<String>,
    /// Restrict to one or more chunk levels (`IN (...)`); use for memory recall
    /// (`memory` + `memory_chunk`).
    pub chunk_levels: Vec<String>,
    /// Restrict to a memory type.
    pub memory_type: Option<String>,
    /// Exclude tombstoned rows when `true`.
    pub exclude_deletions: bool,
}
