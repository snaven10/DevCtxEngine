//! Chunk type, tokenizer estimate, content hashing and chunker config.

use sha2::{Digest, Sha256};
use std::fmt::Write as _;

/// A chunk of source ready to be embedded. Mirrors the legacy `CodeChunk`
/// (minus the vector, which is produced later in the index pipeline).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    /// Chunk text (context header + body).
    pub text: String,
    /// Chunk level: `file` / `class` / `function` / `block`.
    pub level: String,
    /// Symbol name this chunk represents (empty for the file chunk).
    pub symbol_name: String,
    /// Finer-grained symbol kind (`function`/`method`/`class`/…).
    pub symbol_type: String,
    /// 1-based start line.
    pub start_line: u32,
    /// 1-based end line.
    pub end_line: u32,
    /// `# file > class > method` context header (empty for file/class chunks).
    pub context_header: String,
    /// sha256[:16] of `text`.
    pub content_hash: String,
}

impl Chunk {
    /// Construct a chunk, computing its `content_hash` from `text`.
    pub fn new(
        text: String,
        level: &str,
        symbol_name: impl Into<String>,
        symbol_type: impl Into<String>,
        start_line: u32,
        end_line: u32,
        context_header: String,
    ) -> Self {
        let content_hash = content_hash(&text);
        Self {
            text,
            level: level.to_string(),
            symbol_name: symbol_name.into(),
            symbol_type: symbol_type.into(),
            start_line,
            end_line,
            context_header,
            content_hash,
        }
    }
}

/// Configuration for the semantic chunker (defaults mirror the legacy values).
#[derive(Debug, Clone, Copy)]
pub struct ChunkConfig {
    /// Target upper bound (tokens) before a function is split into blocks.
    pub max_chunk_tokens: usize,
    /// Below this, symbols are grouped together into one chunk.
    pub min_chunk_tokens: usize,
    /// Functions above this (tokens) are split into block-level chunks.
    pub large_function_threshold: usize,
}

impl Default for ChunkConfig {
    fn default() -> Self {
        Self {
            max_chunk_tokens: 512,
            min_chunk_tokens: 64,
            large_function_threshold: 1024,
        }
    }
}

/// Rough token estimate: ~4 characters per token (matches the legacy heuristic).
pub fn estimate_tokens(text: &str) -> usize {
    text.chars().count() / 4
}

/// sha256 of `text`, truncated to the first 16 hex characters (8 bytes).
pub fn content_hash(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    let mut s = String::with_capacity(16);
    for b in &digest[..8] {
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_hash_is_16_hex_and_stable() {
        let h = content_hash("hello world");
        assert_eq!(h.len(), 16);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(h, content_hash("hello world"));
        assert_ne!(h, content_hash("hello world!"));
    }

    #[test]
    fn token_estimate() {
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens(&"x".repeat(400)), 100);
    }
}
