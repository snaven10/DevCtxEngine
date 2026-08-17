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

    /// The branch to index. `None` means the checked-out one.
    ///
    /// Naming another branch reads it out of git rather than off disk, so a
    /// repository can keep several branches indexed from wherever it happens to
    /// be checked out — which is the only way a worktree layout works, since
    /// only one branch is on disk at a time.
    pub branch: Option<&'a str>,
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
    // The BM25 index cannot survive the row deletions this run will make, so it
    // comes down first and goes back up at the end if it was there.
    let had_fts = req.store.has_fts();
    if had_fts {
        req.store.drop_fts()?;
    }
    let git = GitRepo::open(req.repo_root)?;
    let state = git.state();
    let repo_short = git.short_name();
    let repo_path = git.root().to_string_lossy().to_string();
    // The branch this run is *about*, which is not always the one on disk.
    let branch = match req.branch {
        Some(b) if b != state.branch => {
            if !git.has_branch(b) {
                return Err(IndexError::UnknownBranch(b.to_string()));
            }
            b.to_string()
        }
        _ => state.branch.clone(),
    };
    // Reading off disk is right only for the checked-out branch, and it is
    // better than reading git there: the work tree includes files written but
    // not committed, which is exactly the code someone is about to ask about.
    // For any other branch the work tree holds someone else's content, so the
    // objects are the only honest source.
    let read_from = (branch != state.branch).then(|| branch.clone());
    let head_commit = if read_from.is_some() {
        git.commit_of(&branch).unwrap_or_default()
    } else {
        state.commit.clone()
    };

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
        commit: &head_commit,
        read_from: read_from.clone(),
        full_reindex,
        cfg: ChunkConfig::default(),
        indexed: HashSet::new(),
        excluded,
    };

    let mut result = IndexResult {
        commit: head_commit.clone(),
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
        None => match &read_from {
            Some(b) => git.changes_at(b, from)?,
            None => git.changes(from)?,
        },
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
    // The counts recorded are what the store holds, not what this run touched:
    // an incremental pass over three files used to overwrite the summary with
    // `files = 3`, so `status` reported a complete index as nearly empty.
    let totals = req.store.index_totals(&repo_path, &branch)?;
    let last_commit = match (&explicit_paths, &prev) {
        (Some(_), Some(p)) => p.last_commit.clone(),
        _ => head_commit.clone(),
    };
    let counts = totals;
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

    if had_fts {
        req.store.rebuild_fts()?;
    }
    // An indexing run is where the write-ahead log comes from, and a WAL that
    // outlives its process leaves the ART indexes behind every PRIMARY KEY and
    // UNIQUE missing entries — see `Store::checkpoint`. Fold it in now, while a
    // connection is still open to do it.
    req.store.checkpoint();

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

/// Directories whose contents are somebody else's code, checked in for
/// convenience. Nobody asks a question whose answer is in `node_modules`.
const VENDOR_DIRS: &[&str] = &[
    "node_modules",
    "vendor",
    "third_party",
    "dist",
    "bower_components",
];

/// A minified line is longer than any line a person writes. 1,000 characters is
/// far past the widest hand-written line and far below a bundle's single line,
/// so the threshold does not need to be delicate.
const MINIFIED_LINE: usize = 1_000;

/// Is this a file a machine wrote?
///
/// Vendored bundles and generated data are the loudest possible noise in a
/// semantic index: they are enormous, so they produce many chunks, and their
/// content resembles nothing, so it sits at a middling distance from every
/// query and crowds the top of the results for all of them. One 900 KB
/// `cytoscape.min.js` in this repository produced 43 chunks — more than
/// `state.rs` — and surfaced above the file a question was actually about.
///
/// Detection is by shape rather than by name: `*.min.js` is the convention, but
/// a single 200,000-character line is the fact, and it catches generated JSON,
/// bundled CSS and vendored blobs that follow no naming convention at all.
fn is_generated(path: &Path, content: &str) -> bool {
    if path
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .any(|c| VENDOR_DIRS.contains(&c))
    {
        return true;
    }
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    if name.contains(".min.") || name.contains(".bundle.") || name.ends_with("-lock.json") {
        return true;
    }
    content.lines().any(|l| l.len() > MINIFIED_LINE)
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
    /// Read file content from this branch's objects; `None` reads the work tree.
    read_from: Option<String>,
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

        let read = match &self.read_from {
            Some(b) => self.git.read_file_at(b, file),
            None => self.git.read_file(file),
        };
        let Ok(content) = read else {
            // Unreadable / non-UTF-8 (binary): skip.
            result.files_skipped += 1;
            return Ok(());
        };
        if is_generated(path, &content) {
            result.files_skipped += 1;
            return Ok(());
        }
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

        // Branches share commits: a feature branch differs from its base in a
        // handful of files and is byte-identical in the other thousand. When
        // some other branch already holds this exact content, its chunks are
        // the chunks this branch would produce — so copy them and skip the
        // embedding, which is the expensive half by orders of magnitude
        // (minutes against milliseconds).
        //
        // Safe because the key is the content hash: identical bytes, identical
        // chunks. What differs between the two rows is only which branch they
        // are filed under.
        if let Some(src) =
            self.store
                .branch_with_same_content(self.repo_path, file, &hash, self.branch)?
        {
            let (language, symbols, chunks) =
                self.store
                    .copy_file_rows(self.repo_short, &src, self.branch, file)?;
            if chunks > 0 {
                self.store.save_file_state(&devctx_store::FileState {
                    repo_path: self.repo_path.to_string(),
                    branch: self.branch.to_string(),
                    file_path: file.to_string(),
                    content_hash: hash,
                    language,
                    symbol_count: symbols as i64,
                    chunk_count: chunks as i64,
                })?;
                self.indexed.insert(file.to_string());
                result.files_indexed += 1;
                result.symbols += symbols;
                result.chunks += chunks;
                return Ok(());
            }
        }

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

#[cfg(test)]
mod tests {
    use super::*;

    /// A vendored bundle is the loudest noise a semantic index can hold: huge,
    /// so it produces many chunks, and resembling nothing, so it sits at a
    /// middling distance from every query and crowds all of them.
    #[test]
    fn machine_written_files_are_recognised() {
        let long = "a".repeat(MINIFIED_LINE + 1);
        assert!(is_generated(Path::new("assets/cytoscape.min.js"), "x"));
        assert!(is_generated(Path::new("web/app.bundle.js"), "x"));
        assert!(is_generated(
            Path::new("node_modules/left-pad/index.js"),
            "x"
        ));
        assert!(is_generated(Path::new("package-lock.json"), "x"));
        assert!(
            is_generated(Path::new("src/data.json"), &long),
            "one impossible line is enough, whatever the name"
        );
    }

    /// The shape test must not catch code people wrote. A long-ish line, a
    /// minified-sounding word in a path, a file that merely lives near assets —
    /// none of those are machine output.
    #[test]
    fn hand_written_files_are_left_alone() {
        assert!(!is_generated(
            Path::new("crates/devctx-index/src/pipeline.rs"),
            "fn main() {}\n"
        ));
        assert!(!is_generated(
            Path::new("src/minify.js"),
            "export function minify() {}\n"
        ));
        assert!(!is_generated(Path::new("docs/vendors.md"), "# Vendors\n"));
        assert!(
            !is_generated(
                Path::new("src/wide.rs"),
                &format!("// {}\n", "x".repeat(300))
            ),
            "a wide line is still a line someone typed"
        );
    }
}
