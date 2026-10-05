//! `devctx-search` — search orchestration: vector, keyword (BM25), or hybrid
//! (Reciprocal Rank Fusion of both), with optional cross-encoder reranking.
//!
//! Centralizes the logic shared by the CLI and the MCP server. See
//! `docs/architecture-spec.md` §6.

use std::collections::HashMap;

use devctx_core::{path_kind, KindPenalty, PathKind, SearchFilter, SearchResult, VectorPoint};
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

/// How the kind of file a hit lives in shapes the answer (PLAN-008 D3).
///
/// Two separate levers: `penalty` demotes tests / docs / config without
/// dropping them (specs and READMEs otherwise dominate an unfiltered search);
/// `kind` / `include_tests` are the explicit hard filters an agent asks for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RankOptions {
    /// Keep only hits of this kind.
    pub kind: Option<PathKind>,
    /// `false` drops [`PathKind::Test`] hits (ignored when `kind` is set: an
    /// explicit `kind` wins).
    pub include_tests: bool,
    /// Per-kind score multipliers.
    pub penalty: KindPenalty,
}

impl Default for RankOptions {
    fn default() -> Self {
        RankOptions {
            kind: None,
            include_tests: true,
            penalty: KindPenalty::default(),
        }
    }
}

impl RankOptions {
    /// Is a hard filter in play (so the candidate pool must be fetched deeper)?
    fn filters(&self) -> bool {
        self.kind.is_some() || !self.include_tests
    }

    /// Does a hit of this kind survive the hard filters?
    pub fn keeps(&self, kind: PathKind) -> bool {
        match self.kind {
            Some(k) => k == kind,
            None => self.include_tests || kind != PathKind::Test,
        }
    }
}

/// The caller's `kind` / `include_tests` request, as it arrives from a tool or a
/// flag (strings, optional) before it is validated into [`RankOptions`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KindSel {
    /// `code`, `test`, `doc` or `config`.
    pub kind: Option<String>,
    /// `Some(false)` drops tests.
    pub include_tests: Option<bool>,
}

impl KindSel {
    /// Validate into ranking options with the configured penalty.
    pub fn options(&self, penalty: KindPenalty) -> std::result::Result<RankOptions, String> {
        let kind = match self
            .kind
            .as_deref()
            .map(str::trim)
            .filter(|k| !k.is_empty())
        {
            None => None,
            Some(k) => Some(PathKind::parse(k).ok_or_else(|| {
                format!("`kind` must be one of code, test, doc, config (got `{k}`)")
            })?),
        };
        Ok(RankOptions {
            kind,
            include_tests: self.include_tests.unwrap_or(true),
            penalty,
        })
    }

    /// The `devctx search` flags that carry this selection to a child process.
    pub fn cli_args(&self) -> Vec<String> {
        let mut a = Vec::new();
        if let Some(k) = self.kind.as_deref().filter(|k| !k.trim().is_empty()) {
            a.extend(["--kind".to_string(), k.trim().to_string()]);
        }
        if self.include_tests == Some(false) {
            a.push("--no-tests".to_string());
        }
        a
    }
}

/// The kind of file a hit comes from.
fn hit_kind(h: &SearchResult) -> PathKind {
    path_kind(&h.point.metadata.file, &h.point.metadata.language)
}

/// Candidates fetched per retriever when a hard filter discards a share of
/// them after the fact. The store cannot filter by kind (it is a function of
/// path *and* language), so the pool is widened instead: a filter that keeps
/// one hit in twenty still finds `limit` of them.
const FILTERED_POOL: usize = 400;

/// Multiply scores by the kind's factor and re-sort (stable). A negative score
/// (cross-encoder logits, cosine below zero) is divided instead, so a penalty
/// always moves a hit *down* whatever the sign. A no-op when nothing is
/// penalised, so an already-ordered list is never reshuffled.
fn apply_penalty(mut hits: Vec<SearchResult>, penalty: &KindPenalty) -> Vec<SearchResult> {
    let mut touched = false;
    for h in &mut hits {
        let f = penalty.factor(hit_kind(h));
        if f != 1.0 && f > 0.0 {
            h.score = if h.score >= 0.0 {
                h.score * f
            } else {
                h.score / f
            };
            touched = true;
        }
    }
    if touched {
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }
    hits
}

/// Run a search in the requested mode, with the default kind penalty and no
/// kind filter. See [`search_ranked`].
pub fn search(
    store: &Store,
    query: &str,
    filter: &SearchFilter,
    limit: usize,
    mode: SearchMode,
    embedder: Option<&dyn EmbeddingProvider>,
    reranker: Option<&dyn Reranker>,
) -> Result<Vec<SearchResult>> {
    search_ranked(
        store,
        query,
        filter,
        limit,
        mode,
        embedder,
        reranker,
        &RankOptions::default(),
    )
}

/// Run a search in the requested mode.
///
/// `embedder` is required for `Vector`/`Hybrid`. `reranker` (if `Some`) reorders
/// the candidate pool down to `limit`; otherwise the retriever order is truncated.
#[allow(clippy::too_many_arguments)]
pub fn search_ranked(
    store: &Store,
    query: &str,
    filter: &SearchFilter,
    limit: usize,
    mode: SearchMode,
    embedder: Option<&dyn EmbeddingProvider>,
    reranker: Option<&dyn Reranker>,
    opts: &RankOptions,
) -> Result<Vec<SearchResult>> {
    // A reranker asks for the pool it can afford; everything else takes the
    // shallow one, since without reordering a deeper fetch is thrown away.
    //
    // The pool is the ceiling on everything reranking could ever fix — it
    // reorders what it is handed and nothing else. Measured here, the chunk
    // answering a behaviour question sits at rank 27–52, so a pool of 20 meant
    // no model, however good, was ever shown it.
    //
    // Twice `limit` at least: the dedup below drops repeated chunks *before*
    // the cut, and a pool of exactly `limit` would hand back fewer results than
    // asked for, silently. The hard filters discard after the fetch too, so
    // they get the same margin on top of their own floor.
    let mut pool = (limit.saturating_mul(2)).max(reranker.map_or(POOL, |r| r.pool().max(POOL)));
    if opts.filters() {
        pool = pool.max(FILTERED_POOL).max(limit.saturating_mul(4));
    }
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
    //
    // The hard filters come first (they define the candidate set), then the
    // kind penalty (so the order the dedup and the reranker see is the
    // penalised one). The penalty is applied to the rerank output too, which
    // is why the reranker is asked for the whole pool and the cut to `limit`
    // happens afterwards.
    let mut candidates = candidates;
    if opts.filters() {
        candidates.retain(|h| opts.keeps(hit_kind(h)));
    }
    let candidates = dedup_hits(apply_penalty(candidates, &opts.penalty));
    let ranked = match reranker {
        Some(_) => {
            let all = candidates.len();
            let mut r = apply_penalty(finalize(candidates, query, all, reranker)?, &opts.penalty);
            r.truncate(limit);
            r
        }
        None => finalize(candidates, query, limit, None)?,
    };

    if mode == SearchMode::Vector {
        return Ok(ranked);
    }
    Ok(anchor_identifiers(
        store, query, filter, limit, ranked, opts,
    ))
}

/// How many definitions one identifier may pin to the front.
const ANCHOR_MAX: usize = 3;

/// How many identifier tokens of one query are looked up. Each costs one or
/// two queries over `vectors`; a pasted stack trace names dozens.
const ANCHOR_TOKENS: usize = 3;

/// Extensions that make `name.ext` a file name rather than `Type.member`.
const FILE_EXTENSIONS: &[&str] = &[
    "rs",
    "md",
    "markdown",
    "txt",
    "rst",
    "adoc",
    "toml",
    "yaml",
    "yml",
    "json",
    "jsonc",
    "lock",
    "xml",
    "html",
    "htm",
    "css",
    "scss",
    "sass",
    "less",
    "js",
    "mjs",
    "cjs",
    "jsx",
    "ts",
    "tsx",
    "vue",
    "svelte",
    "py",
    "pyi",
    "go",
    "java",
    "kt",
    "kts",
    "scala",
    "groovy",
    "gradle",
    "c",
    "h",
    "cc",
    "cpp",
    "cxx",
    "hpp",
    "hh",
    "cs",
    "fs",
    "swift",
    "m",
    "mm",
    "rb",
    "php",
    "pl",
    "lua",
    "sh",
    "bash",
    "zsh",
    "ps1",
    "bat",
    "sql",
    "proto",
    "graphql",
    "gql",
    "ini",
    "cfg",
    "conf",
    "env",
    "properties",
    "csv",
    "tsv",
    "log",
    "pdf",
    "png",
    "jpg",
    "jpeg",
    "svg",
    "gif",
    "duckdb",
    "db",
    "sqlite",
    "wasm",
    "dart",
    "ex",
    "exs",
    "erl",
    "hs",
    "ml",
    "clj",
    "zig",
    "nim",
    "r",
    "jl",
    "tf",
    "hcl",
    "dockerfile",
    "mk",
    "cmake",
];

/// Is this dotted token a file name (`state.rs`), a version (`v0.8.4`) or an
/// abbreviation (`e.g`, `i.e`) rather than a qualified identifier?
fn dotted_non_identifier(tok: &str) -> bool {
    if !tok.contains('.') || tok.contains("::") {
        return false;
    }
    let parts: Vec<&str> = tok.split('.').filter(|p| !p.is_empty()).collect();
    let last = parts
        .last()
        .map(|p| p.to_ascii_lowercase())
        .unwrap_or_default();
    if FILE_EXTENSIONS.contains(&last.as_str()) {
        return true;
    }
    // A version: `v0`, `8`, `4` — a segment that is (v +) digits.
    let numeric = |p: &str| {
        let d = p.strip_prefix(['v', 'V']).unwrap_or(p);
        !d.is_empty() && d.chars().all(|c| c.is_ascii_digit())
    };
    if parts.iter().skip(1).any(|p| numeric(p)) || parts.first().is_some_and(|p| numeric(p)) {
        return true;
    }
    // Abbreviations: every segment one letter.
    parts.iter().all(|p| p.chars().count() == 1)
}

/// The tokens of `query` shaped like an identifier: `snake_case`, `camelCase`
/// or `PascalCase` with an inner capital, or qualified (`Class.method`,
/// `module::item`). Plain words are not — pinning every function called
/// `search` for the query "search" would drown the ranking. Neither are file
/// names (`state.rs`, whose `file` chunk carries the basename as its symbol),
/// versions (`v0.8.4`) or abbreviations (`e.g`).
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
        if dotted_non_identifier(tok) {
            continue;
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
///
/// Bounded twice: only the first [`ANCHOR_TOKENS`] identifiers are looked up,
/// and pinned definitions take at most half of `limit` (at least one), so the
/// retrievers' own ranking always keeps the rest of the answer.
fn anchor_identifiers(
    store: &Store,
    query: &str,
    filter: &SearchFilter,
    limit: usize,
    ranked: Vec<SearchResult>,
    opts: &RankOptions,
) -> Vec<SearchResult> {
    let mut tokens = identifier_tokens(query);
    tokens.truncate(ANCHOR_TOKENS);
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
    // A definition is never demoted by the penalty — the query named it — but
    // an explicit hard filter still applies to it.
    pinned.retain(|p| opts.keeps(path_kind(&p.metadata.file, &p.metadata.language)));
    pinned.truncate((limit / 2).max(1));
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

/// Chunk levels that summarise a range rather than hold its code: the `file`
/// chunk (imports + symbol list, spanning every line), the `class` chunk
/// (signature + method names, spanning every method) and the `doc` chunk
/// (prose above a symbol). Their range says where they point, not what they
/// contain, so they never stand in for — or swallow — the code inside it.
fn is_summary(level: &str) -> bool {
    matches!(level, "file" | "class" | "doc")
}

/// Collapse repeated chunks, keeping the best-ranked of each (input order is
/// rank order): the same point id; the same `(repo, file, start, end)` at the
/// same chunk level — one chunk reached through several branches; and, between
/// *content* chunks only, a range contained in one already kept for the same
/// file (a nested function beside its parent), in either direction, so the
/// smaller fragment wins exactly when it ranked higher.
///
/// Summary chunks ([`is_summary`]) take no part in containment: a `file`
/// chunk spans lines `1..N` and, ranked first, would otherwise drop every
/// function of the file as "contained" — the reader gets the table of
/// contents instead of the code. The smaller, real chunk always survives.
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
            if a == b && m.chunk_level == km.chunk_level {
                continue 'next;
            }
            if is_summary(&m.chunk_level) || is_summary(&km.chunk_level) {
                continue;
            }
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
        if !require_fts(&store, "keyword_mode_needs_no_embedder") {
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
        if !require_fts(
            &store,
            "an_identifier_query_puts_its_definition_first_in_keyword",
        ) {
            return;
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

    /// Same query, three files; the noisy ones are *closer* to the query than
    /// the code, so only the penalty can put the code first.
    fn noisy_store() -> Store {
        let store = Store::open_in_memory(DIM).unwrap();
        let exact = [0.0, 1.0, 0.0, 0.0];
        let near = [0.0, 0.95, 0.3, 0.0];
        store
            .upsert(&[
                code(
                    "spec",
                    "main",
                    "src/test/DbPoolTest.java",
                    (1, 9),
                    "t",
                    "database pool test",
                    exact,
                ),
                code(
                    "readme",
                    "main",
                    "README.md",
                    (1, 9),
                    "",
                    "database pool readme",
                    exact,
                ),
                code(
                    "sql",
                    "main",
                    "db/schema.sql",
                    (1, 9),
                    "",
                    "database pool ddl",
                    exact,
                ),
                code(
                    "code",
                    "main",
                    "src/db/pool.rs",
                    (1, 9),
                    "pool",
                    "database pool",
                    near,
                ),
            ])
            .unwrap();
        store
    }

    fn run_ranked(store: &Store, opts: &RankOptions) -> Vec<String> {
        ids(&search_ranked(
            store,
            "database",
            &SearchFilter::default(),
            10,
            SearchMode::Vector,
            Some(&KwEmbedder),
            None,
            opts,
        )
        .unwrap())
    }

    /// Specs, READMEs and SQL used to outrank the code that answers the
    /// question. They are demoted by default, not dropped.
    #[test]
    fn tests_docs_and_config_rank_below_code_but_stay_in_the_results() {
        let store = noisy_store();
        let hits = ids(&search(
            &store,
            "database",
            &SearchFilter::default(),
            10,
            SearchMode::Vector,
            Some(&KwEmbedder),
            None,
        )
        .unwrap());
        assert_eq!(hits[0], "code", "got {hits:?}");
        assert_eq!(hits.len(), 4, "nothing is excluded: {hits:?}");

        // Back to 1.0 and the raw similarity order returns: the factor is the
        // only thing that moved them.
        let flat = RankOptions {
            penalty: KindPenalty::NONE,
            ..Default::default()
        };
        assert_ne!(run_ranked(&store, &flat)[0], "code");
    }

    #[test]
    fn the_penalty_values_are_configurable_per_kind() {
        let store = noisy_store();
        // Only tests demoted: README and SQL keep their raw lead over the code.
        let only_tests = RankOptions {
            penalty: KindPenalty {
                test: 0.5,
                doc: 1.0,
                config: 1.0,
            },
            ..Default::default()
        };
        let hits = run_ranked(&store, &only_tests);
        let pos = |id: &str| hits.iter().position(|h| h == id).unwrap();
        assert!(
            pos("readme") < pos("code") && pos("sql") < pos("code"),
            "{hits:?}"
        );
        assert!(pos("spec") > pos("code"), "{hits:?}");
    }

    #[test]
    fn a_kind_filter_keeps_only_that_kind() {
        let store = noisy_store();
        for (kind, want) in [
            (PathKind::Test, "spec"),
            (PathKind::Doc, "readme"),
            (PathKind::Config, "sql"),
            (PathKind::Code, "code"),
        ] {
            let opts = RankOptions {
                kind: Some(kind),
                ..Default::default()
            };
            assert_eq!(
                run_ranked(&store, &opts),
                vec![want.to_string()],
                "{kind:?}"
            );
        }
    }

    #[test]
    fn include_tests_false_drops_tests_only() {
        let store = noisy_store();
        let opts = RankOptions {
            include_tests: false,
            ..Default::default()
        };
        let hits = run_ranked(&store, &opts);
        assert!(!hits.contains(&"spec".to_string()), "{hits:?}");
        assert_eq!(hits.len(), 3, "{hits:?}");
        // An explicit kind wins over include_tests.
        let both = RankOptions {
            kind: Some(PathKind::Test),
            include_tests: false,
            ..Default::default()
        };
        assert_eq!(run_ranked(&store, &both), vec!["spec".to_string()]);
    }

    /// A filter that keeps one hit in a hundred still fills `limit`: the pool
    /// is fetched deeper when a filter is in play.
    #[test]
    fn a_selective_filter_looks_past_the_default_pool() {
        let store = Store::open_in_memory(DIM).unwrap();
        let d = [0.0, 1.0, 0.0, 0.0];
        let mut pts: Vec<VectorPoint> = (0..60)
            .map(|i| {
                code(
                    &format!("c{i}"),
                    "main",
                    &format!("src/m{i}.rs"),
                    (1, 9),
                    "f",
                    "database",
                    d,
                )
            })
            .collect();
        pts.push(code(
            "t",
            "main",
            "tests/db.rs",
            (1, 9),
            "t",
            "database",
            [0.0, 0.5, 0.8, 0.0],
        ));
        store.upsert(&pts).unwrap();
        let opts = RankOptions {
            kind: Some(PathKind::Test),
            ..Default::default()
        };
        assert_eq!(run_ranked(&store, &opts), vec!["t".to_string()]);
    }

    /// An identifier query pins its definition to the front. The penalty must
    /// not undo that for a definition that lives in a test file; only an
    /// explicit hard filter may remove it.
    #[test]
    fn an_anchored_definition_survives_the_penalty_but_not_a_hard_filter() {
        let store = Store::open_in_memory(DIM).unwrap();
        let mention = "do_memories_by_symbol do_memories_by_symbol caller";
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
                    "def",
                    "main",
                    "src/test/helpers.rs",
                    (3, 30),
                    "do_memories_by_symbol",
                    "fn do_memories_by_symbol() { lookup }",
                    [0.0, 0.0, 1.0, 0.0],
                ),
            ])
            .unwrap();
        let _ = store.rebuild_fts().unwrap();
        let q = |opts: &RankOptions| {
            ids(&search_ranked(
                &store,
                "do_memories_by_symbol",
                &SearchFilter::default(),
                5,
                SearchMode::Hybrid,
                Some(&KwEmbedder),
                None,
                opts,
            )
            .unwrap())
        };
        assert_eq!(q(&RankOptions::default())[0], "def");
        let no_tests = RankOptions {
            include_tests: false,
            ..Default::default()
        };
        assert!(!q(&no_tests).contains(&"def".to_string()));
    }

    fn leveled(mut p: VectorPoint, level: &str, symbol_type: &str) -> VectorPoint {
        p.metadata.chunk_level = level.into();
        p.metadata.symbol_type = symbol_type.into();
        p
    }

    /// Every file emits a `file` chunk spanning all its lines (a summary:
    /// imports and a symbol list), and every class a `class` chunk spanning its
    /// methods. Ranked first, they used to swallow every real chunk inside them
    /// as "contained" — the answer became the table of contents.
    #[test]
    fn a_summary_chunk_never_swallows_the_code_inside_it() {
        let hit = |p: VectorPoint| SearchResult {
            score: 1.0,
            point: p,
        };
        let z = [0.0; DIM];
        let file = leveled(
            code(
                "file",
                "main",
                "state.rs",
                (1, 400),
                "state.rs",
                "# File",
                z,
            ),
            "file",
            "file",
        );
        let func = code(
            "fn",
            "main",
            "state.rs",
            (50, 80),
            "plan_status_list",
            "fn",
            z,
        );
        let class = leveled(
            code("cls", "main", "state.rs", (100, 300), "Store", "struct", z),
            "class",
            "struct",
        );
        let method = code(
            "m",
            "main",
            "state.rs",
            (120, 140),
            "Store.get",
            "fn get",
            z,
        );

        let out = ids(&dedup_hits(vec![
            hit(file.clone()),
            hit(class.clone()),
            hit(func.clone()),
            hit(method.clone()),
        ]));
        assert!(out.contains(&"fn".to_string()), "{out:?}");
        assert!(out.contains(&"m".to_string()), "{out:?}");

        // Content contained in content is still collapsed.
        let inner = code(
            "inner",
            "main",
            "state.rs",
            (55, 60),
            "inner",
            "fn inner",
            z,
        );
        let out = ids(&dedup_hits(vec![hit(func), hit(inner)]));
        assert_eq!(out, ["fn"]);
    }

    /// A query that names the file and the function used to pin the file
    /// summary (its symbol is the basename), which then swallowed the very
    /// definition the query asked for.
    #[test]
    fn naming_the_file_and_the_function_keeps_the_definition_first() {
        let store = Store::open_in_memory(DIM).unwrap();
        let d = [0.0, 1.0, 0.0, 0.0];
        store
            .upsert(&[
                leveled(
                    code(
                        "file",
                        "main",
                        "src/state.rs",
                        (1, 400),
                        "state.rs",
                        "# File: state.rs plan_status_list plan_status_list",
                        d,
                    ),
                    "file",
                    "file",
                ),
                code(
                    "def",
                    "main",
                    "src/state.rs",
                    (50, 80),
                    "plan_status_list",
                    "fn plan_status_list() {}",
                    [0.0, 0.0, 1.0, 0.0],
                ),
            ])
            .unwrap();
        let _ = store.rebuild_fts().unwrap();
        let hits = ids(&search(
            &store,
            "state.rs plan_status_list",
            &SearchFilter::default(),
            5,
            SearchMode::Hybrid,
            Some(&KwEmbedder),
            None,
        )
        .unwrap());
        assert_eq!(hits.first().map(String::as_str), Some("def"), "{hits:?}");
    }

    #[test]
    fn file_names_and_versions_are_not_identifiers() {
        assert!(identifier_tokens("state.rs").is_empty());
        assert!(identifier_tokens("see README.md and plan_status.rs").is_empty());
        assert!(identifier_tokens("release v0.8.4").is_empty());
        assert!(identifier_tokens("e.g. this, i.e. that").is_empty());
        assert_eq!(
            identifier_tokens("state.rs plan_status_list"),
            ["plan_status_list"]
        );
        assert_eq!(identifier_tokens("Card.charge"), ["Card.charge"]);
    }

    /// A stack trace names dozens of identifiers; each one costs SQL and
    /// pinned hits must not replace the whole ranking.
    #[test]
    fn anchoring_is_bounded_in_tokens_and_in_share_of_the_answer() {
        let store = Store::open_in_memory(DIM).unwrap();
        let mut pts = Vec::new();
        for i in 0..10 {
            pts.push(code(
                &format!("def{i}"),
                "main",
                &format!("src/f{i}.rs"),
                (1, 9),
                &format!("handler_{i}"),
                "fn body",
                [0.0, 0.0, 1.0, 0.0],
            ));
            pts.push(code(
                &format!("hit{i}"),
                "main",
                &format!("src/h{i}.rs"),
                (1, 9),
                "other",
                "database",
                [0.0, 1.0, 0.0, 0.0],
            ));
        }
        store.upsert(&pts).unwrap();
        let query = (0..10)
            .map(|i| format!("handler_{i}"))
            .collect::<Vec<_>>()
            .join(" ")
            + " database";
        let hits = ids(&search(
            &store,
            &query,
            &SearchFilter::default(),
            6,
            SearchMode::Hybrid,
            Some(&KwEmbedder),
            None,
        )
        .unwrap());
        let pinned = hits.iter().filter(|h| h.starts_with("def")).count();
        assert!(pinned <= 3, "at most limit/2 pinned: {hits:?}");
        assert!(pinned >= 1, "anchoring still happens: {hits:?}");
    }

    /// `_` is a LIKE wildcard: `get_x` must not anchor `Foo.getAx`.
    #[test]
    fn the_suffix_match_is_literal() {
        let store = Store::open_in_memory(DIM).unwrap();
        store
            .upsert(&[code(
                "wrong",
                "main",
                "a.rs",
                (1, 9),
                "Foo.getAx",
                "fn",
                [0.0, 0.0, 1.0, 0.0],
            )])
            .unwrap();
        let m = store
            .symbol_matches(&SearchFilter::default(), "get_x", 3)
            .unwrap();
        assert!(
            m.is_empty(),
            "{:?}",
            m.iter().map(|p| &p.id).collect::<Vec<_>>()
        );
        let m = store
            .symbol_matches(&SearchFilter::default(), "getAx", 3)
            .unwrap();
        assert_eq!(m.len(), 1);
    }

    /// Without a reranker the pool used to be `max(limit, 20)`: whatever the
    /// dedup dropped shortened the answer below `limit`, silently.
    #[test]
    fn dedup_does_not_shorten_the_answer_below_the_limit() {
        let store = Store::open_in_memory(DIM).unwrap();
        let mut pts = Vec::new();
        for i in 0..30 {
            // Each pair shares a vector, so they rank side by side.
            let e = i as f32 * 0.01;
            let v = [0.0, 1.0, e, 0.0];
            for b in ["main", "dev"] {
                pts.push(code(
                    &format!("c{i}{b}"),
                    b,
                    &format!("src/m{i}.rs"),
                    (1, 9),
                    "f",
                    "database",
                    v,
                ));
            }
        }
        store.upsert(&pts).unwrap();
        let hits = search(
            &store,
            "database",
            &SearchFilter::default(),
            20,
            SearchMode::Vector,
            Some(&KwEmbedder),
            None,
        )
        .unwrap();
        assert_eq!(hits.len(), 20, "{:?}", ids(&hits));
    }

    /// Build the BM25 index for a test that is *about* keyword search.
    ///
    /// The FTS extension is expected (bundled DuckDB installs it on first
    /// use, and CI has the network for it), so its absence fails the test
    /// rather than letting it pass with nothing asserted. An environment that
    /// genuinely cannot load it opts out with `DEVCTX_TEST_ALLOW_NO_FTS=1`,
    /// and the skip is then announced instead of silent.
    fn require_fts(store: &Store, test: &str) -> bool {
        if store.rebuild_fts().unwrap() {
            return true;
        }
        assert!(
            std::env::var_os("DEVCTX_TEST_ALLOW_NO_FTS").is_some(),
            "{test}: the DuckDB FTS extension could not be loaded; set \
             DEVCTX_TEST_ALLOW_NO_FTS=1 to skip keyword tests explicitly"
        );
        eprintln!("SKIPPED {test}: FTS unavailable (DEVCTX_TEST_ALLOW_NO_FTS)");
        false
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
