//! `devctx-memory` — the memory engine: remember (dedup/revise) + recall (blend).
//!
//! Memories are stored as rows in the `memories` table and embedded into the
//! shared `vectors` table as an intro vector (`memory`) plus body-window vectors
//! (`memory_chunk`). Recall collapses per-memory with
//! `score = alpha*intro_sim + (1-alpha)*max_chunk_sim` (alpha = 0.5). See rewrite
//! plan §3/§5. Symbol references are a follow-up.

pub mod error;

use std::collections::HashMap;

use devctx_chunk::{content_hash, memory_chunks, MemoryChunkConfig};
use devctx_core::types::{SearchFilter, VectorMetadata, VectorPoint};
use devctx_embed::EmbeddingProvider;
use devctx_store::{Memory, MemoryStats, Store};
use sha2::{Digest, Sha256};

pub use error::{MemoryError, Result};

/// Blend weight for the intro vector vs the best body chunk.
const BLEND_ALPHA: f32 = 0.5;

/// Scope value for a memory that belongs to one repository only.
pub const SCOPE_LOCAL: &str = "local";

/// Scope value for a memory worth carrying between repositories.
pub const SCOPE_GLOBAL: &str = "global";

/// Reserved `project` value for globally-scoped memories.
///
/// Memory identity is derived from `project` + content hash, so if global rows
/// kept their contributing project the *same* lesson learned in two repositories
/// would land as two rows in the shared store — deduplication failing exactly
/// where it matters most. Global rows therefore all carry this key, and the
/// repository that contributed one stays in `repo` as provenance.
pub const GLOBAL_PROJECT: &str = "@global";

/// Scope value for a memory shared by the repositories of one product.
pub const SCOPE_GROUP: &str = "group";

/// Prefix of the reserved `project` value for group-scoped memories.
///
/// Group rows live in the central store beside the global ones, keyed
/// `@group:<name>` so each product's shared knowledge stays its own space:
/// deduplication still collapses the same lesson learned in two sibling
/// repositories, without leaking it to unrelated projects the way `@global`
/// does.
pub const GROUP_PREFIX: &str = "@group:";

/// Whether a scope string means "global" (`shared` is the legacy spelling).
pub fn is_global(scope: &str) -> bool {
    scope == SCOPE_GLOBAL || scope == "shared"
}

/// Whether a scope string means "shared within a group".
pub fn is_group(scope: &str) -> bool {
    scope == SCOPE_GROUP
}

/// The reserved `project` key holding one group's memories.
pub fn group_project(group: &str) -> String {
    format!("{GROUP_PREFIX}{group}")
}

/// The `project` a memory is stored under, given its requested scope.
fn identity_project(req: &RememberRequest) -> String {
    if is_global(&req.scope) {
        GLOBAL_PROJECT.to_string()
    } else if is_group(&req.scope) && !req.group.is_empty() {
        group_project(&req.group)
    } else {
        req.project.clone()
    }
}

/// What `remember` did with an incoming memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RememberStatus {
    /// A new memory was created.
    Created,
    /// An existing memory (by topic or content id) was revised.
    Revised,
    /// Identical content already existed; only counters were bumped.
    Duplicate,
}

/// Result of a `remember` call.
#[derive(Debug, Clone)]
pub struct RememberResult {
    /// The stored memory.
    pub memory: Memory,
    /// The outcome.
    pub status: RememberStatus,
}

/// A recalled memory with its blended relevance score.
#[derive(Debug, Clone)]
pub struct RecalledMemory {
    /// The memory.
    pub memory: Memory,
    /// Blended relevance score.
    pub score: f32,
}

/// Fields for creating/updating a memory.
#[derive(Debug, Clone, Default)]
pub struct RememberRequest {
    /// Short title.
    pub title: String,
    /// Full content.
    pub content: String,
    /// Type (insight/decision/note/bug/architecture/pattern/discovery).
    pub memory_type: String,
    /// Project.
    pub project: String,
    /// Topic key for upsert-by-topic (empty = none).
    pub topic_key: String,
    /// Comma-separated tags.
    pub tags: String,
    /// Scope (`local`/`group`/`global`; `shared` is the legacy spelling of global).
    pub scope: String,
    /// Group name, used when `scope` is `group`. Empty otherwise.
    pub group: String,
    /// Author.
    pub author: String,
    /// Repo.
    pub repo: String,
    /// Branch.
    pub branch: String,
    /// Comma-separated related files.
    pub files: String,
    /// Session id.
    pub session_id: String,
    /// Caller-provided timestamp (epoch/ISO string).
    pub now: String,
}

/// Store (or update) a memory, deduplicating by topic key or content.
pub fn remember(
    store: &Store,
    embedder: &dyn EmbeddingProvider,
    req: &RememberRequest,
) -> Result<RememberResult> {
    check_dim(store, embedder)?;

    let normalized_hash = sha_hex(&normalize(&req.content));
    // Global memories are keyed by a reserved project so the same lesson from
    // two repositories converges on one row (see [`GLOBAL_PROJECT`]).
    let project = identity_project(req);

    // Find an existing memory: by topic key, else by content-derived id.
    let existing = if !req.topic_key.is_empty() {
        store.find_memory_by_topic(&project, &req.topic_key)?
    } else {
        store
            .get_memory(&memory_id(&project, &normalized_hash))?
            .filter(|m| m.deleted_at.is_none())
    };

    // Identical content already present => duplicate (bump counter, no re-embed).
    if let Some(ex) = &existing {
        if ex.normalized_hash == normalized_hash {
            let mut m = ex.clone();
            m.duplicate_count += 1;
            m.updated_at = req.now.clone();
            store.upsert_memory(&m)?;
            return Ok(RememberResult {
                memory: m,
                status: RememberStatus::Duplicate,
            });
        }
    }

    let (id, created_at, revision_count, status) = match &existing {
        Some(ex) => (
            ex.id.clone(),
            ex.created_at.clone(),
            ex.revision_count + 1,
            RememberStatus::Revised,
        ),
        None => (
            memory_id(&project, &normalized_hash),
            req.now.clone(),
            0,
            RememberStatus::Created,
        ),
    };

    let memory = Memory {
        id: id.clone(),
        title: req.title.clone(),
        content: req.content.clone(),
        memory_type: req.memory_type.clone(),
        scope: req.scope.clone(),
        project,
        topic_key: req.topic_key.clone(),
        tags: req.tags.clone(),
        author: req.author.clone(),
        repo: req.repo.clone(),
        branch: req.branch.clone(),
        files: req.files.clone(),
        revision_count,
        duplicate_count: existing.as_ref().map(|e| e.duplicate_count).unwrap_or(0),
        normalized_hash,
        vector_id: id,
        session_id: req.session_id.clone(),
        created_at,
        updated_at: req.now.clone(),
        deleted_at: None,
    };

    store.upsert_memory(&memory)?;
    index_memory_vectors(store, embedder, &memory)?;

    Ok(RememberResult { memory, status })
}

/// Embed a memory into the vectors table (intro + body-window chunks).
fn index_memory_vectors(store: &Store, embedder: &dyn EmbeddingProvider, m: &Memory) -> Result<()> {
    store.delete_memory_vectors(&m.id)?;

    let chunks = memory_chunks(&m.title, &m.content, &MemoryChunkConfig::default());
    let texts: Vec<String> = chunks.iter().map(|c| c.text.clone()).collect();
    let vectors = embedder.embed(&texts)?;

    let mut points = Vec::with_capacity(chunks.len());
    for (idx, (chunk, vector)) in chunks.iter().zip(vectors).enumerate() {
        let id = if idx == 0 {
            m.id.clone()
        } else {
            format!("{}_c{}", m.id, idx)
        };
        points.push(VectorPoint {
            id,
            vector,
            text: chunk.text.clone(),
            metadata: VectorMetadata {
                repo: m.repo.clone(),
                branch: m.branch.clone(),
                commit: String::new(),
                file: String::new(),
                symbol: m.title.clone(),
                symbol_type: m.memory_type.clone(),
                language: "memory".to_string(),
                start_line: 0,
                end_line: 0,
                chunk_level: chunk.level.clone(),
                content_hash: content_hash(&chunk.text),
                is_deletion: false,
                memory_type: m.memory_type.clone(),
                memory_scope: m.scope.clone(),
                memory_tags: m.tags.clone(),
                indexed_at: m.updated_at.clone(),
            },
        });
    }
    store.upsert(&points)?;
    Ok(())
}

/// What to recall, and how to narrow it.
#[derive(Debug, Clone, Default)]
pub struct RecallQuery<'a> {
    /// The natural-language query.
    pub query: &'a str,
    /// Restrict to one `project` (`None` = any). For the central store this is
    /// [`GLOBAL_PROJECT`], since every global row carries it.
    pub project: Option<&'a str>,
    /// Restrict to memories contributed by one repository — the way to ask the
    /// central store for "what did I learn in *that* project".
    pub repo: Option<&'a str>,
    /// Maximum memories to return.
    pub limit: usize,
}

impl<'a> RecallQuery<'a> {
    /// A query for `text`, unfiltered.
    pub fn new(text: &'a str, limit: usize) -> Self {
        Self {
            query: text,
            project: None,
            repo: None,
            limit,
        }
    }
}

/// Recall memories relevant to a query, blending intro + best-chunk similarity.
pub fn recall(
    store: &Store,
    embedder: &dyn EmbeddingProvider,
    q: &RecallQuery<'_>,
) -> Result<Vec<RecalledMemory>> {
    let query = q.query;
    let project = q.project;
    let limit = q.limit;
    check_dim(store, embedder)?;
    let qvec = embedder.embed_query(query)?;
    let filter = SearchFilter {
        chunk_levels: vec!["memory".to_string(), "memory_chunk".to_string()],
        exclude_deletions: true,
        ..Default::default()
    };
    let fetch_k = (limit * 8).max(40);
    let hits = store.search(&qvec, &filter, fetch_k)?;

    // Collapse per memory: track intro similarity and best chunk similarity.
    let mut intro: HashMap<String, f32> = HashMap::new();
    let mut best_chunk: HashMap<String, f32> = HashMap::new();
    for h in &hits {
        let level = h.point.metadata.chunk_level.as_str();
        if level == "memory" {
            let e = intro.entry(h.point.id.clone()).or_insert(f32::MIN);
            *e = e.max(h.score);
        } else if level == "memory_chunk" {
            let base = strip_chunk_suffix(&h.point.id).to_string();
            let e = best_chunk.entry(base).or_insert(f32::MIN);
            *e = e.max(h.score);
        }
    }

    let mut bases: Vec<String> = intro.keys().chain(best_chunk.keys()).cloned().collect();
    bases.sort();
    bases.dedup();

    let mut out = Vec::new();
    for base in bases {
        let score = blend(intro.get(&base).copied(), best_chunk.get(&base).copied());
        let Some(memory) = store.get_memory(&base)? else {
            continue;
        };
        if memory.deleted_at.is_some() {
            continue;
        }
        if let Some(p) = project {
            if memory.project != p {
                continue;
            }
        }
        if let Some(r) = q.repo {
            if memory.repo != r {
                continue;
            }
        }
        out.push(RecalledMemory { memory, score });
    }

    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out.truncate(limit);
    Ok(out)
}

/// Recent memories for a project (no query).
pub fn memory_context(store: &Store, project: &str, limit: usize) -> Result<Vec<Memory>> {
    Ok(store.recent_memories(project, limit)?)
}

/// Aggregate memory counts for a project.
pub fn memory_stats(store: &Store, project: &str) -> Result<MemoryStats> {
    Ok(store.memory_stats(project)?)
}

/// Fuse independently-ranked recall results **by rank**, never by score.
///
/// The lists may come from stores embedded with different models — a project
/// using `ml-granite` and a central store pinned to `minilm-l6`, say — whose
/// cosine similarities live on incomparable scales. Reciprocal rank fusion only
/// looks at position, so it stays correct across that boundary, and a memory
/// surfacing in both lists is rewarded for it.
pub fn fuse(lists: Vec<Vec<RecalledMemory>>, limit: usize) -> Vec<RecalledMemory> {
    // Positions per memory, so the surviving entries can carry a fused score
    // rather than whichever store's incomparable one happened to win.
    let mut positions: HashMap<String, Vec<usize>> = HashMap::new();
    for list in &lists {
        for (rank, hit) in list.iter().enumerate() {
            positions
                .entry(hit.memory.id.clone())
                .or_default()
                .push(rank);
        }
    }
    let mut out = devctx_core::fuse_by_rank(lists, |h| h.memory.id.clone(), limit);
    for hit in &mut out {
        hit.score = positions
            .get(&hit.memory.id)
            .map(|p| devctx_core::rank_score(p))
            .unwrap_or(0.0);
    }
    out
}

fn blend(intro: Option<f32>, chunk: Option<f32>) -> f32 {
    match (intro, chunk) {
        (Some(i), Some(c)) => BLEND_ALPHA * i + (1.0 - BLEND_ALPHA) * c,
        (Some(i), None) => i,
        (None, Some(c)) => c,
        (None, None) => 0.0,
    }
}

fn check_dim(store: &Store, embedder: &dyn EmbeddingProvider) -> Result<()> {
    if embedder.dimension() != store.dimension() {
        return Err(MemoryError::DimensionMismatch {
            embedder: embedder.dimension(),
            store: store.dimension(),
        });
    }
    Ok(())
}

/// Normalize content for dedup: lowercased, whitespace-collapsed.
fn normalize(content: &str) -> String {
    content
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn sha_hex(s: &str) -> String {
    let digest = Sha256::digest(s.as_bytes());
    let mut out = String::with_capacity(64);
    for b in digest.iter() {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Deterministic memory id from project + normalized content hash.
fn memory_id(project: &str, normalized_hash: &str) -> String {
    let h = sha_hex(&format!("{project}:{normalized_hash}"));
    format!("mem_{}", &h[..24])
}

/// Strip a `_c{n}` chunk suffix (only valid for `memory_chunk` ids).
fn strip_chunk_suffix(id: &str) -> &str {
    if let Some(pos) = id.rfind("_c") {
        let suffix = &id[pos + 2..];
        if !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()) {
            return &id[..pos];
        }
    }
    id
}

#[cfg(test)]
mod tests {
    use super::*;
    use devctx_embed::Result as EmbedResult;

    const KEYWORDS: [&str; 4] = ["auth", "database", "cache", "test"];
    const DIM: usize = 4;

    /// Keyword-presence embedder: deterministic and offline.
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
                    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
                    if norm > 0.0 {
                        for x in &mut v {
                            *x /= norm;
                        }
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

    fn req(title: &str, content: &str, ty: &str) -> RememberRequest {
        RememberRequest {
            title: title.into(),
            content: content.into(),
            memory_type: ty.into(),
            project: "proj".into(),
            now: "100".into(),
            ..Default::default()
        }
    }

    #[test]
    fn remember_then_recall_ranks_by_keyword() {
        let store = Store::open_in_memory(DIM).unwrap();
        remember(
            &store,
            &KwEmbedder,
            &req("Auth decision", "we use auth tokens for login", "decision"),
        )
        .unwrap();
        remember(
            &store,
            &KwEmbedder,
            &req("DB choice", "the database is postgres", "decision"),
        )
        .unwrap();

        let hits = recall(
            &store,
            &KwEmbedder,
            &RecallQuery {
                query: "how does auth work",
                project: Some("proj"),
                repo: None,
                limit: 5,
            },
        )
        .unwrap();
        assert!(!hits.is_empty());
        assert_eq!(hits[0].memory.title, "Auth decision");
    }

    #[test]
    fn identical_content_is_deduped() {
        let store = Store::open_in_memory(DIM).unwrap();
        let r1 = remember(&store, &KwEmbedder, &req("T", "same content here", "note")).unwrap();
        assert_eq!(r1.status, RememberStatus::Created);
        let r2 = remember(
            &store,
            &KwEmbedder,
            &req("T", "same   CONTENT here", "note"),
        )
        .unwrap();
        assert_eq!(r2.status, RememberStatus::Duplicate);
        assert_eq!(r2.memory.id, r1.memory.id);
        assert_eq!(r2.memory.duplicate_count, 1);
        assert_eq!(store.memory_stats("proj").unwrap().total, 1);
    }

    #[test]
    fn topic_upsert_revises() {
        let store = Store::open_in_memory(DIM).unwrap();
        let mut a = req("Auth", "auth v1", "decision");
        a.topic_key = "auth-approach".into();
        let r1 = remember(&store, &KwEmbedder, &a).unwrap();
        assert_eq!(r1.status, RememberStatus::Created);

        let mut b = req("Auth", "auth v2 with database sessions", "decision");
        b.topic_key = "auth-approach".into();
        b.now = "200".into();
        let r2 = remember(&store, &KwEmbedder, &b).unwrap();
        assert_eq!(r2.status, RememberStatus::Revised);
        assert_eq!(r2.memory.id, r1.memory.id);
        assert_eq!(r2.memory.revision_count, 1);
        assert_eq!(r2.memory.content, "auth v2 with database sessions");
        assert_eq!(store.memory_stats("proj").unwrap().total, 1);
    }

    #[test]
    fn context_returns_recent() {
        let store = Store::open_in_memory(DIM).unwrap();
        remember(&store, &KwEmbedder, &req("A", "first note", "note")).unwrap();
        let ctx = memory_context(&store, "proj", 10).unwrap();
        assert_eq!(ctx.len(), 1);
        assert_eq!(ctx[0].title, "A");
    }

    #[test]
    fn strip_chunk_suffix_is_safe() {
        assert_eq!(strip_chunk_suffix("mem_abc123_c5"), "mem_abc123");
        // 'c' as a hex digit at the start must not be stripped.
        assert_eq!(strip_chunk_suffix("mem_c3abdef"), "mem_c3abdef");
    }

    /// The whole point of the reserved key: the same lesson contributed by two
    /// repositories must converge on one row, not sit there twice.
    #[test]
    fn the_same_global_lesson_from_two_projects_is_one_memory() {
        let store = Store::open_in_memory(DIM).unwrap();
        let mk = |project: &str, repo: &str| RememberRequest {
            title: "Cache invalidation".into(),
            content: "always bust the cache key on schema change".into(),
            memory_type: "insight".into(),
            project: project.into(),
            repo: repo.into(),
            scope: SCOPE_GLOBAL.into(),
            now: "100".into(),
            ..Default::default()
        };

        let a = remember(&store, &KwEmbedder, &mk("alpha", "alpha")).unwrap();
        assert_eq!(a.status, RememberStatus::Created);
        assert_eq!(
            a.memory.project, GLOBAL_PROJECT,
            "stored under the shared key"
        );
        assert_eq!(a.memory.repo, "alpha", "provenance preserved");

        let b = remember(&store, &KwEmbedder, &mk("beta", "beta")).unwrap();
        assert_eq!(
            b.status,
            RememberStatus::Duplicate,
            "converged, not duplicated"
        );
        assert_eq!(b.memory.id, a.memory.id);
        assert_eq!(store.memory_stats(GLOBAL_PROJECT).unwrap().total, 1);
    }

    /// A group is a tier of its own: two repositories of one product converge
    /// on a single row, and that row stays out of the global space.
    #[test]
    fn group_memories_converge_within_the_group_and_stay_out_of_global() {
        let store = Store::open_in_memory(DIM).unwrap();
        let mk = |project: &str, group: &str| RememberRequest {
            title: "Order ids".into(),
            content: "order ids are minted by the billing service".into(),
            memory_type: "insight".into(),
            project: project.into(),
            repo: project.into(),
            scope: SCOPE_GROUP.into(),
            group: group.into(),
            now: "100".into(),
            ..Default::default()
        };

        let a = remember(&store, &KwEmbedder, &mk("shop-api", "shop")).unwrap();
        assert_eq!(a.status, RememberStatus::Created);
        assert_eq!(a.memory.project, group_project("shop"));
        assert_eq!(a.memory.repo, "shop-api", "provenance preserved");

        // The sibling repository contributes the same lesson: one row, not two.
        let b = remember(&store, &KwEmbedder, &mk("shop-web", "shop")).unwrap();
        assert_eq!(b.status, RememberStatus::Duplicate);
        assert_eq!(b.memory.id, a.memory.id);
        assert_eq!(store.memory_stats(&group_project("shop")).unwrap().total, 1);

        // An unrelated product must not see it, and neither must the global space.
        assert_eq!(store.memory_stats(&group_project("crm")).unwrap().total, 0);
        assert_eq!(store.memory_stats(GLOBAL_PROJECT).unwrap().total, 0);
    }

    /// Local memories keep their per-project identity, so the same note in two
    /// projects stays two notes.
    #[test]
    fn local_memories_stay_per_project() {
        let store = Store::open_in_memory(DIM).unwrap();
        let mk = |project: &str| RememberRequest {
            title: "Cache".into(),
            content: "the cache lives in redis".into(),
            memory_type: "note".into(),
            project: project.into(),
            scope: SCOPE_LOCAL.into(),
            now: "100".into(),
            ..Default::default()
        };
        let a = remember(&store, &KwEmbedder, &mk("alpha")).unwrap();
        let b = remember(&store, &KwEmbedder, &mk("beta")).unwrap();
        assert_ne!(a.memory.id, b.memory.id);
        assert_eq!(store.memory_stats("alpha").unwrap().total, 1);
        assert_eq!(store.memory_stats("beta").unwrap().total, 1);
    }

    /// `shared` is the legacy spelling of `global` and must behave identically.
    #[test]
    fn shared_is_accepted_as_global() {
        assert!(is_global("global"));
        assert!(is_global("shared"));
        assert!(!is_global("local"));
        assert!(!is_global(""));
    }

    /// Global memories can be narrowed to the repository that contributed them.
    #[test]
    fn recall_can_filter_globals_by_contributing_repo() {
        let store = Store::open_in_memory(DIM).unwrap();
        for (repo, content) in [
            ("alpha", "auth tokens expire hourly"),
            ("beta", "the database is sharded"),
        ] {
            remember(
                &store,
                &KwEmbedder,
                &RememberRequest {
                    title: content.into(),
                    content: content.into(),
                    memory_type: "insight".into(),
                    project: "whatever".into(),
                    repo: repo.into(),
                    scope: SCOPE_GLOBAL.into(),
                    now: "100".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        }

        let all = recall(
            &store,
            &KwEmbedder,
            &RecallQuery::new("auth and database", 10),
        )
        .unwrap();
        assert_eq!(all.len(), 2);

        let only_beta = recall(
            &store,
            &KwEmbedder,
            &RecallQuery {
                query: "auth and database",
                project: Some(GLOBAL_PROJECT),
                repo: Some("beta"),
                limit: 10,
            },
        )
        .unwrap();
        assert_eq!(only_beta.len(), 1);
        assert_eq!(only_beta[0].memory.repo, "beta");
    }

    /// Fusion must rank by position, never by score, because the two lists can
    /// come from different embedding models.
    #[test]
    fn fusion_ranks_by_position_not_score() {
        let mk = |id: &str, score: f32| RecalledMemory {
            memory: Memory {
                id: id.into(),
                ..Default::default()
            },
            score,
        };

        // `both` is second in each list but appears in both; `huge` is third in
        // one list with an absurd score that must not buy it the top spot.
        let local = vec![mk("local_top", 0.9), mk("both", 0.8), mk("huge", 99.0)];
        let global = vec![mk("global_top", 0.2), mk("both", 0.1)];

        let fused = fuse(vec![local, global], 10);
        assert_eq!(fused[0].memory.id, "both", "appearing in both lists wins");
        assert!(
            fused.iter().position(|m| m.memory.id == "huge").unwrap() > 0,
            "an incomparable score must not decide the ranking"
        );
        assert_eq!(fused.len(), 4, "deduplicated across lists");

        assert!(fuse(vec![], 5).is_empty());
        assert_eq!(fuse(vec![vec![mk("a", 1.0), mk("b", 0.5)]], 1).len(), 1);
    }
}
