//! The indexing pipeline: git diff → parse → chunk → embed → store.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use devai_chunk::{chunk_file, content_hash, ChunkConfig};
use devai_core::types::{VectorMetadata, VectorPoint};
use devai_embed::EmbeddingProvider;
use devai_parse::{detect_lang, extract_routes, parse};
use devai_store::{FileState, IndexRecord, Store, StoredEdge, StoredRoute};

use crate::error::{IndexError, Result};
use crate::git::{Change, GitRepo};
use crate::id::chunk_id;

/// Inputs for one indexing run.
pub struct IndexRequest<'a> {
    /// The DuckDB store.
    pub store: &'a Store,
    /// The embedding provider (dimension must equal the store's).
    pub embedder: &'a dyn EmbeddingProvider,
    /// Any path inside the target repository.
    pub repo_root: &'a Path,
    /// Attempt an incremental (diff-based) index when possible.
    pub incremental: bool,
    /// Model name recorded in `index_state` (drives model-change reindex).
    pub model_name: &'a str,
}

/// Summary of an indexing run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IndexResult {
    /// Indexed commit.
    pub commit: String,
    /// Branch.
    pub branch: String,
    /// Whether a full reindex was performed.
    pub full_reindex: bool,
    /// Files parsed and stored.
    pub files_indexed: usize,
    /// Files skipped (unchanged or unsupported).
    pub files_skipped: usize,
    /// Files removed from the index.
    pub files_deleted: usize,
    /// Files renamed.
    pub files_renamed: usize,
    /// Total symbols across indexed files.
    pub symbols: usize,
    /// Total chunks stored.
    pub chunks: usize,
}

/// Run the indexing pipeline against the repository containing `repo_root`.
pub fn run(req: IndexRequest) -> Result<IndexResult> {
    if req.embedder.dimension() != req.store.dimension() {
        return Err(IndexError::DimensionMismatch {
            embedder: req.embedder.dimension(),
            store: req.store.dimension(),
        });
    }

    let git = GitRepo::open(req.repo_root)?;
    let state = git.state();
    let repo_short = git.short_name();
    let repo_path = git.root().to_string_lossy().to_string();
    let branch = state.branch.clone();

    let prev = req.store.get_index_record(&repo_path, &branch)?;
    let model_changed = prev.as_ref().is_some_and(|p| {
        p.model_name != req.model_name || p.model_dimension as usize != req.embedder.dimension()
    });
    let last_commit = prev.as_ref().map(|p| p.last_commit.clone());
    let can_incremental = req.incremental
        && !model_changed
        && last_commit.as_deref().is_some_and(|c| git.commit_exists(c));
    let from = if can_incremental {
        last_commit.as_deref()
    } else {
        None
    };
    let full_reindex = from.is_none();

    let mut ctx = Ctx {
        store: req.store,
        embedder: req.embedder,
        git: &git,
        repo_short: &repo_short,
        repo_path: &repo_path,
        branch: &branch,
        commit: &state.commit,
        full_reindex,
        cfg: ChunkConfig::default(),
    };

    let mut result = IndexResult {
        commit: state.commit.clone(),
        branch: branch.clone(),
        full_reindex,
        ..Default::default()
    };

    for change in git.changes(from)? {
        match change {
            Change::Deleted(file) => {
                ctx.delete_file(&file)?;
                result.files_deleted += 1;
            }
            Change::Renamed { from, to } => {
                ctx.delete_file(&from)?;
                result.files_renamed += 1;
                ctx.index_file(&to, &mut result)?;
            }
            Change::Added(file) | Change::Modified(file) => {
                ctx.index_file(&file, &mut result)?;
            }
        }
    }

    req.store.save_index_record(&IndexRecord {
        repo_path,
        branch,
        last_commit: state.commit,
        model_name: req.model_name.to_string(),
        model_dimension: req.embedder.dimension() as i64,
        file_count: result.files_indexed as i64,
        symbol_count: result.symbols as i64,
        chunk_count: result.chunks as i64,
        indexed_at: now_stamp(),
    })?;

    Ok(result)
}

struct Ctx<'a> {
    store: &'a Store,
    embedder: &'a dyn EmbeddingProvider,
    git: &'a GitRepo,
    repo_short: &'a str,
    repo_path: &'a str,
    branch: &'a str,
    commit: &'a str,
    full_reindex: bool,
    cfg: ChunkConfig,
}

impl Ctx<'_> {
    fn delete_file(&self, file: &str) -> Result<()> {
        self.store
            .delete_by_file(self.repo_short, self.branch, file)?;
        self.store
            .delete_file_edges(self.repo_short, self.branch, file)?;
        self.store
            .delete_file_routes(self.repo_short, self.branch, file)?;
        self.store
            .delete_file_state(self.repo_path, self.branch, file)?;
        Ok(())
    }

    fn index_file(&mut self, file: &str, result: &mut IndexResult) -> Result<()> {
        let Some(lang) = detect_lang(Path::new(file)) else {
            result.files_skipped += 1;
            return Ok(());
        };
        let Ok(content) = self.git.read_file(file) else {
            // Unreadable / non-UTF-8 (binary): skip.
            result.files_skipped += 1;
            return Ok(());
        };
        let hash = content_hash(&content);

        if !self.full_reindex {
            if let Some(prev) = self
                .store
                .get_file_hash(self.repo_path, self.branch, file)?
            {
                if prev == hash {
                    result.files_skipped += 1;
                    return Ok(());
                }
            }
        }

        // Replace any existing vectors for this file.
        self.store
            .delete_by_file(self.repo_short, self.branch, file)?;

        let parsed = parse(lang, &content)?;
        let chunks = chunk_file(file, &content, &parsed, &self.cfg);

        if !chunks.is_empty() {
            let texts: Vec<String> = chunks.iter().map(|c| c.text.clone()).collect();
            let vectors = self.embedder.embed(&texts)?;
            let mut points = Vec::with_capacity(chunks.len());
            for (ordinal, (chunk, vector)) in chunks.iter().zip(vectors).enumerate() {
                points.push(VectorPoint {
                    id: chunk_id(
                        self.repo_short,
                        self.branch,
                        file,
                        chunk.start_line,
                        ordinal,
                    ),
                    vector,
                    text: chunk.text.clone(),
                    metadata: VectorMetadata {
                        repo: self.repo_short.to_string(),
                        branch: self.branch.to_string(),
                        commit: self.commit.to_string(),
                        file: file.to_string(),
                        symbol: chunk.symbol_name.clone(),
                        symbol_type: chunk.symbol_type.clone(),
                        language: parsed.language.clone(),
                        start_line: chunk.start_line as i32,
                        end_line: chunk.end_line as i32,
                        chunk_level: chunk.level.clone(),
                        content_hash: chunk.content_hash.clone(),
                        is_deletion: false,
                        indexed_at: now_stamp(),
                        ..Default::default()
                    },
                });
            }
            self.store.upsert(&points)?;
        }

        // Store call-graph edges for this file.
        let edges: Vec<StoredEdge> = parsed
            .edges
            .iter()
            .map(|e| StoredEdge {
                source: e.source.clone(),
                target: e.target.clone(),
                kind: e.kind.clone(),
                source_file: file.to_string(),
                line: e.line as i32,
            })
            .collect();
        self.store
            .replace_file_edges(self.repo_short, self.branch, file, &edges)?;

        // Extract and store HTTP routes (framework-detected from content).
        let routes: Vec<StoredRoute> = extract_routes(&content, Path::new(file))
            .into_iter()
            .map(|r| StoredRoute {
                framework: r.framework,
                http_method: r.http_method,
                path: r.path,
                handler_class: r.handler_class,
                handler_method: r.handler_method,
                handler_symbol: r.handler_symbol,
                file: file.to_string(),
                line: r.line as i32,
            })
            .collect();
        self.store.replace_file_routes(
            self.repo_short,
            self.branch,
            file,
            &routes,
            &now_stamp(),
        )?;

        self.store.save_file_state(&FileState {
            repo_path: self.repo_path.to_string(),
            branch: self.branch.to_string(),
            file_path: file.to_string(),
            content_hash: hash,
            language: parsed.language.clone(),
            symbol_count: parsed.symbols.len() as i64,
            chunk_count: chunks.len() as i64,
        })?;

        result.files_indexed += 1;
        result.symbols += parsed.symbols.len();
        result.chunks += chunks.len();
        Ok(())
    }
}

fn now_stamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_default()
}
