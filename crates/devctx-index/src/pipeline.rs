//! The indexing pipeline: git diff → parse → chunk → embed → store.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{mpsc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use devctx_chunk::{chunk_file, chunk_raw_text, content_hash, Chunk, ChunkConfig};
use devctx_core::types::{VectorMetadata, VectorPoint};
use devctx_embed::EmbeddingProvider;
use devctx_parse::{detect_lang, extract_routes, parse, raw_text_language};
use devctx_store::{FileState, IndexRecord, Store, StoredEdge, StoredRoute, EXTRACTOR_META_KEY};
use ignore::gitignore::{Gitignore, GitignoreBuilder};

use crate::error::{IndexError, Result};
use crate::git::{Change, GitRepo};
use crate::id::chunk_id;

/// Receives progress updates during an indexing run (e.g. a CLI progress bar).
///
/// `Sync` because long phases that cannot report per file (rebuilding the HNSW
/// index, a checkpoint) are heartbeat from a helper thread — see [`heartbeat`].
pub trait ProgressSink: Sync {
    /// Called once with the total number of changes to process.
    fn start(&self, total: usize);
    /// Called before each change is processed, with its file path.
    fn file(&self, path: &str);
    /// The run is alive in a phase that is not "the next file": pruning,
    /// rebuilding a derived index, checkpointing. Called repeatedly while such
    /// a phase lasts, so a watcher that measures "time since the run last
    /// moved" does not mistake a long rebuild for a stuck run.
    fn phase(&self, _name: &str) {}
    /// Whether the run should stop at the next file boundary (the server is
    /// shutting down). Checked between files; a cancelled run commits what it
    /// has, folds the WAL and returns with [`IndexResult::cancelled`] set.
    fn cancelled(&self) -> bool {
        false
    }
}

/// How often a long, un-instrumentable phase reports that it is still alive.
const HEARTBEAT: Duration = Duration::from_secs(5);

/// Run `op` while ticking `progress` every [`HEARTBEAT`].
///
/// For single statements that can run for many minutes on a big repository
/// (building the HNSW graph, the BM25 index, a checkpoint of a large WAL): they
/// are CPU-bound work inside DuckDB, not a wait on the network, so ticking
/// while they run cannot make a wedged process look alive forever — they end.
/// Without it the idle watchdog read a 15-minute HNSW build as a stuck run and
/// killed a legitimate index in its last phase.
fn heartbeat<T>(progress: Option<&dyn ProgressSink>, name: &str, op: impl FnOnce() -> T) -> T {
    let Some(p) = progress else {
        return op();
    };
    p.phase(name);
    let (tx, rx) = mpsc::channel::<()>();
    std::thread::scope(|s| {
        s.spawn(move || {
            while let Err(mpsc::RecvTimeoutError::Timeout) = rx.recv_timeout(HEARTBEAT) {
                p.phase(name);
            }
        });
        let out = op();
        drop(tx);
        out
    })
}

/// Index-meta key (repository- and branch-independent: `("", "")`) recording
/// that the HNSW index was dropped for a run and not yet rebuilt; its value is
/// the metric.
pub const PENDING_HNSW_META_KEY: &str = "pending_hnsw";
/// Same, for the BM25 index.
pub const PENDING_FTS_META_KEY: &str = "pending_fts";

/// Runs of [`run`] in flight, per database ([`Store::instance_id`]).
///
/// The derived indexes are database-wide while runs are per branch: a short
/// run (a watcher's one-file save) finishing in the middle of a full one used
/// to be harmless only because it never saw the indexes it would rebuild. Now
/// that a dropped index is remembered ([`PENDING_HNSW_META_KEY`]) the last run
/// to finish rebuilds it, so the long one is not left inserting into an HNSW
/// graph.
static RUNS_IN_FLIGHT: Mutex<Option<HashMap<usize, usize>>> = Mutex::new(None);

struct InFlight(usize);

fn in_flight_map() -> std::sync::MutexGuard<'static, Option<HashMap<usize, usize>>> {
    RUNS_IN_FLIGHT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl InFlight {
    fn enter(store: &Store) -> Self {
        let id = store.instance_id();
        *in_flight_map()
            .get_or_insert_with(HashMap::new)
            .entry(id)
            .or_insert(0) += 1;
        InFlight(id)
    }
    /// Whether this is the only run left on its database.
    fn alone(&self) -> bool {
        in_flight_map()
            .as_ref()
            .and_then(|m| m.get(&self.0))
            .is_none_or(|n| *n <= 1)
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        let mut guard = in_flight_map();
        if let Some(m) = guard.as_mut() {
            if let Some(n) = m.get_mut(&self.0) {
                *n -= 1;
                if *n == 0 {
                    m.remove(&self.0);
                }
            }
        }
    }
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
    /// Files whose chunks were copied from another branch instead of embedded,
    /// because that branch already had the identical content.
    ///
    /// Reported because the saving is the entire argument for keeping several
    /// branches indexed, and an argument nobody can see the effect of is one
    /// nobody can tell has stopped working.
    pub files_copied: usize,
    /// Total symbols across indexed files.
    pub symbols: usize,
    /// Total chunks stored.
    pub chunks: usize,
    /// An incremental run left this branch's index on an older (or unknown)
    /// extractor: only the changed files were re-parsed, so the rest still
    /// carries what the old extractor produced. `--full` clears it.
    pub extractor_stale: bool,
    /// The run stopped early because [`ProgressSink::cancelled`] said so (the
    /// server is shutting down). What it wrote is committed and checkpointed;
    /// the index record was **not** advanced, so the next incremental run
    /// starts from the same commit and the content-hash check skips the files
    /// this one already did.
    pub cancelled: bool,
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
    let cancelled = || req.progress.is_some_and(|p| p.cancelled());
    // A run that starts after the stop request has nothing to do but must not
    // take the derived indexes down on its way: dropping them is a write, and
    // the rebuild it would owe is one nobody is going to run in this process.
    if cancelled() {
        return Ok(IndexResult {
            cancelled: true,
            ..Default::default()
        });
    }
    let in_flight = InFlight::enter(req.store);
    // A run that was cut short (cancelled, killed) after dropping the derived
    // indexes left a note saying so; honour it as if they were still there.
    let pending_hnsw = req
        .store
        .get_index_meta("", "", PENDING_HNSW_META_KEY)
        .ok()
        .flatten();
    let pending_fts = req
        .store
        .get_index_meta("", "", PENDING_FTS_META_KEY)
        .ok()
        .flatten()
        .is_some();
    // The BM25 index cannot survive the row deletions this run will make, so it
    // comes down first and goes back up at the end if it was there.
    let had_fts = req.store.has_fts() || pending_fts;
    if had_fts {
        // Noted *before* the drop: a process that dies anywhere between here
        // and the rebuild must not lose the index for good.
        req.store
            .set_index_meta("", "", PENDING_FTS_META_KEY, "1")?;
        req.store.drop_fts()?;
    }
    // The HNSW index has to come down for the same reason, and for a larger
    // one: unlike FTS it survives the writes, so nothing forces the issue —
    // it just makes every single insert pay to maintain the graph. Measured on
    // a 1368-file branch of a real repository: 7 files a minute, an ETA of 108
    // minutes, against minutes for the same work with the index absent and
    // rebuilt once at the end.
    //
    // This is not specific to indexing a second branch. Every incremental run
    // after the first has been paying it; a three-file commit hides the cost,
    // and a large one does not.
    //
    // Searches fall back to brute force while it is down, which is what FTS
    // already does and is the right trade: a slower search during an index
    // beats an index that does not finish.
    let had_hnsw = req.store.hnsw_metric().or(pending_hnsw);
    if let Some(metric) = &had_hnsw {
        req.store
            .set_index_meta("", "", PENDING_HNSW_META_KEY, metric)?;
        req.store.drop_hnsw()?;
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
    let prev_extractor_stale =
        req.store
            .extractor_stale(&repo_path, &branch, &devctx_parse::extractor_fingerprint())?;
    // The exclude set decides what belongs in the index, and an incremental run
    // only looks at what changed since the last commit, so a rule added since
    // would leave what it now covers behind forever. A different fingerprint
    // (or none recorded: an index from before defaults existed) makes this a
    // full run, which is the one that prunes.
    let exclude_fp = exclude_fingerprint(req.exclude);
    let exclude_stale = req
        .store
        .get_index_meta(&repo_path, &branch, EXCLUDE_META_KEY)?
        .as_deref()
        != Some(exclude_fp.as_str());
    let last_commit = prev.as_ref().map(|p| p.last_commit.clone());
    let can_incremental = req.incremental
        && !model_changed
        && !exclude_stale
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
        written: 0,
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
        // Between files, never inside one: every write so far is committed, so
        // stopping here leaves nothing half-done for the checkpoint to fold.
        if cancelled() {
            result.cancelled = true;
            break;
        }
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
    // Checked for a stop between files like the loop above: a cancelled prune
    // leaves the rest of the stale files for the next full run, which is what
    // a cancelled loop does with the files it did not reach.
    if !result.cancelled && !prev_files.is_empty() {
        if let Some(p) = req.progress {
            p.phase("prune");
        }
        for file in &prev_files {
            if ctx.indexed.contains(file) {
                continue;
            }
            if cancelled() {
                result.cancelled = true;
                break;
            }
            if let Some(p) = req.progress {
                p.phase("prune");
            }
            ctx.delete_file(file)?;
            result.files_pruned += 1;
        }
    }

    if result.cancelled {
        // Nothing past this point is safe or useful for a partial run: pruning
        // would delete every file the run did not reach, the index record would
        // claim a commit it did not cover, and rebuilding HNSW is the slowest
        // step of all on the one path that must be quick. The pending notes keep
        // the derived indexes owed to the next run; the checkpoint is the point.
        eprintln!(
            "· indexing cancelled after {} file(s) (the server is stopping); the next run \
             resumes from the same commit",
            ctx.indexed.len()
        );
        heartbeat(req.progress, "checkpoint", || req.store.checkpoint());
        return Ok(result);
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
        repo_path: repo_path.clone(),
        branch: branch.clone(),
        last_commit,
        model_name: req.model_name.to_string(),
        model_dimension: req.embedder.dimension() as i64,
        file_count: counts.0,
        symbol_count: counts.1,
        chunk_count: counts.2,
        indexed_at: now_stamp(),
    })?;

    // Stamp the extractor only when the whole branch was produced by it: a full
    // run, or an incremental one over an index already stamped with it. An
    // incremental run over an older index re-parses just the changed files, so
    // stamping it would hide the rest — the very thing this record is for.
    let current_extractor = devctx_parse::extractor_fingerprint();
    // Same rule for the exclude set: stamped only when a full run applied it (or
    // nothing changed), never by a path-list run that never pruned.
    if full_reindex || !exclude_stale {
        req.store
            .set_index_meta(&repo_path, &branch, EXCLUDE_META_KEY, &exclude_fp)?;
    }
    if full_reindex || !prev_extractor_stale {
        req.store
            .set_index_meta(&repo_path, &branch, EXTRACTOR_META_KEY, &current_extractor)?;
    } else {
        result.extractor_stale = true;
        eprintln!(
            "· this index was built by an older extractor; an incremental run keeps its old \
             symbols and edges. Run `devctx index --full` to rebuild it"
        );
    }

    // Rebuild what this run (or an earlier, interrupted one) took down — but
    // only as the last run in flight: rebuilding under a concurrent run makes
    // every one of its remaining inserts maintain the graph. The note is read
    // again here because a concurrent run may have dropped an index this one
    // never saw.
    //
    // So the run that settles a note is *whichever run finishes last* — a
    // full one, an incremental one, or a watcher's one-file save alike — and
    // it settles it by doing the rebuild, over the whole database. The note is
    // cleared only once the rebuild really happened: an extension that is not
    // available reports `false`, and the index stays owed rather than being
    // declared rebuilt.
    //
    // A stop request skips both rebuilds — each can take minutes on a large
    // repository, which is exactly the time a shutdown does not have — and
    // the notes keep them owed to the next run. Checked before each, since
    // the first can be long enough for the request to arrive during it.
    if in_flight.alone() {
        let hnsw = had_hnsw.or_else(|| {
            req.store
                .get_index_meta("", "", PENDING_HNSW_META_KEY)
                .ok()
                .flatten()
        });
        if let Some(metric) = &hnsw {
            if cancelled() {
                eprintln!("· the server is stopping; the HNSW index is left for the next run");
            } else if heartbeat(req.progress, "hnsw", || req.store.enable_hnsw(metric))? {
                req.store.delete_index_meta("", "", PENDING_HNSW_META_KEY)?;
            }
        }
        let fts = had_fts
            || req
                .store
                .get_index_meta("", "", PENDING_FTS_META_KEY)
                .ok()
                .flatten()
                .is_some();
        if fts {
            if cancelled() {
                eprintln!("· the server is stopping; the BM25 index is left for the next run");
            } else if heartbeat(req.progress, "fts", || req.store.rebuild_fts())? {
                req.store.delete_index_meta("", "", PENDING_FTS_META_KEY)?;
            }
        }
    }
    // An indexing run is where the write-ahead log comes from, and a WAL that
    // outlives its process leaves the ART indexes behind every PRIMARY KEY and
    // UNIQUE missing entries — see `Store::checkpoint`. Fold it in now, while a
    // connection is still open to do it.
    heartbeat(req.progress, "checkpoint", || req.store.checkpoint());

    // A full run deletes every row and writes them again, and DuckDB does not
    // hand the space of the deleted ones back to the file — a checkpoint folds
    // the log in, it does not compact. Measured on a 1,500-file repository: 90 MB
    // before, 1,175 MB after, holding FEWER chunks than it started with, and
    // three checkpoints moved it by nothing. Rebuilding the tables took it to
    // 166 MB.
    //
    // Rebuilding here is not an option — it needs exclusive access to a database
    // this process is serving — so say it instead. Silence is what let a
    // thirteen-fold file go unremarked.
    if !req.incremental {
        if let Some(ratio) = req.store.bloat_ratio() {
            if ratio >= BLOAT_WARN_RATIO {
                eprintln!(
                    "· the index file is about {ratio:.0}× the size of what it holds; \
                     `devctx repair` rebuilds it and gives the space back"
                );
            }
        }
    }

    Ok(result)
}

/// How much bigger than its contents the file has to be before saying so.
///
/// Some overhead is normal — indexes, page alignment, a little slack. Three is
/// past all of that and into "a full run left its old rows behind".
const BLOAT_WARN_RATIO: f64 = 3.0;

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

/// `index_meta` key holding the fingerprint of the exclude set the index was
/// last fully built with.
pub const EXCLUDE_META_KEY: &str = "excludes";

/// A stable fingerprint of an exclude list (FNV-1a over the patterns in order;
/// order matters because a later `!rule` overrides an earlier one).
pub fn exclude_fingerprint(patterns: &[String]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for p in patterns {
        for b in p.bytes().chain(std::iter::once(0u8)) {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    format!("{h:016x}")
}

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
    // Vendored directories (`node_modules/`, `vendor/`, `dist/`...) are not
    // decided here any more: they are `devctx_core::config::DEFAULT_EXCLUDES`,
    // applied through `indexing.exclude`, so that `default_excludes: false`
    // really does bring them back.
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
    /// Files that reached the write step this run (for [`stall_inside_write`]).
    written: usize,
}

/// Test seam: `DEVCTX_TEST_STALL_IN_WRITE=<n>` stalls the run for good inside
/// the writes of the n-th file it writes, after the first of them.
///
/// The serve-lifecycle tests need an index stuck *mid-write* — not between
/// files, where every write is already committed and any exit is clean — to
/// prove that a forced exit leaves no half-written file behind. Nothing else
/// can hold a run there deterministically. Read once; unset (always, outside
/// those tests) it costs one branch per file.
fn stall_inside_write(nth: usize) {
    static AT: std::sync::OnceLock<Option<usize>> = std::sync::OnceLock::new();
    let at = AT.get_or_init(|| {
        std::env::var("DEVCTX_TEST_STALL_IN_WRITE")
            .ok()
            .and_then(|v| v.parse().ok())
    });
    if *at == Some(nth) {
        eprintln!("· test seam: stalled inside the writes of file #{nth}");
        loop {
            std::thread::sleep(Duration::from_secs(3600));
        }
    }
}

impl Ctx<'_> {
    /// Forget a file: its vectors, edges, routes and state, as one
    /// transaction (see `index_file` for why one).
    fn delete_file(&self, file: &str) -> Result<()> {
        self.store.in_transaction(|| {
            self.store
                .delete_by_file(self.repo_short, self.branch, file)?;
            self.store
                .delete_file_edges(self.repo_short, self.branch, file)?;
            self.store
                .delete_file_routes(self.repo_short, self.branch, file)?;
            self.store
                .delete_file_state(self.repo_path, self.branch, file)
        })?;
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
        if let Some(src) = self.store.branch_with_same_content(
            self.repo_path,
            file,
            &hash,
            self.branch,
            &devctx_parse::extractor_fingerprint(),
        )? {
            // One transaction, like the embedding path below; rolled back
            // (the old rows kept) when the source turns out to hold nothing.
            let copied = self.store.in_transaction_opt(|| {
                self.store
                    .delete_by_file(self.repo_short, self.branch, file)?;
                let (language, symbols, chunks) =
                    self.store
                        .copy_file_rows(self.repo_short, &src, self.branch, file)?;
                if chunks == 0 {
                    return Ok(None);
                }
                self.store.save_file_state(&devctx_store::FileState {
                    repo_path: self.repo_path.to_string(),
                    branch: self.branch.to_string(),
                    file_path: file.to_string(),
                    content_hash: hash.clone(),
                    language,
                    symbol_count: symbols as i64,
                    chunk_count: chunks as i64,
                })?;
                Ok(Some((symbols, chunks)))
            })?;
            if let Some((symbols, chunks)) = copied {
                self.indexed.insert(file.to_string());
                result.files_indexed += 1;
                result.files_copied += 1;
                result.symbols += symbols;
                result.chunks += chunks;
                return Ok(());
            }
        }

        // Everything slow or fallible that writes nothing — parsing, chunking,
        // the embedding round trip — happens first. Only then the writes, as
        // one transaction: the old vectors out, the new ones in, edges, routes
        // and the content hash commit together or not at all. Five separate
        // autocommits let a process that ended between the first and the last
        // leave a file whose recorded hash said "indexed, unchanged" over no
        // vectors — a hole no incremental run would ever revisit.
        let (language, parsed, chunks) = match lang {
            // Parseable code: chunk + embed, plus call-graph edges and routes.
            Some(lang) => {
                let parsed = parse(lang, &content)?;
                let chunks = chunk_file(file, &content, &parsed, &self.cfg);
                (parsed.language.clone(), Some(parsed), chunks)
            }
            // Raw text (markdown/json/yaml/kotlin/…): one file-spanning chunk (or
            // blocks). Route extraction still runs (e.g. Kotlin Spring), returning
            // nothing for non-route file types.
            None => {
                let rl = raw_lang.expect("raw language checked above");
                (
                    rl.to_string(),
                    None,
                    chunk_raw_text(file, &content, &self.cfg),
                )
            }
        };
        let points = self.embed(file, &language, &chunks)?;
        let symbol_count = parsed.as_ref().map_or(0, |p| p.symbols.len());
        let chunk_count = chunks.len();

        self.written += 1;
        let nth = self.written;
        self.store.in_transaction(|| {
            self.store
                .delete_by_file(self.repo_short, self.branch, file)?;
            if !points.is_empty() {
                self.store.upsert(&points)?;
            }
            stall_inside_write(nth);
            if let Some(parsed) = &parsed {
                self.store_edges(file, parsed)?;
            }
            self.store_routes(file, &content)?;
            self.store.save_file_state(&FileState {
                repo_path: self.repo_path.to_string(),
                branch: self.branch.to_string(),
                file_path: file.to_string(),
                content_hash: hash.clone(),
                language: language.clone(),
                symbol_count: symbol_count as i64,
                chunk_count: chunk_count as i64,
            })?;
            Ok(())
        })?;
        self.indexed.insert(file.to_string());

        result.files_indexed += 1;
        result.symbols += symbol_count;
        result.chunks += chunk_count;
        Ok(())
    }

    /// Embed `chunks` into the points to store for `file` under `language`.
    /// Writes nothing: the caller stores them inside the file's transaction.
    fn embed(&self, file: &str, language: &str, chunks: &[Chunk]) -> Result<Vec<VectorPoint>> {
        if chunks.is_empty() {
            return Ok(Vec::new());
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
        Ok(points)
    }

    fn store_edges(
        &self,
        file: &str,
        parsed: &devctx_parse::ParsedFile,
    ) -> devctx_store::Result<()> {
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
    }

    fn store_routes(&self, file: &str, content: &str) -> devctx_store::Result<()> {
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
