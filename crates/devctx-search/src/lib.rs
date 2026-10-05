//! `devctx-search` — search orchestration: vector, keyword (BM25), or hybrid
//! (Reciprocal Rank Fusion of both), with optional cross-encoder reranking.
//!
//! Centralizes the logic shared by the CLI and the MCP server. See
//! `docs/architecture-spec.md` §6.

use std::collections::HashMap;

use devctx_core::{SearchFilter, SearchResult, VectorPoint};
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

    // Fusion dedups by point id only; the same chunk reached through another
    // branch, or a method next to its own class, still repeats. Collapse those
    // before the pool is cut so `limit` is filled with distinct code.
    let candidates = dedup_hits(candidates);
    let ranked = finalize(candidates, query, limit, reranker)?;

    if mode == SearchMode::Vector {
        return Ok(ranked);
    }
    Ok(anchor_identifiers(store, query, filter, limit, ranked))
}

/// How many definitions one identifier may pin to the front.
const ANCHOR_MAX: usize = 3;

/// The tokens of `query` shaped like an identifier: `snake_case`, `camelCase`
/// or `PascalCase` with an inner capital, or qualified (`Class.method`,
/// `module::item`). Plain words are not — pinning every function called
/// `search` for the query "search" would drown the ranking.
pub fn identifier_tokens(query: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in query.split(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | '.' | ':'))) {
        let tok = raw.trim_matches(|c| c == '.' || c == ':');
        if tok.is_empty()
            || !tok
                .chars()
                .all(|c| c.is_alphanumeric() || "_.:".contains(c))
        {
            continue;
        }
        let starts_ok = tok
            .chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_');
        let qualified = (tok.contains('.') || tok.contains("::"))
            && tok
                .split(['.', ':'])
                .filter(|p| !p.is_empty())
                .all(|p| p.chars().all(|c| c.is_alphanumeric() || c == '_'))
            && tok.split(['.', ':']).filter(|p| !p.is_empty()).count() >= 2;
        let snake = tok.contains('_') && tok.chars().any(|c| c.is_alphanumeric());
        let mut prev_lower = false;
        let mut camel = false;
        for c in tok.chars() {
            if prev_lower && c.is_uppercase() {
                camel = true;
            }
            prev_lower = c.is_lowercase();
        }
        if starts_ok && (qualified || ((snake || camel) && !tok.contains(['.', ':']))) {
            let t = tok.to_string();
            if !out.contains(&t) {
                out.push(t);
            }
        }
    }
    out
}

/// Put the definitions of identifier-shaped query tokens in front of `ranked`.
///
/// BM25 ranks a chunk that *mentions* `do_memories_by_symbol` many times above
/// the one that defines it; an identifier is the one query where the exact
/// definition is known to be what was asked for. Pinned hits take the best
/// score present so the order they were given is the order shown, and the list
/// is cut back to `limit` afterwards, so the caller never gets more than asked.
/// A store without symbols (or an error) leaves the ranking untouched.
fn anchor_identifiers(
    store: &Store,
    query: &str,
    filter: &SearchFilter,
    limit: usize,
    ranked: Vec<SearchResult>,
) -> Vec<SearchResult> {
    let tokens = identifier_tokens(query);
    if tokens.is_empty() {
        return ranked;
    }
    let mut pinned: Vec<VectorPoint> = Vec::new();
    for t in &tokens {
        let found = store
            .symbol_matches(filter, t, ANCHOR_MAX)
            .unwrap_or_default();
        for p in found {
            if !pinned.iter().any(|q| q.id == p.id) {
                pinned.push(p);
            }
        }
    }
    if pinned.is_empty() {
        return ranked;
    }
    let top = ranked
        .iter()
        .map(|h| h.score)
        .fold(f32::MIN, f32::max)
        .max(0.0);
    let mut out: Vec<SearchResult> = pinned
        .into_iter()
        .map(|point| SearchResult { point, score: top })
        .collect();
    out.extend(ranked);
    let mut out = dedup_hits(out);
    out.truncate(limit);
    out
}

/// Collapse repeated chunks, keeping the best-ranked of each (input order is
/// rank order): the same point id; the same `(repo, file, start, end)` — one
/// chunk reached through several branches; and a range contained in one
/// already kept for the same file (a method beside its class), in either
/// direction, so the smaller fragment wins exactly when it ranked higher.
/// Rows without a file (memories) only dedup by id.
pub fn dedup_hits(hits: Vec<SearchResult>) -> Vec<SearchResult> {
    let mut kept: Vec<SearchResult> = Vec::with_capacity(hits.len());
    'next: for hit in hits {
        let m = &hit.point.metadata;
        for k in &kept {
            if k.point.id == hit.point.id {
                continue 'next;
            }
            let km = &k.point.metadata;
            if m.file.is_empty() || km.file != m.file || km.repo != m.repo {
                continue;
            }
            let (a, b) = ((m.start_line, m.end_line), (km.start_line, km.end_line));
            let inside = a.0 >= b.0 && a.1 <= b.1;
            let around = b.0 >= a.0 && b.1 <= a.1;
            if inside || around {
                continue 'next;
            }
        }
        kept.push(hit);
    }
    kept
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

    fn code(
        id: &str,
        branch: &str,
        file: &str,
        range: (i32, i32),
        symbol: &str,
        text: &str,
        axis: [f32; DIM],
    ) -> VectorPoint {
        VectorPoint {
            id: id.into(),
            vector: axis.to_vec(),
            text: text.into(),
            metadata: VectorMetadata {
                repo: "demo".into(),
                branch: branch.into(),
                file: file.into(),
                symbol: symbol.into(),
                symbol_type: "function".into(),
                chunk_level: "function".into(),
                start_line: range.0,
                end_line: range.1,
                ..Default::default()
            },
        }
    }

    /// The same chunk reached through two branches, and a method next to the
    /// class that contains it, used to come back as separate results.
    #[test]
    fn the_same_chunk_is_returned_once() {
        let store = Store::open_in_memory(DIM).unwrap();
        let d = [0.0, 1.0, 0.0, 0.0];
        store
            .upsert(&[
                code("m1", "main", "a.rs", (10, 20), "link", "database link", d),
                code("m2", "dev", "a.rs", (10, 20), "link", "database link", d),
                code(
                    "cls",
                    "main",
                    "b.rs",
                    (1, 50),
                    "Store",
                    "database store class",
                    d,
                ),
                code(
                    "meth",
                    "main",
                    "b.rs",
                    (10, 20),
                    "Store.get",
                    "database get",
                    d,
                ),
                code("other", "main", "c.rs", (1, 5), "pool", "database pool", d),
            ])
            .unwrap();
        let hits = search(
            &store,
            "database",
            &SearchFilter::default(),
            10,
            SearchMode::Vector,
            Some(&KwEmbedder),
            None,
        )
        .unwrap();
        let spans: Vec<_> = hits
            .iter()
            .map(|h| {
                let m = &h.point.metadata;
                (m.file.clone(), m.start_line, m.end_line)
            })
            .collect();
        let mut uniq = spans.clone();
        uniq.sort();
        uniq.dedup();
        assert_eq!(spans.len(), uniq.len(), "repeated spans: {spans:?}");
        assert!(
            !(spans.contains(&("b.rs".into(), 1, 50)) && spans.contains(&("b.rs".into(), 10, 20))),
            "container and contained both returned: {spans:?}"
        );
        assert_eq!(hits.len(), 3, "a.rs once, b.rs once, c.rs once: {spans:?}");
    }

    #[test]
    fn dedup_prefers_the_higher_ranked_of_container_and_contained() {
        let mk = |id: &str, r: (i32, i32)| SearchResult {
            score: 1.0,
            point: code(id, "main", "b.rs", r, id, id, [0.0; DIM]),
        };
        let small_first = dedup_hits(vec![mk("small", (10, 20)), mk("big", (1, 50))]);
        assert_eq!(small_first.len(), 1);
        assert_eq!(small_first[0].point.id, "small");
        let big_first = dedup_hits(vec![mk("big", (1, 50)), mk("small", (10, 20))]);
        assert_eq!(big_first.len(), 1);
        assert_eq!(big_first[0].point.id, "big");
        // Overlap without containment is two different fragments.
        let both = dedup_hits(vec![mk("x", (1, 20)), mk("y", (15, 40))]);
        assert_eq!(both.len(), 2);
    }

    #[test]
    fn identifier_tokens_only_take_identifier_shapes() {
        assert_eq!(
            identifier_tokens("do_memories_by_symbol"),
            ["do_memories_by_symbol"]
        );
        assert_eq!(identifier_tokens("how does fooBar work?"), ["fooBar"]);
        assert_eq!(identifier_tokens("Card.charge now"), ["Card.charge"]);
        assert_eq!(identifier_tokens("pay::charge"), ["pay::charge"]);
        assert!(identifier_tokens("how are memories linked to symbols").is_empty());
        assert!(identifier_tokens("search").is_empty());
        assert!(identifier_tokens("end.").is_empty());
    }

    /// Chunks that merely mention an identifier many times outrank its
    /// definition under BM25; the definition is what was asked for.
    fn anchoring_store() -> Store {
        let store = Store::open_in_memory(DIM).unwrap();
        let mention = "do_memories_by_symbol do_memories_by_symbol do_memories_by_symbol caller";
        store
            .upsert(&[
                code(
                    "c1",
                    "main",
                    "x.rs",
                    (1, 9),
                    "caller_one",
                    mention,
                    [0.0, 1.0, 0.0, 0.0],
                ),
                code(
                    "c2",
                    "main",
                    "y.rs",
                    (1, 9),
                    "caller_two",
                    mention,
                    [0.0, 1.0, 0.0, 0.0],
                ),
                code(
                    "def",
                    "main",
                    "state.rs",
                    (300, 330),
                    "do_memories_by_symbol",
                    "fn do_memories_by_symbol() { lookup }",
                    [0.0, 0.0, 1.0, 0.0],
                ),
            ])
            .unwrap();
        store
    }

    #[test]
    fn an_identifier_query_puts_its_definition_first_in_hybrid() {
        let store = anchoring_store();
        let _ = store.rebuild_fts().unwrap();
        let hits = search(
            &store,
            "do_memories_by_symbol",
            &SearchFilter::default(),
            5,
            SearchMode::Hybrid,
            Some(&KwEmbedder),
            None,
        )
        .unwrap();
        assert_eq!(hits[0].point.id, "def", "got {:?}", ids(&hits));
        assert_eq!(hits.iter().filter(|h| h.point.id == "def").count(), 1);
    }

    #[test]
    fn an_identifier_query_puts_its_definition_first_in_keyword() {
        let store = anchoring_store();
        if !store.rebuild_fts().unwrap() {
            return; // FTS extension unavailable.
        }
        let hits = search(
            &store,
            "do_memories_by_symbol",
            &SearchFilter::default(),
            5,
            SearchMode::Keyword,
            None,
            None,
        )
        .unwrap();
        assert_eq!(hits[0].point.id, "def", "got {:?}", ids(&hits));
    }

    #[test]
    fn a_plain_word_query_is_not_anchored() {
        let store = anchoring_store();
        let _ = store.rebuild_fts().unwrap();
        let hits = search(
            &store,
            "caller",
            &SearchFilter::default(),
            5,
            SearchMode::Hybrid,
            Some(&KwEmbedder),
            None,
        )
        .unwrap();
        assert_ne!(hits[0].point.id, "def");
    }

    fn ids(hits: &[SearchResult]) -> Vec<String> {
        hits.iter().map(|h| h.point.id.clone()).collect()
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
