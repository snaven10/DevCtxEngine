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
    /// Scope (`shared`/`local`).
    pub scope: String,
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

    // Find an existing memory: by topic key, else by content-derived id.
    let existing = if !req.topic_key.is_empty() {
        store.find_memory_by_topic(&req.project, &req.topic_key)?
    } else {
        store
            .get_memory(&memory_id(&req.project, &normalized_hash))?
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
            memory_id(&req.project, &normalized_hash),
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
        project: req.project.clone(),
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

/// Recall memories relevant to `query`, blending intro + best-chunk similarity.
pub fn recall(
    store: &Store,
    embedder: &dyn EmbeddingProvider,
    query: &str,
    project: Option<&str>,
    limit: usize,
) -> Result<Vec<RecalledMemory>> {
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

        let hits = recall(&store, &KwEmbedder, "how does auth work", Some("proj"), 5).unwrap();
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
}
