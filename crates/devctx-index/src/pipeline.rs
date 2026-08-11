//! The indexing pipeline: git diff → parse → chunk → embed → store.

use std::collections::HashSet;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use devctx_chunk::{chunk_file, chunk_raw_text, content_hash, Chunk, ChunkConfig};
use devctx_core::types::{VectorMetadata, VectorPoint};
use devctx_embed::EmbeddingProvider;
use devctx_parse::{detect_lang, extract_routes, parse, raw_text_language};
use devctx_store::{FileState, IndexRecord, Store, StoredEdge, StoredRoute};
use ignore::gitignore::{Gitignore, GitignoreBuilder};

use crate::error::{IndexError, Result};
use crate::git::{Change, GitRepo};
use crate::id::chunk_id;

/// Receives progress updates during an indexing run (e.g. a CLI progress bar).
pub trait ProgressSink {
    /// Called once with the total number of changes to process.
    fn start(&self, total: usize);
    /// Called before each change is processed, with its file path.
    fn file(&self, path: &str);
}

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
    /// Index exactly these repo-relative paths instead of asking git what
    /// changed between commits.
    ///
    /// File *selection* is the only commit-bound part of the pipeline —
    /// `read_file` already reads the work tree, and `index_file` already skips
    /// unchanged content by hash. Handing the list in directly is therefore all
    /// it takes to index work that has not been committed yet, which is what a
    /// file watcher or an editor integration needs.
    pub paths: Option<&'a [String]>,
    /// Model name recorded in `index_state` (drives model-change reindex).
    pub model_name: &'a str,
    /// Optional progress reporter.
    pub progress: Option<&'a dyn ProgressSink>,
    /// `.gitignore`-style patterns for paths to keep out of the index
    /// (`indexing.exclude` in the project config).
    pub exclude: &'a [String],
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
    /// Files removed from the index (via git deletions).
    pub files_deleted: usize,
    /// Files pruned as stale during a full reindex (vanished since last index).
    pub files_pruned: usize,
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

    let excluded = build_exclude(req.exclude);
    let git = GitRepo::open(req.repo_root)?;
    let state = git.state();
    let repo_short = git.short_name();
    let repo_path = git.root().to_string_lossy().to_string();
    let branch = state.branch.clone();

    let explicit_paths = req.paths.filter(|p| !p.is_empty());
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
    // An explicit path list is never a full reindex, whatever the commit state:
    // it says exactly which files to look at.
    let full_reindex = from.is_none() && explicit_paths.is_none();

    // Snapshot the previously-indexed files so a full reindex can prune any that
    // vanished (git diff can't detect them when we list all tracked files).
    let prev_files = if full_reindex {
        req.store.list_file_states(&repo_path, &branch)?
    } else {
        Vec::new()
    };

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
        indexed: HashSet::new(),
        excluded,
    };

    let mut result = IndexResult {
        commit: state.commit.clone(),
        branch: branch.clone(),
        full_reindex,
        ..Default::default()
    };

    let changes = match explicit_paths {
        // A path that no longer exists on disk was deleted; everything else is
        // re-read and dropped by the content-hash check if it did not change.
        Some(paths) => paths
            .iter()
            .map(|p| {
                if git.root().join(p).exists() {
                    Change::Modified(p.clone())
                } else {
                    Change::Deleted(p.clone())
                }
            })
            .collect(),
        None => git.changes(from)?,
    };
    if let Some(p) = req.progress {
        p.start(changes.len());
    }
    for change in changes {
        if let Some(p) = req.progress {
            p.file(change_path(&change));
        }
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

    // Prune stale files: previously indexed but not re-indexed this full run.
    for file in &prev_files {
        if !ctx.indexed.contains(file) {
            ctx.delete_file(file)?;
            result.files_pruned += 1;
        }
    }

    // A path-list run indexes uncommitted work, so HEAD is not what it covered:
    // advancing `last_commit` would make the next incremental diff start after
    // commits whose other files were never looked at. Its counts are equally
    // meaningless as totals, so the previous record's are carried forward.
    let (last_commit, counts) = match (&explicit_paths, &prev) {
        (Some(_), Some(p)) => (
            p.last_commit.clone(),
            (p.file_count, p.symbol_count, p.chunk_count),
        ),
        _ => (
            state.commit.clone(),
            (
                result.files_indexed as i64,
                result.symbols as i64,
                result.chunks as i64,
            ),
        ),
    };
    req.store.save_index_record(&IndexRecord {
        repo_path,
        branch,
        last_commit,
        model_name: req.model_name.to_string(),
        model_dimension: req.embedder.dimension() as i64,
        file_count: counts.0,
        symbol_count: counts.1,
        chunk_count: counts.2,
        indexed_at: now_stamp(),
    })?;

    Ok(result)
}

/// DevCtxEngine's own working directories, which must never be indexed.
///
/// This is not a convenience filter — it is load-bearing. The work tree now
/// includes files git is not tracking, and our state directory and downloaded
/// model cache both live there: without this the index would swallow its own
/// database and a few hundred megabytes of tokenizer JSON, and then answer
/// questions with it.
/// `.fastembed_cache` is legacy: models now live under
/// [`devctx_core::dirs::model_cache_dir`], but older checkouts still carry one.
const OWN_ARTIFACTS: &[&str] = &[".devctx", ".fastembed_cache", ".git"];

/// Compile `indexing.exclude` into a matcher.
///
/// Reusing the gitignore engine rather than a plain glob is deliberate: a rule
/// like `target/` then covers everything beneath it, and `*.log` matches at any
/// depth — which is what anyone writing these patterns expects. An unparseable
/// pattern is dropped rather than failing the run; refusing to index because one
/// line of config is malformed helps nobody.
fn build_exclude(patterns: &[String]) -> Gitignore {
    if patterns.is_empty() {
        return Gitignore::empty();
    }
    let mut b = GitignoreBuilder::new("");
    for p in patterns {
        let _ = b.add_line(None, p);
    }
    b.build().unwrap_or_else(|_| Gitignore::empty())
}

fn is_own_artifact(path: &Path) -> bool {
    path.components()
        .next()
        .and_then(|c| c.as_os_str().to_str())
        .is_some_and(|first| OWN_ARTIFACTS.contains(&first))
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
    /// Files that were (re)indexed this run — used to prune stale files.
    indexed: HashSet<String>,
    /// Compiled `indexing.exclude` patterns.
    excluded: Gitignore,
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
        let path = Path::new(file);
        if is_own_artifact(path)
            || self
                .excluded
                .matched_path_or_any_parents(path, false)
                .is_ignore()
        {
            result.files_skipped += 1;
            return Ok(());
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default();
        let lang = detect_lang(path);
        let raw_lang = if lang.is_none() {
            raw_text_language(&ext)
        } else {
            None
        };
        if lang.is_none() && raw_lang.is_none() {
            result.files_skipped += 1;
            return Ok(());
        }

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

        let (language, symbol_count, chunk_count) = match lang {
            // Parseable code: chunk + embed, plus call-graph edges and routes.
            Some(lang) => {
                let parsed = parse(lang, &content)?;
                let chunks = chunk_file(file, &content, &parsed, &self.cfg);
                self.embed_and_store(file, &parsed.language, &chunks)?;
                self.store_edges(file, &parsed)?;
                self.store_routes(file, &content)?;
                result.symbols += parsed.symbols.len();
                (parsed.language.clone(), parsed.symbols.len(), chunks.len())
            }
            // Raw text (markdown/json/yaml/kotlin/…): one file-spanning chunk (or
            // blocks). Route extraction still runs (e.g. Kotlin Spring), returning
            // nothing for non-route file types.
            None => {
                let rl = raw_lang.expect("raw language checked above");
                let chunks = chunk_raw_text(file, &content, &self.cfg);
                self.embed_and_store(file, rl, &chunks)?;
                self.store_routes(file, &content)?;
                (rl.to_string(), 0, chunks.len())
            }
        };

        self.store.save_file_state(&FileState {
            repo_path: self.repo_path.to_string(),
            branch: self.branch.to_string(),
            file_path: file.to_string(),
            content_hash: hash,
            language,
            symbol_count: symbol_count as i64,
            chunk_count: chunk_count as i64,
        })?;
        self.indexed.insert(file.to_string());

        result.files_indexed += 1;
        result.chunks += chunk_count;
        Ok(())
    }

    /// Embed `chunks` and upsert them as vectors for `file` under `language`.
    fn embed_and_store(&self, file: &str, language: &str, chunks: &[Chunk]) -> Result<()> {
        if chunks.is_empty() {
            return Ok(());
        }
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
                    language: language.to_string(),
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
        Ok(())
    }

    fn store_edges(&self, file: &str, parsed: &devctx_parse::ParsedFile) -> Result<()> {
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
            .replace_file_edges(self.repo_short, self.branch, file, &edges)
            .map_err(Into::into)
    }

    fn store_routes(&self, file: &str, content: &str) -> Result<()> {
        let routes: Vec<StoredRoute> = extract_routes(content, Path::new(file))
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
        self.store
            .replace_file_routes(self.repo_short, self.branch, file, &routes, &now_stamp())
            .map_err(Into::into)
    }
}

/// The display path of a change (the destination for a rename).
fn change_path(c: &Change) -> &str {
    match c {
        Change::Added(f) | Change::Modified(f) | Change::Deleted(f) => f,
        Change::Renamed { to, .. } => to,
    }
}

fn now_stamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_default()
}
