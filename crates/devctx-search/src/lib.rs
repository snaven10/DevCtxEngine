//! `devctx-search` — search orchestration: vector, keyword (BM25), or hybrid
//! (Reciprocal Rank Fusion of both), with optional cross-encoder reranking.
//!
//! Centralizes the logic shared by the CLI and the MCP server. See
//! `docs/architecture-spec.md` §6.

use std::collections::HashMap;

use devctx_core::{SearchFilter, SearchResult};
use devctx_embed::EmbeddingProvider;
use devctx_rerank::Reranker;
use devctx_store::Store;

/// Candidate pool fetched from each retriever when the retriever order is final.
const POOL: usize = 20;

/// RRF constant (standard default).
const RRF_K: f32 = 60.0;

/// Which retrieval strategy to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    /// Semantic vector search.
    Vector,
    /// BM25 keyword search.
    Keyword,
    /// Reciprocal-rank fusion of vector + keyword.
    Hybrid,
}

/// Search errors.
#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    /// A store operation failed.
    #[error(transparent)]
    Store(#[from] devctx_store::StoreError),
    /// An embedding operation failed.
    #[error(transparent)]
    Embed(#[from] devctx_embed::EmbedError),
    /// A rerank operation failed.
    #[error(transparent)]
    Rerank(#[from] devctx_rerank::RerankError),
    /// Vector/hybrid search requested without an embedder.
    #[error("an embedder is required for {0:?} search")]
    MissingEmbedder(SearchMode),
}

/// Result alias.
pub type Result<T> = std::result::Result<T, SearchError>;

/// Run a search in the requested mode.
///
/// `embedder` is required for `Vector`/`Hybrid`. `reranker` (if `Some`) reorders
/// the candidate pool down to `limit`; otherwise the retriever order is truncated.
pub fn search(
    store: &Store,
    query: &str,
    filter: &SearchFilter,
    limit: usize,
    mode: SearchMode,
    embedder: Option<&dyn EmbeddingProvider>,
    reranker: Option<&dyn Reranker>,
) -> Result<Vec<SearchResult>> {
    // A reranker asks for the pool it can afford; everything else takes the
    // shallow one, since without reordering a deeper fetch is thrown away.
    //
    // The pool is the ceiling on everything reranking could ever fix — it
    // reorders what it is handed and nothing else. Measured here, the chunk
    // answering a behaviour question sits at rank 27–52, so a pool of 20 meant
    // no model, however good, was ever shown it.
    let pool = limit.max(reranker.map_or(POOL, |r| r.pool().max(POOL)));
    let candidates = match mode {
        SearchMode::Keyword => store.keyword_search(query, filter, pool)?,
        SearchMode::Vector => {
            let emb = embedder.ok_or(SearchError::MissingEmbedder(mode))?;
            store.search(&emb.embed_query(query)?, filter, pool)?
        }
        SearchMode::Hybrid => {
            let emb = embedder.ok_or(SearchError::MissingEmbedder(mode))?;
            let vector = store.search(&emb.embed_query(query)?, filter, pool)?;
            // Degrade to vector-only if the FTS index isn't built.
            let keyword = store
                .keyword_search(query, filter, pool)
                .unwrap_or_default();
            reciprocal_rank_fusion(&[vector, keyword], RRF_K)
        }
    };

    finalize(candidates, query, limit, reranker)
}

/// Rerank the candidate pool down to `limit`, or truncate when no reranker.
fn finalize(
    candidates: Vec<SearchResult>,
    query: &str,
    limit: usize,
    reranker: Option<&dyn Reranker>,
) -> Result<Vec<SearchResult>> {
    match reranker {
        Some(r) => {
            let texts: Vec<String> = candidates.iter().map(|h| h.point.text.clone()).collect();
            Ok(r.rerank(query, &texts, limit)?
                .into_iter()
                .map(|ranked| {
                    let mut hit = candidates[ranked.index].clone();
                    hit.score = ranked.score;
                    hit
                })
                .collect())
        }
        None => {
            let mut out = candidates;
            out.truncate(limit);
            Ok(out)
        }
    }
}

/// Fuse ranked lists by Reciprocal Rank Fusion: each item scores
/// `Σ 1/(k + rank)` across the lists it appears in (rank is 1-based). Results are
/// deduplicated by point id and ordered by fused score (highest first).
pub fn reciprocal_rank_fusion(lists: &[Vec<SearchResult>], k: f32) -> Vec<SearchResult> {
    let mut scores: HashMap<String, f32> = HashMap::new();
    let mut repr: HashMap<String, SearchResult> = HashMap::new();
    for list in lists {
        for (rank, hit) in list.iter().enumerate() {
            let id = hit.point.id.clone();
            *scores.entry(id.clone()).or_insert(0.0) += 1.0 / (k + rank as f32 + 1.0);
            repr.entry(id).or_insert_with(|| hit.clone());
        }
    }
    let mut out: Vec<SearchResult> = repr
        .into_iter()
        .map(|(id, mut hit)| {
            hit.score = scores[&id];
            hit
        })
        .collect();
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use devctx_core::{VectorMetadata, VectorPoint};
    use devctx_embed::{EmbeddingProvider, Result as EmbedResult};

    const DIM: usize = 4;
    const KEYWORDS: [&str; 4] = ["auth", "database", "cache", "test"];

    fn hit(id: &str, score: f32) -> SearchResult {
        SearchResult {
            score,
            point: VectorPoint {
                id: id.into(),
                vector: vec![],
                text: id.into(),
                metadata: VectorMetadata::default(),
            },
        }
    }

    #[test]
    fn rrf_favors_items_in_both_lists() {
        // `b` is rank 2 in list1 and rank 1 in list2 → beats rank-1-only items.
        let list1 = vec![hit("a", 0.9), hit("b", 0.8), hit("c", 0.7)];
        let list2 = vec![hit("b", 5.0), hit("d", 4.0)];
        let fused = reciprocal_rank_fusion(&[list1, list2], 60.0);
        assert_eq!(fused[0].point.id, "b");
        // All unique ids present.
        assert_eq!(fused.len(), 4);
    }

    struct KwEmbedder;
    impl EmbeddingProvider for KwEmbedder {
        fn embed(&self, texts: &[String]) -> EmbedResult<Vec<Vec<f32>>> {
            Ok(texts
                .iter()
                .map(|t| {
                    let lc = t.to_lowercase();
                    let mut v: Vec<f32> = KEYWORDS
                        .iter()
                        .map(|k| if lc.contains(k) { 1.0 } else { 0.0 })
                        .collect();
                    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
                    if n > 0.0 {
                        v.iter_mut().for_each(|x| *x /= n);
                    }
                    v
                })
                .collect())
        }
        fn dimension(&self) -> usize {
            DIM
        }
        fn model_name(&self) -> &str {
            "kw"
        }
    }

    fn point(id: &str, text: &str, kw_vec: [f32; DIM]) -> VectorPoint {
        VectorPoint {
            id: id.into(),
            vector: kw_vec.to_vec(),
            text: text.into(),
            metadata: VectorMetadata {
                repo: "demo".into(),
                branch: "main".into(),
                ..Default::default()
            },
        }
    }

    /// Counts what it was handed, and promotes the last candidate — standing in
    /// for a cross-encoder that finds the answer deep in the pool.
    struct Recording(std::sync::Mutex<usize>);
    impl devctx_rerank::Reranker for Recording {
        fn rerank(
            &self,
            _query: &str,
            candidates: &[String],
            top_k: usize,
        ) -> devctx_rerank::Result<Vec<devctx_rerank::Ranked>> {
            *self.0.lock().unwrap() = candidates.len();
            Ok((0..candidates.len())
                .rev()
                .take(top_k)
                .map(|index| devctx_rerank::Ranked { index, score: 1.0 })
                .collect())
        }
        fn name(&self) -> &str {
            "recording"
        }

        fn pool(&self) -> usize {
            100
        }
    }

    /// A cross-encoder can only reorder what it is given, so the pool is the
    /// ceiling on everything it could ever fix. Handing it `limit` candidates
    /// made reranking structurally unable to help: the chunk that answers a
    /// behaviour question sits well past the first twenty, and no model can
    /// promote what it was never shown.
    #[test]
    fn the_reranker_is_given_a_pool_deeper_than_the_limit() {
        let store = Store::open_in_memory(DIM).unwrap();
        let points: Vec<VectorPoint> = (0..60)
            .map(|i| point(&format!("p{i}"), "database pool", [0.0, 1.0, 0.0, 0.0]))
            .collect();
        store.upsert(&points).unwrap();

        let seen = Recording(std::sync::Mutex::new(0));
        let hits = search(
            &store,
            "database",
            &SearchFilter::default(),
            5,
            SearchMode::Vector,
            Some(&KwEmbedder),
            Some(&seen),
        )
        .unwrap();

        assert_eq!(hits.len(), 5, "still returns what was asked for");
        let n = *seen.0.lock().unwrap();
        assert!(
            n > POOL,
            "the reranker saw {n} candidates, no deeper than the un-reranked pool"
        );
        assert_eq!(n, 60, "every stored chunk fits inside the rerank pool here");
    }

    #[test]
    fn hybrid_search_fuses_vector_and_keyword() {
        let store = Store::open_in_memory(DIM).unwrap();
        store
            .upsert(&[
                point("a", "authentication and login tokens", [1.0, 0.0, 0.0, 0.0]),
                point("b", "the database connection pool", [0.0, 1.0, 0.0, 0.0]),
                point("c", "unrelated helper utility", [0.0, 0.0, 1.0, 0.0]),
            ])
            .unwrap();
        let fts = store.rebuild_fts().unwrap();

        // Query embeds to the "database" axis; text contains "database".
        let hits = search(
            &store,
            "database",
            &SearchFilter::default(),
            5,
            SearchMode::Hybrid,
            Some(&KwEmbedder),
            None,
        )
        .unwrap();
        assert!(!hits.is_empty());
        assert_eq!(hits[0].point.id, "b");
        if fts {
            // Keyword list also surfaced `b`, so it should dominate the fusion.
            assert!(hits.iter().any(|h| h.point.id == "b"));
        }
    }

    #[test]
    fn keyword_mode_needs_no_embedder() {
        let store = Store::open_in_memory(DIM).unwrap();
        store
            .upsert(&[point("a", "database pool", [0.0, 1.0, 0.0, 0.0])])
            .unwrap();
        if !store.rebuild_fts().unwrap() {
            return;
        }
        let hits = search(
            &store,
            "database",
            &SearchFilter::default(),
            5,
            SearchMode::Keyword,
            None,
            None,
        )
        .unwrap();
        assert_eq!(hits[0].point.id, "a");
    }

    #[test]
    fn vector_without_embedder_errors() {
        let store = Store::open_in_memory(DIM).unwrap();
        let err = search(
            &store,
            "q",
            &SearchFilter::default(),
            5,
            SearchMode::Vector,
            None,
            None,
        )
        .unwrap_err();
        assert!(matches!(err, SearchError::MissingEmbedder(_)));
    }
}
