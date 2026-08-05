//! The reranker abstraction and the no-op fallback.

use crate::error::Result;

/// A reranked candidate: its original index and the cross-encoder score.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ranked {
    /// Index into the `candidates` slice passed to [`Reranker::rerank`].
    pub index: usize,
    /// Relevance score (higher is better).
    pub score: f32,
}

/// A cross-encoder that reorders candidates by relevance to a query.
pub trait Reranker: Send + Sync {
    /// Rerank `candidates` against `query`, returning at most `top_k` results
    /// ordered best-first.
    fn rerank(&self, query: &str, candidates: &[String], top_k: usize) -> Result<Vec<Ranked>>;

    /// Human-readable model identifier.
    fn name(&self) -> &str;
}

/// A reranker that preserves the input order (used when reranking is disabled or
/// the local backend is unavailable).
pub struct NoopReranker;

impl Reranker for NoopReranker {
    fn rerank(&self, _query: &str, candidates: &[String], top_k: usize) -> Result<Vec<Ranked>> {
        let n = candidates.len();
        Ok((0..n.min(top_k))
            .map(|i| Ranked {
                index: i,
                // Descending so the original order is preserved by score.
                score: (n - i) as f32,
            })
            .collect())
    }

    fn name(&self) -> &str {
        "noop"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_preserves_order_and_truncates() {
        let cands: Vec<String> = ["a", "b", "c", "d"].iter().map(|s| s.to_string()).collect();
        let out = NoopReranker.rerank("q", &cands, 2).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].index, 0);
        assert_eq!(out[1].index, 1);
        assert!(out[0].score > out[1].score);
    }

    #[test]
    fn noop_handles_empty() {
        assert!(NoopReranker.rerank("q", &[], 5).unwrap().is_empty());
    }
}
