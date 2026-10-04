//! Shared server state and the blocking tool implementations.
//!
//! The embedder and reranker are built **lazily** on first use (model load is
//! expensive and would otherwise block server startup — e.g. an MCP client's
//! connect handshake times out while the model downloads). The DuckDB store is
//! opened once and cloned per call (a `Connection` is not `Sync`).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use devctx_core::config::ProjectConfig;
use devctx_core::plans::{self, Analysis, Plan, PlansRoot};
use devctx_core::{SearchFilter, SearchResult};
use devctx_embed::{create_provider, EmbedSettings, EmbeddingProvider};
use devctx_index::{run as index_run, GitRepo, IndexRequest, ProgressSink};
use devctx_memory::{memory_context, memory_stats, recall, remember, RecallQuery, RememberRequest};
use devctx_rerank::{create_reranker, RerankSettings, Reranker};
use devctx_search::SearchMode;
use devctx_store::Store;
use devctx_summarize::{create_summarizer, SummarizeSettings};
use serde_json::{json, Value};

/// How far an indexing run has got, for anyone who asks while it runs.
///
/// The work happens inside the server, so the client that asked for it sees
/// nothing until the whole run answers — minutes, on a large repository, that
/// look exactly like a hang. This is what a caller polls to tell the two apart.
#[derive(Clone, Default)]
pub struct IndexProgress {
    /// Whether a run is in flight right now.
    pub running: bool,
    /// Which run these counts belong to, counting up from one.
    ///
    /// `running` alone cannot tell two runs apart: a watcher save or a commit
    /// hook can start one while another is still going, and a client polling
    /// across the handover would otherwise read the new run's `done` against
    /// the old run's `total` — which is how a bar reaches 788/647.
    pub run: u64,
    /// Changes the run expects to process, known once it has diffed.
    pub total: usize,
    /// Changes it has started on. Started, not finished: the sink is called
    /// before each file, so this counts what is under way.
    pub done: usize,
    /// The file it reached last.
    pub file: String,
    /// When the run last moved (started, reached a file, or reported a phase
    /// that is still working). `running` with an old stamp is a run that is
    /// stuck, not one that is working.
    pub advanced: Option<Instant>,
    /// What the run is doing when it is not on a file: `loading model`,
    /// `files`, `prune`, `hnsw`, `fts`, `checkpoint`.
    pub phase: String,
}

/// Writes an indexing run's progress where a request handler can read it.
///
/// There is one slot for the whole server and any number of runs that may want
/// it: `index --full` from the CLI, a watcher indexing a saved file, a commit
/// hook. The first to arrive owns the slot until it finishes; the others do
/// their work and report nothing, because a full run's counts are what someone
/// is actually watching and a one-file run would otherwise overwrite them.
struct SharedProgress {
    shared: Arc<Mutex<IndexProgress>>,
    /// Whether this run is the one currently filling the slot. Decided at
    /// [`SharedProgress::begin`] or [`ProgressSink::start`].
    owns: AtomicBool,
    /// Set when the server is shutting down: the run stops at the next file.
    cancel: Arc<AtomicBool>,
}

impl SharedProgress {
    #[cfg(test)]
    fn new(shared: Arc<Mutex<IndexProgress>>) -> Self {
        Self::with_cancel(shared, Arc::new(AtomicBool::new(false)))
    }

    fn with_cancel(shared: Arc<Mutex<IndexProgress>>, cancel: Arc<AtomicBool>) -> Self {
        Self {
            shared,
            owns: AtomicBool::new(false),
            cancel,
        }
    }

    /// Claim the slot before the run has anything to count: loading (perhaps
    /// downloading) the model comes first and can take minutes, and a run that
    /// only showed up once it had diffed was invisible — to a poller, and to
    /// the idle watchdog, which then shut the server down under it.
    fn begin(&self, phase: &str) {
        let mut p = self.lock();
        if p.running {
            return;
        }
        self.owns.store(true, Ordering::SeqCst);
        p.running = true;
        p.run += 1;
        p.total = 0;
        p.done = 0;
        p.file.clear();
        p.phase.clear();
        p.phase.push_str(phase);
        p.advanced = Some(Instant::now());
    }

    /// A poisoned lock must never take an indexing run down with it: this is a
    /// counter for a progress bar, not part of the work. Recover and carry on,
    /// the same way [`AppState::checkpoint`] does.
    fn lock(&self) -> std::sync::MutexGuard<'_, IndexProgress> {
        self.shared
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Mark the run finished, keeping the final counts for whoever polls last.
    ///
    /// Only the owner may clear `running`: a concurrent run releasing a slot it
    /// never held would tell every poller that the run they are watching is
    /// over while it is still embedding files.
    fn finish(&self) {
        if self.owns.swap(false, Ordering::SeqCst) {
            let mut p = self.lock();
            p.running = false;
            // The end of a run is activity too: the idle timer counts from
            // here, not from the request that started it long ago (see
            // `AppState::last_index_activity`).
            p.advanced = Some(Instant::now());
        }
    }
}

impl ProgressSink for SharedProgress {
    fn start(&self, total: usize) {
        if self.owns.load(Ordering::SeqCst) {
            // Claimed at `begin`; now the size of the run is known.
            let mut p = self.lock();
            p.total = total;
            p.phase.clear();
            p.phase.push_str("files");
            p.advanced = Some(Instant::now());
            return;
        }
        self.begin("files");
        if self.owns.load(Ordering::SeqCst) {
            self.lock().total = total;
        }
    }

    fn phase(&self, name: &str) {
        if !self.owns.load(Ordering::SeqCst) {
            return;
        }
        let mut p = self.lock();
        if p.phase != name {
            p.phase.clear();
            p.phase.push_str(name);
        }
        p.advanced = Some(Instant::now());
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    fn file(&self, path: &str) {
        if !self.owns.load(Ordering::SeqCst) {
            return;
        }
        let mut p = self.lock();
        p.done += 1;
        p.file.clear();
        p.file.push_str(path);
        p.advanced = Some(Instant::now());
    }
}

/// Hand freed pages back to the kernel after a model is dropped.
///
/// Dropping the model frees its allocations, but glibc keeps the pages on its
/// own free lists rather than returning them, so the process's resident size
/// barely moves and the memory stays unavailable to everything else on the
/// machine — which is the whole point of releasing it. `malloc_trim` asks for
/// the top of each arena back.
///
/// glibc only; elsewhere the drop stands on whatever the allocator chooses to
/// do with it.
#[cfg(target_env = "gnu")]
fn trim_allocator() {
    // SAFETY: `malloc_trim` takes no pointers and touches only the allocator's
    // own bookkeeping. It is callable from any thread at any time.
    unsafe {
        libc::malloc_trim(0);
    }
}

#[cfg(not(target_env = "gnu"))]
fn trim_allocator() {}

/// A model held in memory, with the last time it was handed to a caller.
///
/// Loading one costs seconds; holding one costs hundreds of megabytes for as
/// long as the process lives. The timestamp is what lets
/// [`AppState::release_idle_models`] tell a model still in the working set from
/// one nobody has asked for since.
struct Cached<T> {
    value: T,
    last_used: Instant,
}

impl<T: Clone> Cached<T> {
    /// Hand out the value and record that it was wanted.
    fn touch(&mut self) -> T {
        self.last_used = Instant::now();
        self.value.clone()
    }
}

/// Server state shared across tool calls. Models are loaded on first use.
pub struct AppState {
    cfg: ProjectConfig,
    root: PathBuf,
    /// Git's top-level directory for `root`, found once (one `git` spawn) and
    /// reused: it cannot change while the server runs, and every search and
    /// graph call needs it. Unset until git first answers.
    toplevel: OnceLock<PathBuf>,
    /// Where `plans/` is read from (PLAN-007 DD-3): the project's own root, or the workspace
    /// that holds the plans of a group member. Decided once, at construction.
    plans_root: PlansRoot,
    /// The primary connection: opened once and kept for the server's lifetime so
    /// this process owns the DuckDB file (single writer). Request handlers get a
    /// cloned connection to the same in-process database via [`Store::try_clone`],
    /// so they never take a second file lock.
    primary: Arc<Mutex<Store>>,
    embed_settings: EmbedSettings,
    rerank_settings: RerankSettings,
    rerank_enabled: bool,
    /// Built on first use (see [`AppState::embedder`]), dropped again when it
    /// goes unused (see [`AppState::release_idle_models`]).
    embedder: Mutex<Option<Cached<Arc<dyn EmbeddingProvider>>>>,
    /// Built on first use (see [`AppState::reranker`]), dropped again when it
    /// goes unused.
    reranker: Mutex<Option<Cached<Arc<dyn Reranker>>>>,
    /// How far the current indexing run has got, for `/index/progress`.
    index_progress: Arc<Mutex<IndexProgress>>,
    /// Raised when the server is asked to stop: every indexing run in flight
    /// (and any that starts afterwards) stops at its next file boundary,
    /// commits, checkpoints and returns. See [`AppState::cancel_indexing`].
    index_cancel: Arc<AtomicBool>,
    /// Consecutive polls that found the project's `.devctx/` missing; see
    /// [`AppState::project_vanished`].
    vanish_strikes: AtomicU32,
}

impl AppState {
    /// Build state from a project config. Cheap: opens the store (dimension read
    /// from the registry) but does **not** load any model — those come lazily.
    pub fn build(cfg: ProjectConfig) -> anyhow::Result<Self> {
        let embed_settings = EmbedSettings::from_config(&cfg.embeddings);
        let rerank_enabled = cfg.reranking.enabled;
        let rerank_settings = RerankSettings {
            enabled: rerank_enabled,
            pool: cfg.reranking.pool,
            model: cfg.reranking.model.clone(),
            model_dir: (!cfg.reranking.model_dir.is_empty())
                .then(|| PathBuf::from(&cfg.reranking.model_dir)),
            device: cfg.reranking.device.with_env_override(),
        };
        let root = if cfg.project.path.is_empty() {
            std::env::current_dir()?
        } else {
            PathBuf::from(&cfg.project.path)
        };
        let dim = configured_dimension(&cfg);
        let primary = Store::open(&cfg.db_path(), dim)?;
        let plans_root = plans::resolve_plans_root(
            Some(&root),
            None,
            !cfg.project.group.trim().is_empty(),
            home_dir().as_deref(),
        );
        Ok(Self {
            cfg,
            root,
            toplevel: OnceLock::new(),
            plans_root,
            primary: Arc::new(Mutex::new(primary)),
            embed_settings,
            rerank_settings,
            rerank_enabled,
            embedder: Mutex::new(None),
            reranker: Mutex::new(None),
            index_progress: Arc::new(Mutex::new(IndexProgress::default())),
            index_cancel: Arc::new(AtomicBool::new(false)),
            vanish_strikes: AtomicU32::new(0),
        })
    }

    /// The directory `plan_status`, the plans graph and `memories_by_file` read `plans/` from.
    pub fn plans_root(&self) -> &PlansRoot {
        &self.plans_root
    }

    /// The embedding provider, built (and cached) on first use.
    fn embedder(&self) -> Result<Arc<dyn EmbeddingProvider>, String> {
        let mut guard = self.embedder.lock().map_err(|e| e.to_string())?;
        if let Some(c) = guard.as_mut() {
            return Ok(c.touch());
        }
        let e: Arc<dyn EmbeddingProvider> =
            Arc::from(create_provider(&self.embed_settings).map_err(|e| e.to_string())?);
        *guard = Some(Cached {
            value: e.clone(),
            last_used: Instant::now(),
        });
        Ok(e)
    }

    /// The reranker, built (and cached) on first use. Only called when enabled.
    fn reranker(&self) -> Result<Arc<dyn Reranker>, String> {
        let mut guard = self.reranker.lock().map_err(|e| e.to_string())?;
        if let Some(c) = guard.as_mut() {
            return Ok(c.touch());
        }
        let r: Arc<dyn Reranker> =
            Arc::from(create_reranker(&self.rerank_settings).map_err(|e| e.to_string())?);
        *guard = Some(Cached {
            value: r.clone(),
            last_used: Instant::now(),
        });
        Ok(r)
    }

    /// Drop any model that has not been asked for in `max_idle`, and name what
    /// went. Returns empty when there was nothing to release.
    ///
    /// A server stays up for its whole idle window so the next command finds it
    /// warm, but "warm" need not mean holding a cross-encoder the whole time.
    /// One server per project path means several coexist, and each was keeping
    /// its models for the life of the process: an embedder is hundreds of
    /// megabytes and a cross-encoder gigabytes, so a few projects touched in the
    /// same quarter of an hour added up to more memory than the machine had.
    /// Empty, a server costs about fifty megabytes.
    ///
    /// The cost of getting it wrong is one reload — seconds on the next request
    /// — so this errs towards releasing. A model handed out and still in use is
    /// safe regardless: the caller holds an `Arc`, and dropping this one only
    /// means the *next* caller builds a fresh one.
    pub fn release_idle_models(&self, max_idle: Duration) -> Vec<&'static str> {
        let mut released = Vec::new();
        if let Ok(mut guard) = self.embedder.lock() {
            if guard
                .as_ref()
                .is_some_and(|c| c.last_used.elapsed() >= max_idle)
            {
                *guard = None;
                released.push("embedding model");
            }
        }
        if let Ok(mut guard) = self.reranker.lock() {
            if guard
                .as_ref()
                .is_some_and(|c| c.last_used.elapsed() >= max_idle)
            {
                *guard = None;
                released.push("reranker");
            }
        }
        if !released.is_empty() {
            trim_allocator();
        }
        released
    }

    /// Fold the write-ahead log into the database file before the process goes
    /// away. A WAL that outlives its writer leaves the ART indexes behind every
    /// `PRIMARY KEY` and `UNIQUE` missing entries, and the next delete then
    /// fails for good — see [`Store::checkpoint`].
    pub fn checkpoint(&self) {
        // Recover from a poisoned lock rather than return quietly. This runs on
        // the way out, and the panic that poisoned the mutex is exactly the kind
        // of exit that leaves a WAL behind — skipping the fold here would drop
        // it in the one case it matters most.
        let store = self
            .primary
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        store.checkpoint();
    }

    /// The checkpoint for a process that is about to end, whatever else is
    /// running: a plain `CHECKPOINT` first, escalated to `FORCE CHECKPOINT`
    /// (which aborts other connections' open transactions — they are about to
    /// die with the process anyway) when the plain one is refused. Returns
    /// whether the WAL was folded.
    ///
    /// The database is [frozen](Store::freeze) first — waiting at most
    /// `freeze_wait` for a statement already running — so nothing written
    /// after this checkpoint can reach a WAL the process will not fold: an
    /// indexing thread still alive at `_exit` has its next write refused
    /// instead.
    pub fn checkpoint_for_exit(&self, freeze_wait: Duration) -> bool {
        let store = self
            .primary
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !store.freeze(freeze_wait) {
            eprintln!(
                "DevCtxEngine: a write was still running {freeze_wait:?} into the exit; \
                 checkpointing anyway (FORCE aborts it)"
            );
        }
        match store.try_checkpoint() {
            Ok(()) => true,
            Err(plain) => match store.force_checkpoint() {
                Ok(()) => true,
                Err(forced) => {
                    eprintln!(
                        "DevCtxEngine: the exit checkpoint failed ({plain}; forced: {forced}); \
                         the write-ahead log is left for the next open to replay"
                    );
                    false
                }
            },
        }
    }

    /// Ask every indexing run to stop at its next file boundary (the server is
    /// shutting down). Irreversible for this process: a run started afterwards
    /// stops before its first file.
    pub fn cancel_indexing(&self) {
        self.index_cancel.store(true, Ordering::SeqCst);
    }

    /// Whether an indexing run is writing to the database and reported progress
    /// within `quiet`. What the stop watchdog asks while a cancelled run winds
    /// down: a run still moving is finishing its file or its checkpoint and
    /// deserves the wait; one that is not is stuck.
    ///
    /// A run still loading its model does not count: it has written nothing,
    /// it cannot be cancelled (the load is not ours to interrupt), and waiting
    /// on it is waiting on the network.
    pub fn index_winding_down(&self, quiet: Duration) -> bool {
        let p = self
            .index_progress
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        p.running && p.phase != LOADING_MODEL && p.advanced.is_some_and(|at| at.elapsed() < quiet)
    }

    fn open_store(&self) -> Result<Store, String> {
        // Hand out a fresh connection to the same in-process database; the mutex
        // is held only for the cheap clone, not for the query.
        self.primary
            .lock()
            .map_err(|e| e.to_string())?
            .try_clone()
            .map_err(|e| e.to_string())
    }

    /// Project name for memory scoping.
    pub fn project(&self) -> String {
        if self.cfg.project.name.is_empty() {
            "default".to_string()
        } else {
            self.cfg.project.name.clone()
        }
    }

    /// The group this repository belongs to, empty when it stands alone.
    pub fn group_name(&self) -> String {
        self.cfg.project.group.clone()
    }

    /// The branch this project keeps indexed by default, if it declares one.
    ///
    /// What search falls back to when the checked-out branch has never been
    /// indexed — the ordinary state of a branch created five minutes ago, and
    /// of every linked worktree in a repository that indexes only its trunk.
    pub fn default_branch(&self) -> Option<String> {
        self.cfg.indexing.default_branch().map(str::to_string)
    }

    /// The key the store files this project's index under: git's top-level
    /// directory, which is what the indexing pipeline uses. Not `self.root`
    /// (the configured project path), which differs when the project is a
    /// subdirectory of the repository or reached through a symlink.
    pub fn repo_path(&self) -> String {
        match self.git() {
            Ok(g) => g.root().to_string_lossy().into_owned(),
            Err(_) => self.root.to_string_lossy().into_owned(),
        }
    }

    /// The repository, with its top-level directory cached after the first
    /// success so later calls spawn nothing.
    fn git(&self) -> Result<GitRepo, String> {
        if let Some(top) = self.toplevel.get() {
            return Ok(GitRepo::at_root(top.clone()));
        }
        let git = GitRepo::open(&self.root).map_err(|e| e.to_string())?;
        let _ = self.toplevel.set(git.root().to_path_buf());
        Ok(git)
    }

    /// The short repo name + branch (for graph queries), from git. One `git`
    /// spawn once the top level is cached (the branch itself can change).
    pub fn repo_branch(&self) -> Result<(String, String), String> {
        let git = self.git()?;
        Ok((git.short_name(), git.branch()))
    }
}

/// The `repo_path` key the store uses for the repository containing `root`:
/// git's top-level directory (as the indexing pipeline records it), falling
/// back to `root` itself when it is not a git repository.
pub fn repo_key(root: &std::path::Path) -> String {
    GitRepo::open(root)
        .map(|g| g.root().to_string_lossy().into_owned())
        .unwrap_or_else(|_| root.to_string_lossy().into_owned())
}

/// The store vector dimension for a config, read from the registry so we don't
/// have to load the model just to open the store.
fn configured_dimension(cfg: &ProjectConfig) -> usize {
    devctx_embed::dimension_for(&cfg.embeddings.provider, &cfg.embeddings.model)
}

/// `search` tool: vector / keyword / hybrid search, then rerank, return JSON hits.
pub fn do_search(
    state: &AppState,
    query: &str,
    limit: usize,
    language: Option<String>,
    mode: SearchMode,
    rerank: bool,
) -> Result<String, String> {
    let store = state.open_store()?;
    let (branch_filter, fallback) = search_branch(state, &store);
    let filter = SearchFilter {
        languages: language.into_iter().collect(),
        exclude_deletions: true,
        ..branch_filter
    };
    // Keyword search needs the BM25 index, which is opt-in and therefore usually
    // absent. Building it here — the user has just asked for the feature — turns
    // a raw `match_bm25 does not exist` catalog error into a one-off wait.
    if mode != SearchMode::Vector && !store.has_fts() {
        match store.rebuild_fts() {
            Ok(true) => eprintln!("· built the keyword (BM25) index for this project"),
            Ok(false) if mode == SearchMode::Keyword => {
                return Err("keyword search needs DuckDB's FTS extension, which is \
                            unavailable here (it is downloaded on first use, so this \
                            usually means no network). Use vector search instead."
                    .to_string())
            }
            Ok(false) => {}
            Err(e) if mode == SearchMode::Keyword => {
                return Err(format!("building the keyword index failed: {e}"))
            }
            Err(_) => {}
        }
    }

    let embedder = if mode == SearchMode::Keyword {
        None
    } else {
        Some(state.embedder()?)
    };
    // Reranking is by far the most expensive stage — a cross-encoder pass over the
    // whole candidate pool, seconds on CPU against milliseconds for the search
    // itself. Callers that value latency over ordering must be able to skip it,
    // and until this flag existed `--no-rerank` was silently ignored whenever a
    // command routed through the server, which is the normal case.
    let reranker = if rerank && state.rerank_enabled && mode != SearchMode::Keyword {
        Some(state.reranker()?)
    } else {
        None
    };
    let hits = devctx_search::search(
        &store,
        query,
        &filter,
        limit,
        mode,
        embedder.as_deref(),
        reranker.as_deref(),
    )
    .map_err(|e| e.to_string())?;
    let items = match hits_to_json(&hits) {
        Value::Array(a) => a,
        other => return serde_json::to_string_pretty(&other).map_err(|e| e.to_string()),
    };
    let budget = env_usize("DEVCTX_MAX_OUTPUT_TOKENS", DEFAULT_MAX_OUTPUT_TOKENS);
    let (kept, dropped) = fit_json_array(items, budget, Some("text"), |v| {
        let file = v.get("file").and_then(|f| f.as_str()).unwrap_or("");
        let line = v.get("start_line").and_then(|l| l.as_i64()).unwrap_or(0);
        format!("{file}:{line}")
    });
    if dropped.is_empty() && fallback.is_none() {
        return serde_json::to_string_pretty(&Value::Array(kept)).map_err(|e| e.to_string());
    }
    let mut out = json!({ "results": kept });
    if !dropped.is_empty() {
        out["omitted_for_budget"] = json!({ "count": dropped.len(), "items": dropped });
    }
    if let Some(f) = &fallback {
        out["branch_fallback"] = f.to_json();
    }
    serde_json::to_string_pretty(&out).map_err(|e| e.to_string())
}

/// Drop rows for branches the config no longer lists. Returns rows removed.
///
/// Never runs when the list is empty: that means "track whatever is checked
/// out", and treating it as "track nothing" would delete the entire index of
/// every project that has not configured this.
fn prune_untracked_branches(state: &AppState, store: &devctx_store::Store) -> usize {
    if state.cfg.indexing.branches.is_empty() {
        return 0;
    }
    let Ok((repo, current)) = state.repo_branch() else {
        return 0;
    };
    let repo_path = state.repo_path();
    let Ok(indexed) = store.indexed_branches(&repo_path) else {
        return 0;
    };
    let mut removed = 0;
    for b in indexed {
        // The checked-out branch is spared even when untracked: someone is
        // standing in it, and deleting the index under their feet mid-session
        // is not a tidy-up.
        if b == current || state.cfg.indexing.tracks(&b) {
            continue;
        }
        removed += store.drop_branch(&repo, &repo_path, &b).unwrap_or(0);
    }
    removed
}

/// Narrow a search to one branch — when, and only when, that is meaningful.
///
/// A store may hold several branches at once, and mixing them answers with two
/// versions of the same file and no way to tell which is which. But narrowing
/// blindly is worse: a branch nobody has indexed would return nothing at all,
/// which is what a freshly created branch always is, and the moment someone
/// most wants to search.
///
/// So: the checked-out branch if it has rows, else the configured default if it
/// has rows, else the most recently indexed branch with rows (the same rule as
/// the graph tools, via `pick_graph_branch`), else no filter — which is exactly
/// the behaviour of every version before branches were tracked.
fn search_branch(
    state: &AppState,
    store: &devctx_store::Store,
) -> (SearchFilter, Option<BranchFallback>) {
    // One rule for search and graph: see `pick_graph_branch`.
    match pick_branch(state, store) {
        Ok(c) if c.indexed => {
            let filter = SearchFilter {
                repo: Some(c.repo),
                branch: Some(c.branch),
                ..SearchFilter::default()
            };
            (filter, c.fallback)
        }
        _ => (SearchFilter::default(), None),
    }
}

/// The checked-out branch had nothing indexed, so an answer came from another.
///
/// Reported in the tool output, never silently: the other branch may hold code
/// that has since changed in the one the caller is standing in.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BranchFallback {
    current: String,
    used: String,
    /// The checked-out branch has an index record but no rows (an index that
    /// ran and found nothing), as opposed to never having been indexed.
    current_empty: bool,
}

impl BranchFallback {
    fn to_json(&self) -> Value {
        let state = if self.current_empty {
            "is indexed but empty"
        } else {
            "is not indexed"
        };
        json!({
            "current": self.current,
            "used": self.used,
            "why": format!(
                "current branch {} {state}; answering from branch {}",
                self.current, self.used
            ),
        })
    }
}

/// The branch a graph query (symbols, references, routes, impact) runs against.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BranchChoice {
    repo: String,
    branch: String,
    fallback: Option<BranchFallback>,
    /// `false` when no branch has rows at all.
    indexed: bool,
    /// The chosen branch was indexed by an older (or unknown) extractor, so
    /// symbols and edges may differ from what a fresh index would hold.
    extractor_stale: bool,
}

/// The line the graph tools add, and `index_status` repeats, for such an index.
///
/// An index made before `index_meta` existed (devctx <= 0.8.2) reads as stale
/// too, and a new branch indexed from it copies nothing from it: each new
/// branch re-embeds everything until one `devctx index --full` stamps it.
pub const STALE_EXTRACTOR_WARNING: &str =
    "index generated by an older extractor (or by a version that left no record); \
     reindex (devctx index --full). Until then new branches re-embed every file instead \
     of copying from this one";

impl BranchChoice {
    /// The branch to query, or the explicit "nothing indexed" error — an empty
    /// answer from an empty index reads as "no such symbol", which is false.
    fn ready(self) -> Result<Self, String> {
        if self.indexed {
            Ok(self)
        } else {
            Err(format!(
                "{} has no index for any branch; run devctx index",
                self.repo
            ))
        }
    }

    /// Adds `branch_fallback` when the branch is not the current one and
    /// `warning` when its index comes from an older extractor.
    fn annotate(&self, out: &mut Value) {
        if let Some(f) = &self.fallback {
            out["branch_fallback"] = f.to_json();
        }
        if self.extractor_stale {
            out["warning"] = json!(STALE_EXTRACTOR_WARNING);
        }
    }
}

/// Same rule as [`search_branch`] — checked-out branch if it has rows, else the
/// configured default if it has rows — plus one more step the graph needs
/// because it cannot run unfiltered: the most recently indexed branch that has
/// rows. If none does, `indexed` is false.
fn pick_graph_branch(
    store: &devctx_store::Store,
    repo: &str,
    repo_path: &str,
    current: &str,
    default: Option<&str>,
) -> BranchChoice {
    let has = |b: &str| store.has_branch_rows(repo, b).unwrap_or(false);
    let choice = |branch: &str, indexed: bool| BranchChoice {
        repo: repo.to_string(),
        branch: branch.to_string(),
        fallback: (indexed && branch != current).then(|| BranchFallback {
            current: current.to_string(),
            used: branch.to_string(),
            current_empty: store
                .get_index_record(repo_path, current)
                .map(|r| r.is_some())
                .unwrap_or(false),
        }),
        indexed,
        // A failed read counts as unknown, which is stale: never a silent "fine".
        extractor_stale: indexed
            && store
                .extractor_stale(repo_path, branch, &devctx_index::extractor_fingerprint())
                .unwrap_or(true),
    };
    if has(current) {
        return choice(current, true);
    }
    if let Some(d) = default.filter(|d| has(d)) {
        return choice(d, true);
    }
    let latest = store
        .branches_by_recency(repo_path)
        .unwrap_or_default()
        .into_iter()
        .find(|b| has(b));
    match latest {
        Some(b) => choice(&b, true),
        None => choice(current, false),
    }
}

/// The branch choice for this project, without insisting that something is
/// indexed (callers that can answer from nothing — `graph`, `search` — use this).
fn pick_branch(state: &AppState, store: &devctx_store::Store) -> Result<BranchChoice, String> {
    let (repo, current) = state.repo_branch()?;
    let repo_path = state.repo_path();
    Ok(pick_graph_branch(
        store,
        &repo,
        &repo_path,
        &current,
        state.default_branch().as_deref(),
    ))
}

fn graph_branch(state: &AppState, store: &devctx_store::Store) -> Result<BranchChoice, String> {
    pick_branch(state, store)?.ready()
}

/// The branch a graph-shaped query should run against for the repository at
/// `root`, for callers that hold a store but no [`AppState`] (the local CLI).
/// Returns `(repo, branch, branch_fallback, extractor_stale)`; `branch_fallback`
/// is the same JSON note the MCP tools emit, and `extractor_stale` says the
/// chosen branch was indexed by an older extractor (see
/// [`STALE_EXTRACTOR_WARNING`]).
pub fn graph_target(
    store: &devctx_store::Store,
    root: &std::path::Path,
    default_branch: Option<&str>,
) -> Result<(String, String, Option<Value>, bool), String> {
    let git = GitRepo::open(root).map_err(|e| e.to_string())?;
    let repo_path = git.root().to_string_lossy().into_owned();
    let c = pick_graph_branch(
        store,
        &git.short_name(),
        &repo_path,
        &git.branch(),
        default_branch,
    );
    let fallback = c.fallback.as_ref().map(BranchFallback::to_json);
    Ok((c.repo, c.branch, fallback, c.extractor_stale))
}

/// Parse an optional mode string into a [`SearchMode`] (default vector).
pub fn parse_mode(mode: Option<&str>) -> SearchMode {
    match mode.map(str::to_ascii_lowercase).as_deref() {
        Some("keyword") => SearchMode::Keyword,
        Some("hybrid") => SearchMode::Hybrid,
        _ => SearchMode::Vector,
    }
}

/// `read_file` tool: read a repo file, optionally a 1-based inclusive line range.
pub fn do_read_file(
    state: &AppState,
    path: &str,
    start_line: Option<usize>,
    end_line: Option<usize>,
) -> Result<String, String> {
    let full = state.root.join(path);
    let content =
        std::fs::read_to_string(&full).map_err(|e| format!("reading {}: {e}", full.display()))?;
    // A caller that named a range has already decided how much it wants; capping
    // that would silently deliver less than was asked for. The budget exists for
    // the blind case — "read this file" against something that turns out to be
    // twenty thousand lines, which arrives as a six-figure token bill nobody
    // chose to pay.
    if start_line.is_some() || end_line.is_some() {
        return Ok(slice_lines(&content, start_line, end_line));
    }
    let budget = env_usize("DEVCTX_MAX_OUTPUT_TOKENS", DEFAULT_MAX_OUTPUT_TOKENS);
    Ok(cap_whole_file(&content, budget))
}

/// Characters per token, roughly, for budgeting. Deliberately crude: the cost of
/// a real tokenizer here is not worth the precision, and the budget is a guard
/// rail rather than an accounting figure.
const CHARS_PER_TOKEN: usize = 4;

/// Read a `usize` from the environment, falling back when absent or unparseable.
fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Default budget for a whole-file read, in tokens. `0` disables the cap.
const DEFAULT_MAX_OUTPUT_TOKENS: usize = 8000;

/// Trim a whole-file read to the output budget, on line boundaries.
///
/// Cut *lines*, never bytes: a fragment of a line is unreadable as code, and the
/// line numbers of what survives have to stay meaningful for the follow-up range
/// request. The trailing note carries the file's real length, which is the one
/// fact the caller needs to ask for the rest — without it a truncated read looks
/// exactly like a short file, and an agent reasons on half a class believing it
/// has seen all of it.
/// `budget_tokens` is passed in rather than read here: the environment is
/// process-global, and tests that set it race each other.
fn cap_whole_file(content: &str, budget_tokens: usize) -> String {
    if budget_tokens == 0 || content.len() <= budget_tokens * CHARS_PER_TOKEN {
        return content.to_string();
    }
    let budget = budget_tokens * CHARS_PER_TOKEN;
    let mut out = String::with_capacity(budget + 160);
    let mut kept = 0usize;
    for line in content.lines() {
        if out.len() + line.len() + 1 > budget {
            break;
        }
        out.push_str(line);
        out.push('\n');
        kept += 1;
    }
    let total = content.lines().count();
    out.push_str(&format!(
        "\n[devctx] truncated: lines 1-{kept} of {total}. \
         Request the rest with start_line/end_line.\n"
    ));
    out
}

/// `index_repo` tool: run the pipeline and return a summary.
pub fn do_index(state: &AppState, full: bool) -> Result<String, String> {
    do_index_inner(state, full, None, None)
}

/// Index a named branch. `None` falls back to the project's declared default,
/// then to whatever is checked out.
///
/// Separate from [`do_index`] because the routed path used to drop the branch
/// on the floor: `devctx index --branch X` with a server running indexed the
/// server's branch instead, and said nothing. A caller asking for a specific
/// branch and silently getting another is worse than an error.
pub fn do_index_on(state: &AppState, full: bool, branch: Option<String>) -> Result<String, String> {
    do_index_inner(state, full, None, branch)
}

/// Index exactly these repo-relative paths — what a file watcher needs, since a
/// save moves no commit and the commit diff would therefore be empty.
pub fn do_index_paths(state: &AppState, paths: &[String]) -> Result<String, String> {
    do_index_inner(state, false, Some(paths), None)
}

/// Load the embedder, reporting progress whenever the model cache grows.
///
/// A model that has to be downloaded first can take minutes inside an
/// `/index` request, and the run is real work all that time. Only *growth*
/// counts: a download that stopped receiving data does not tick, so it loses
/// the idle exemption like any other stuck run (and `guard_load` fails it).
fn embedder_reporting(
    state: &AppState,
    sink: &SharedProgress,
) -> Result<Arc<dyn EmbeddingProvider>, String> {
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    std::thread::scope(|s| {
        s.spawn(move || {
            let mut last = None;
            while let Err(std::sync::mpsc::RecvTimeoutError::Timeout) =
                rx.recv_timeout(Duration::from_secs(1))
            {
                let now = devctx_core::modelload::cache_fingerprint();
                if last.is_some() && now != last {
                    sink.phase(LOADING_MODEL);
                }
                last = now;
            }
        });
        let out = state.embedder();
        drop(tx);
        out
    })
}

fn do_index_inner(
    state: &AppState,
    full: bool,
    paths: Option<&[String]>,
    branch: Option<String>,
) -> Result<String, String> {
    let store = state.open_store()?;
    let sink =
        SharedProgress::with_cancel(state.index_progress.clone(), state.index_cancel.clone());
    sink.begin(LOADING_MODEL);
    let embedder = match embedder_reporting(state, &sink) {
        Ok(e) => e,
        Err(e) => {
            sink.finish();
            return Err(e);
        }
    };
    // Which branch this run is about. Declared config wins over what happens to
    // be checked out, so running `index` from a linked worktree keeps the
    // repository's trunk fresh instead of quietly indexing the worktree's
    // branch into the same store — which is how two branches end up mixed.
    // An explicit request wins over the project's default, which wins over the
    // checked-out branch.
    let target_branch = branch.or_else(|| state.default_branch());
    // The run stays "running" until this function returns — through pruning
    // other branches and the report, which still write to the database — so
    // neither the idle timer nor a stop request reads it as over while it is
    // not. Dropped on every path, including an error.
    struct Finish<'a>(&'a SharedProgress);
    impl Drop for Finish<'_> {
        fn drop(&mut self) {
            self.0.finish();
        }
    }
    let _finish = Finish(&sink);
    let run = index_run(IndexRequest {
        store: &store,
        embedder: embedder.as_ref(),
        repo_root: &state.root,
        incremental: !full,
        model_name: &state.cfg.embeddings.model,
        progress: Some(&sink),
        paths,
        exclude: &state.cfg.indexing.exclude,
        branch: target_branch.as_deref(),
    });
    let res = run.map_err(|e| e.to_string())?;
    if res.cancelled {
        // The server is going away: say what happened and touch nothing else
        // (pruning other branches is housekeeping, not something to start now).
        return Ok(json!({
            "cancelled": true,
            "commit": res.commit,
            "branch": res.branch,
            "files_indexed": res.files_indexed,
            "hint": "indexing was cancelled because the server is stopping; what was \
                     indexed is saved, and the next `devctx index` resumes from the same commit",
        })
        .to_string());
    }

    // Anything the config no longer declares is dropped now. Doing it here,
    // rather than in a command someone has to remember, is what keeps a branch
    // list from being a suggestion: merged-and-deleted branches leave, and a
    // name reused later cannot inherit their rows.
    let pruned = prune_untracked_branches(state, &store);
    report_index(&store, &state.root, &res);
    if pruned > 0 {
        eprintln!("· dropped {pruned} row(s) of branches this project no longer tracks");
    }
    let mut report = json!({
        "commit": res.commit,
        "branch": res.branch,
        "full_reindex": res.full_reindex,
        "files_indexed": res.files_indexed,
        "files_skipped": res.files_skipped,
        "files_deleted": res.files_deleted,
        "files_pruned": res.files_pruned,
        "files_renamed": res.files_renamed,
        "files_copied": res.files_copied,
        "symbols": res.symbols,
        "chunks": res.chunks,
    });
    if res.extractor_stale {
        report["extractor_stale"] = json!(true);
        report["hint"] = json!(
            "this index was built by an older extractor and an incremental run keeps its old \
             symbols and edges; run `devctx index --full` to rebuild it"
        );
    }
    Ok(report.to_string())
}

/// How far the indexing run in this server has got.
///
/// Deliberately cheap: it copies four fields under a short lock and touches no
/// database. It is polled *while* an index is running, so anything heavier
/// would queue behind the very work it reports on and arrive too late to be
/// worth reporting.
impl AppState {
    /// Whether the project this server owns has been deleted from under it (its
    /// directory or its `.devctx/`). Such a server serves nothing anyone can
    /// reach again, and the test-suite leak of PLAN-008 B9 was exactly one.
    ///
    /// Only a definite "not found" counts, and only [`VANISH_STRIKES`] times in
    /// a row: on WSL's `/mnt/c` (drvfs, 9p) a stat can fail transiently with
    /// EIO or EACCES, and a directory can be missing for an instant during a
    /// rename — reading either as "deleted" killed healthy servers. The caller
    /// (the watchdog) additionally defers while requests or an index are in
    /// flight.
    pub fn project_vanished(&self) -> bool {
        if !dir_definitely_missing(&self.root.join(".devctx")) {
            self.vanish_strikes.store(0, Ordering::SeqCst);
            return false;
        }
        self.vanish_strikes.fetch_add(1, Ordering::SeqCst) + 1 >= VANISH_STRIKES
    }

    /// When an indexing run last moved — reached a file, reported a phase, or
    /// finished. `None` before the first run.
    ///
    /// The idle timer counts from the later of this and the last request: a
    /// run's request was stamped when it *started*, minutes ago, and between
    /// the run marking itself finished and its answer leaving the server there
    /// is still work (pruning branches, the report). Counting only requests,
    /// an idle window shorter than the run killed the server in that gap, with
    /// the answer unsent.
    pub fn last_index_activity(&self) -> Option<Instant> {
        self.index_progress
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .advanced
    }

    /// Whether an indexing run is in flight at all, advancing or not.
    pub fn index_running(&self) -> bool {
        self.index_progress
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .running
    }

    /// Whether an indexing run is in flight right now.
    ///
    /// The idle watchdog asks before shutting the server down: indexing happens
    /// *inside* the server, so a run whose client has stopped asking about it is
    /// still real work, and exiting would throw away everything it has done.
    ///
    /// Only a run that is still *advancing* counts. One that is `running` but
    /// has not reached a new file for [`index_stall_limit`] is stuck, and
    /// exempting it from the idle timeout forever made a wedged server
    /// immortal (PLAN-008 B11).
    pub fn is_indexing(&self) -> bool {
        let p = self
            .index_progress
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        p.running
            && p.advanced
                .is_none_or(|at| at.elapsed() < index_stall_limit())
    }
}

/// The progress phase of a run that is still loading (or downloading) its model.
const LOADING_MODEL: &str = "loading model";

/// Consecutive "not found" polls before a project counts as deleted.
pub const VANISH_STRIKES: u32 = 3;

/// `path` is reported as not existing — not unreadable, not erroring.
fn dir_definitely_missing(path: &std::path::Path) -> bool {
    match std::fs::metadata(path) {
        Ok(_) => false,
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
    }
}

/// How long an indexing run may go without reaching a new file before it stops
/// excusing the server from its idle timeout. `DEVCTX_INDEX_STALL_SECS`
/// overrides the 15 minutes (tests use seconds). Generous on purpose: one very
/// large file is slow, not stuck.
fn index_stall_limit() -> Duration {
    let secs: u64 = std::env::var("DEVCTX_INDEX_STALL_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(900);
    Duration::from_secs(secs)
}

pub fn do_index_progress(state: &AppState) -> Result<String, String> {
    let p = state
        .index_progress
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    Ok(json!({
        "running": p.running,
        "run": p.run,
        "total": p.total,
        "done": p.done,
        "file": p.file,
        "phase": p.phase,
    })
    .to_string())
}

/// What `index_status` says when the checked-out branch has no index record.
///
/// Three different situations used to share one sentence that read
/// "branch X is not indexed; search answers from branch X" (the same branch on
/// both sides) in a repository that had never been indexed at all.
fn no_record_hint(indexed_branches: &[String], chosen: &BranchChoice, current: &str) -> String {
    if indexed_branches.is_empty() {
        // No record for any branch: nothing to fall back to either.
        "nothing indexed yet; run `devctx index`".to_string()
    } else if !chosen.indexed {
        "nothing is indexed for this repository; run devctx index".to_string()
    } else if chosen.branch == current {
        // Rows with no completed record: an index that was cut short.
        format!(
            "branch {current} has rows but no completed index record (an interrupted run?); \
             run `devctx index` to finish it"
        )
    } else {
        // Name the branch search will actually use, not every candidate.
        format!(
            "branch {current} is not indexed; search and the graph tools answer from branch \
             {}; run devctx index to index this one",
            chosen.branch
        )
    }
}

/// `index_status` tool: report the last-indexed record for the repo/branch.
pub fn do_index_status(state: &AppState) -> Result<String, String> {
    let store = state.open_store()?;
    let git = state.git()?;
    let state_git = git.state();
    let repo_path = git.root().to_string_lossy().to_string();
    let record = store
        .get_index_record(&repo_path, &state_git.branch)
        .map_err(|e| e.to_string())?;
    let value = match record {
        None => {
            let indexed_branches = store.branches_by_recency(&repo_path).unwrap_or_default();
            let chosen = pick_graph_branch(
                &store,
                &git.short_name(),
                &repo_path,
                &state_git.branch,
                state.default_branch().as_deref(),
            );
            let hint = no_record_hint(&indexed_branches, &chosen, &state_git.branch);
            json!({
                "indexed": false,
                "branch": state_git.branch,
                "indexed_branches": indexed_branches,
                "hint": hint,
            })
        }
        Some(r) => {
            let stale = store
                .extractor_stale(
                    &repo_path,
                    &r.branch,
                    &devctx_index::extractor_fingerprint(),
                )
                .unwrap_or(true);
            let mut v = json!({
            "indexed": true,
            "branch": r.branch,
            "last_commit": r.last_commit,
            "model": r.model_name,
            "dimension": r.model_dimension,
            "files": r.file_count,
            "symbols": r.symbol_count,
            "chunks": r.chunk_count,
            "indexed_at": r.indexed_at,
            "head_commit": state_git.commit,
            "up_to_date": r.last_commit == state_git.commit,
            "extractor_stale": stale,
            });
            if stale {
                v["hint"] = json!(
                    "index built by an older extractor (or one that left no record, as in \
                     releases up to 0.8.2); run `devctx index --full` to rebuild it. Until \
                     then new branches re-embed every file instead of copying"
                );
            }
            // A record with no rows: the index ran and found nothing to keep.
            // Say so rather than let `indexed: true` read as "searchable".
            let empty = !store
                .has_branch_rows(&git.short_name(), &r.branch)
                .unwrap_or(true);
            if empty {
                v["empty"] = json!(true);
                let empty_hint = format!(
                    "branch {} is indexed but empty (no rows); search and the graph tools \
                     answer from another branch if one has rows",
                    r.branch
                );
                // Both can apply (an old extractor that found nothing): keep the
                // stale-extractor advice instead of overwriting it.
                v["hint"] = json!(match v["hint"].as_str() {
                    Some(stale_hint) => format!("{stale_hint}. Also: {empty_hint}"),
                    None => empty_hint,
                });
            }
            v
        }
    };
    Ok(value.to_string())
}

/// `plan_status` tool: read `plans/PLAN-*/` (see `plans/PLAN-005-plan-status/`) and answer
/// "where am I": the list of plans with no argument, or one plan's ready/in-progress/blocked
/// tasks with `plan`. Reads markdown from disk on every call — plans are the source of truth,
/// never copied into the store (PLAN-005 §3).
pub fn do_plan_status(state: &AppState, plan: Option<&str>) -> Result<String, String> {
    plan_status_budgeted(&state.plans_root, plan)
}

/// `plan_status_value_in` trimmed to `DEVCTX_MAX_OUTPUT_TOKENS`. The single path behind the
/// daemon's `do_plan_status` and the MCP's in-process answer in group mode (PLAN-007 DD-4).
pub fn plan_status_budgeted(root: &PlansRoot, plan: Option<&str>) -> Result<String, String> {
    let value = plan_status_value_in(root, plan)?;
    let budget = env_usize("DEVCTX_MAX_OUTPUT_TOKENS", DEFAULT_MAX_OUTPUT_TOKENS);
    Ok(fit_plan_status_budget(value, budget))
}

/// The user's home directory, the upper bound of the plans-root ancestor walk.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

/// The unbudgeted `plan_status` JSON, built straight from disk. Shared by `do_plan_status`
/// (which trims it to `DEVCTX_MAX_OUTPUT_TOKENS`) and the CLI's `devctx plan-status`, which
/// wants the same shape without going through `AppState` — no store, no daemon, just markdown.
pub fn plan_status_value(root: &std::path::Path, plan: Option<&str>) -> Result<Value, String> {
    plan_status_value_in(
        &PlansRoot {
            root: root.to_path_buf(),
            source: plans::PlansRootSource::Project,
        },
        plan,
    )
}

/// Like [`plan_status_value`], for a root that was resolved: the JSON also says where the plans
/// came from (`plans_root: {path, source}`), so nobody has to wonder which `plans/` was read.
pub fn plan_status_value_in(root: &PlansRoot, plan: Option<&str>) -> Result<Value, String> {
    let loaded = plans::load_plans(&root.root);
    let mut value = match plan {
        None => plan_status_list(&loaded),
        Some(query) => plan_status_detail(&loaded, query)?,
    };
    value["plans_root"] = json!({
        "path": root.root.to_string_lossy(),
        "source": root.source.as_str(),
    });
    Ok(value)
}

/// The plan with the most recently modified `.md` file among plans that still have at least one
/// non-done task. A tie (e.g. a fresh clone or checkout, which equalizes mtimes) is broken by the
/// highest plan number. If every plan is fully done (or there are no plans), there is no active one.
fn active_plan_id(plans: &[Plan]) -> Option<String> {
    plans
        .iter()
        .filter(|p| !p.tasks.is_empty() && p.tasks.iter().any(|t| !t.status.is_resolved()))
        .max_by(|a, b| {
            let by_mtime = a.mtime.cmp(&b.mtime);
            if by_mtime != std::cmp::Ordering::Equal {
                return by_mtime;
            }
            plan_number(&a.id).cmp(&plan_number(&b.id))
        })
        .map(|p| p.id.clone())
}

fn plan_number(id: &str) -> u32 {
    id.trim_start_matches("PLAN-").parse().unwrap_or(0)
}

fn plan_status_list(plans: &[Plan]) -> Value {
    let active = active_plan_id(plans);
    let items: Vec<Value> = plans
        .iter()
        .map(|p| {
            let done = p.tasks.iter().filter(|t| t.status.is_done()).count();
            let skipped = p
                .tasks
                .iter()
                .filter(|t| matches!(t.status, plans::Status::Skipped))
                .count();
            json!({
                "id": p.id,
                "title": p.title,
                "done": done,
                "skipped": skipped,
                "total": p.tasks.len(),
                "active": active.as_deref() == Some(p.id.as_str()),
                "task_files": !p.tasks.is_empty(),
                "warnings_count": p.warnings.len(),
            })
        })
        .collect();
    json!({ "plans": items, "active": active })
}

/// Resolves a user-supplied plan identifier (`PLAN-005`, `005`, `5`, or the directory name)
/// against the loaded plans.
fn find_plan<'a>(plans: &'a [Plan], query: &str) -> Option<&'a Plan> {
    let q = query.trim();
    let normalized = if q.to_uppercase().starts_with("PLAN-") {
        let n: u32 = q[5..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>()
            .parse()
            .ok()?;
        Some(format!("PLAN-{n:03}"))
    } else if q.chars().all(|c| c.is_ascii_digit()) && !q.is_empty() {
        let n: u32 = q.parse().ok()?;
        Some(format!("PLAN-{n:03}"))
    } else {
        None
    };
    if let Some(id) = &normalized {
        if let Some(p) = plans.iter().find(|p| &p.id == id) {
            return Some(p);
        }
    }
    plans
        .iter()
        .find(|p| p.dir.file_name().and_then(|n| n.to_str()) == Some(q))
}

fn plan_status_detail(plans: &[Plan], query: &str) -> Result<Value, String> {
    let Some(plan) = find_plan(plans, query) else {
        let ids: Vec<&str> = plans.iter().map(|p| p.id.as_str()).collect();
        return Err(format!(
            "plan '{query}' no encontrado. Planes disponibles: {}",
            ids.join(", ")
        ));
    };
    let Analysis {
        ready,
        in_progress,
        blocked,
        missing,
        cycles,
    } = plans::analyze(plan);
    let by_id: HashMap<&str, &devctx_core::plans::Task> =
        plan.tasks.iter().map(|t| (t.id.as_str(), t)).collect();

    let title_of = |id: &str| by_id.get(id).map(|t| t.title.clone()).unwrap_or_default();

    let ready: Vec<Value> = ready
        .iter()
        .map(|id| json!({ "id": id, "title": title_of(id) }))
        .collect();
    let in_progress: Vec<Value> = in_progress
        .iter()
        .map(|id| json!({ "id": id, "title": title_of(id) }))
        .collect();
    let blocked: Vec<Value> = blocked
        .iter()
        .map(
            |(id, waiting_on)| json!({ "id": id, "title": title_of(id), "waiting_on": waiting_on }),
        )
        .collect();

    let mut warnings: Vec<String> = plan.warnings.clone();
    for (task, dep) in &missing {
        warnings.push(format!(
            "{task}: depende de {dep}, que no existe en el plan"
        ));
    }
    for cycle in &cycles {
        warnings.push(format!("ciclo de dependencias: {}", cycle.join(" -> ")));
    }

    let done = plan.tasks.iter().filter(|t| t.status.is_done()).count();
    let skipped = plan
        .tasks
        .iter()
        .filter(|t| matches!(t.status, plans::Status::Skipped))
        .count();
    // Cross-plan dependencies are only reported: they never block and are never "missing".
    let external_deps: Vec<Value> = plan
        .tasks
        .iter()
        .filter(|t| !t.external_deps.is_empty())
        .map(|t| json!({ "id": t.id, "deps": t.external_deps }))
        .collect();
    let mut detail = json!({
        "plan": plan.id,
        "title": plan.title,
        "done": done,
        "skipped": skipped,
        "total": plan.tasks.len(),
        "ready": ready,
        "in_progress": in_progress,
        "blocked": blocked,
        "warnings": warnings,
    });
    if !external_deps.is_empty() {
        detail["external_deps"] = Value::Array(external_deps);
    }
    Ok(detail)
}

/// Fits the `plan_status` detail (or list) JSON into the output budget by dropping, in order,
/// `warnings` then `blocked` — never `ready` — and recording what was omitted.
fn fit_plan_status_budget(mut value: Value, budget_tokens: usize) -> String {
    let mut out = value.to_string();
    if budget_tokens == 0 {
        return out;
    }
    let budget_chars = budget_tokens * CHARS_PER_TOKEN;
    if out.len() <= budget_chars {
        return out;
    }
    let mut omitted_warnings = 0usize;
    let mut omitted_blocked = 0usize;
    if let Some(obj) = value.as_object_mut() {
        if let Some(Value::Array(warnings)) = obj.get_mut("warnings") {
            omitted_warnings = warnings.len();
            warnings.clear();
        }
    }
    out = value.to_string();
    if out.len() > budget_chars {
        if let Some(obj) = value.as_object_mut() {
            if let Some(Value::Array(blocked)) = obj.get_mut("blocked") {
                omitted_blocked = blocked.len();
                blocked.clear();
            }
        }
    }
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "omitted".to_string(),
            json!({ "blocked": omitted_blocked, "warnings": omitted_warnings }),
        );
    }
    value.to_string()
}

/// The dashboard's "Plans" tab: one plan's dependency graph as cytoscape `{nodes, edges}`, plus
/// `plans` (every plan id, for the tab's selector). No `plan` means the active plan (same rule
/// as `do_plan_status`), falling back to the first plan by id when nothing is active (e.g. every
/// plan is done) so the tab is never empty just because there is nothing left to do.
pub fn do_plan_graph(state: &AppState, plan: Option<&str>) -> Result<String, String> {
    plan_graph_value(&state.plans_root.root, plan)
}

/// The unbudgeted `plan_status`-graph JSON, built straight from disk (see [`plan_status_value`]
/// for the same split and why: it keeps this testable without an `AppState`).
pub fn plan_graph_value(root: &std::path::Path, plan: Option<&str>) -> Result<String, String> {
    let loaded = plans::load_plans(root);
    let plan_ids: Vec<String> = loaded.iter().map(|p| p.id.clone()).collect();

    let target_id = match plan {
        Some(query) => find_plan(&loaded, query)
            .map(|p| p.id.clone())
            .ok_or_else(|| {
                format!(
                    "plan '{query}' no encontrado. Planes disponibles: {}",
                    plan_ids.join(", ")
                )
            })?,
        None => active_plan_id(&loaded)
            .or_else(|| loaded.first().map(|p| p.id.clone()))
            .ok_or_else(|| "no hay planes en plans/".to_string())?,
    };
    let p = loaded
        .iter()
        .find(|p| p.id == target_id)
        .ok_or_else(|| format!("plan '{target_id}' no encontrado"))?;

    if p.tasks.is_empty() {
        return Ok(json!({
            "plan": p.id,
            "title": p.title,
            "nodes": [],
            "edges": [],
            "plans": plan_ids,
            "message": "sin tasks",
        })
        .to_string());
    }

    let Analysis {
        ready,
        in_progress,
        missing,
        ..
    } = plans::analyze(p);
    let ready_set: HashSet<&str> = ready.iter().map(|s| s.as_str()).collect();
    let in_progress_set: HashSet<&str> = in_progress.iter().map(|s| s.as_str()).collect();
    let missing_ids: HashSet<&str> = missing.iter().map(|(_, dep)| dep.as_str()).collect();

    let mut nodes: Vec<Value> = Vec::new();
    for t in &p.tasks {
        let status = if t.status.is_done() {
            "done"
        } else if matches!(t.status, plans::Status::Skipped) {
            "skipped"
        } else if in_progress_set.contains(t.id.as_str()) {
            "in_progress"
        } else if matches!(t.status, plans::Status::Blocked) {
            "blocked"
        } else if ready_set.contains(t.id.as_str()) {
            "ready"
        } else {
            "blocked"
        };
        nodes.push(json!({
            "data": {
                "id": t.id,
                "label": format!("{} {}", t.id, t.title),
                "status": status,
                "ready": ready_set.contains(t.id.as_str()),
                "depends_on": t.depends_on,
                "external_deps": t.external_deps,
                "files": t.files.iter().map(|f| f.path.clone()).collect::<Vec<_>>(),
            }
        }));
    }
    for missing_id in &missing_ids {
        nodes.push(json!({
            "data": { "id": missing_id, "label": missing_id, "status": "missing", "ready": false }
        }));
    }

    let mut edges: Vec<Value> = Vec::new();
    for t in &p.tasks {
        for dep in &t.depends_on {
            edges.push(json!({
                "data": { "id": format!("{dep}->{}", t.id), "source": dep, "target": t.id }
            }));
        }
    }

    Ok(json!({
        "plan": p.id,
        "title": p.title,
        "nodes": nodes,
        "edges": edges,
        "plans": plan_ids,
    })
    .to_string())
}

/// `remember` tool: save a memory (deduplicated).
pub fn do_remember(
    state: &AppState,
    content: String,
    title: String,
    memory_type: String,
    topic: String,
    tags: String,
    files: String,
) -> Result<String, String> {
    let store = state.open_store()?;
    let (repo, branch) = state.repo_branch().unwrap_or_default();
    let req = RememberRequest {
        title,
        content,
        memory_type,
        project: state.project(),
        topic_key: topic,
        tags,
        files,
        repo,
        branch,
        now: now_epoch(),
        ..Default::default()
    };
    let embedder = state.embedder()?;
    let res = remember(&store, embedder.as_ref(), &req).map_err(|e| e.to_string())?;
    // Link after saving, never before: the memory is the thing worth keeping,
    // and a repository with no graph yet must not turn a save into a failure.
    let links = devctx_memory::links::link_memory(&store, &res.memory);
    Ok(json!({
        "status": format!("{:?}", res.status).to_lowercase(),
        "id": res.memory.id,
        "title": res.memory.title,
        "linked_symbols": links,
    })
    .to_string())
}

/// `recall` tool: recall memories relevant to a query.
pub fn do_recall(state: &AppState, query: &str, limit: usize) -> Result<String, String> {
    let store = state.open_store()?;
    let project = state.project();
    let embedder = state.embedder()?;
    let hits = recall(
        &store,
        embedder.as_ref(),
        &RecallQuery {
            query,
            project: Some(&project),
            repo: None,
            limit,
        },
    )
    .map_err(|e| e.to_string())?;
    let arr: Vec<Value> = hits
        .iter()
        .map(|h| {
            json!({
                "score": h.score,
                "id": h.memory.id,
                "title": h.memory.title,
                "type": h.memory.memory_type,
                "tags": h.memory.tags,
                "content": h.memory.content,
            })
        })
        .collect();
    serde_json::to_string_pretty(&Value::Array(arr)).map_err(|e| e.to_string())
}

/// `graph` tool: cytoscape-shaped `{nodes, edges}` for the call-graph.
///
/// A node is *external* when it is called but never defined locally (never a
/// source). `hide_external`/`hide_synthetic` drop those before assembly.
pub fn do_graph(
    state: &AppState,
    kind: Option<String>,
    file: Option<String>,
    limit: usize,
    hide_external: bool,
    hide_synthetic: bool,
) -> Result<String, String> {
    let store = state.open_store()?;
    let chosen = pick_branch(state, &store)?;
    let (repo, branch) = (chosen.repo.clone(), chosen.branch.clone());
    let edges = store
        .graph_edges(&repo, &branch, kind.as_deref(), file.as_deref(), limit)
        .map_err(|e| e.to_string())?;

    // "Defined locally" = any symbol that makes a call anywhere in the repo.
    // A call target that is never itself a caller is treated as external
    // (a library/undefined symbol). Computed over the whole graph, not the
    // limited display window, so internal→internal edges survive the limit.
    let defined = store
        .graph_defined_symbols(&repo, &branch)
        .map_err(|e| e.to_string())?;
    let external = |id: &str| !defined.contains(id);

    let mut node_ids: Vec<String> = Vec::new();
    let mut node_file: HashMap<String, String> = HashMap::new();
    let mut node_ext: HashMap<String, bool> = HashMap::new();
    let mut out_edges: Vec<Value> = Vec::new();

    for (i, e) in edges.iter().enumerate() {
        if hide_external && external(&e.target) {
            continue;
        }
        if hide_synthetic && (is_synthetic(&e.source) || is_synthetic(&e.target)) {
            continue;
        }
        // Register both endpoints; keep the first file seen for a node.
        for (id, file) in [(&e.source, e.source_file.as_str()), (&e.target, "")] {
            if !node_file.contains_key(id) {
                node_ids.push(id.clone());
                node_file.insert(id.clone(), file.to_string());
                node_ext.insert(id.clone(), external(id));
            } else if node_file[id].is_empty() && !file.is_empty() {
                node_file.insert(id.clone(), file.to_string());
            }
        }
        out_edges.push(json!({
            "data": {
                "id": format!("e{i}"),
                "source": e.source,
                "target": e.target,
                "kind": e.kind,
                "file": e.source_file,
                "line": e.line,
            }
        }));
    }

    let nodes: Vec<Value> = node_ids
        .iter()
        .map(|id| {
            json!({
                "data": {
                    "id": id,
                    "label": short_label(id),
                    "file": node_file.get(id).cloned().unwrap_or_default(),
                    "repo": repo,
                    "external": node_ext.get(id).copied().unwrap_or(false),
                }
            })
        })
        .collect();

    let mut out = json!({
        "repo": repo,
        "branch": branch,
        "nodes": nodes,
        "edges": out_edges,
    });
    chosen.annotate(&mut out);
    Ok(out.to_string())
}

/// Tell the registry what an indexing run produced, so `projects list` reflects
/// reality instead of always reading "never indexed".
///
/// Best-effort: a repository need not be registered at all, and a central store
/// that cannot be reached is no reason to fail an index that already succeeded.
pub fn report_index(store: &Store, root: &std::path::Path, res: &devctx_index::IndexResult) {
    // The registry is keyed by the canonical project path...
    let Ok(path) = std::fs::canonicalize(root) else {
        return;
    };
    let repo_path = path.to_string_lossy().into_owned();
    // ...but the store files the index under git's toplevel, which differs in a
    // monorepo subdirectory (and from `\\?\` paths on Windows).
    let (files, symbols, chunks) = totals_for_root(store, root, &res.branch);
    if let Ok(c) = central() {
        if c.record_index(&repo_path, &res.commit, &res.branch, files, symbols, chunks)
            .is_ok()
        {
            return;
        }
    }
    // No daemon to route through — `DEVCTX_NO_AUTOSERVE`, or one that refused to
    // start. Opening the central store here is safe precisely because nothing
    // else holds it, and the alternative is worse than it sounds: the registry
    // keeps reporting "never indexed" for a fully indexed project, which reads
    // as a broken index rather than an unsent report.
    let stats = devctx_store::ProjectIndexStats {
        commit: res.commit.clone(),
        branch: res.branch.clone(),
        files,
        symbols,
        chunks,
    };
    match devctx_central::Central::open() {
        Ok(central) => {
            if let Err(e) = central.record_index(&repo_path, &stats, &devctx_central::now_stamp()) {
                eprintln!("warning: could not record the index in the central registry: {e}");
            }
        }
        Err(e) => eprintln!("warning: could not record the index in the central registry: {e}"),
    }
}

/// Totals, not this run's deltas: an incremental run that found nothing
/// changed would otherwise report the project as empty. Read under the key the
/// store uses (the git toplevel), never the registry's canonical path.
fn totals_for_root(store: &Store, root: &std::path::Path, branch: &str) -> (i64, i64, i64) {
    store
        .index_totals(&repo_key(root), branch)
        .unwrap_or((0, 0, 0))
}

/// Reach the central store, auto-spawning the daemon if needed.
///
/// The project server must never open the central database itself — DuckDB
/// allows one writing process per file, and that process is the daemon.
fn central() -> Result<devctx_central::CentralClient, String> {
    let paths = devctx_central::CentralPaths::resolve().map_err(|e| e.to_string())?;
    devctx_central::client::ensure(&paths).ok_or_else(|| {
        let base =
            "no central store daemon and one could not be started; run `devctx serve --central`";
        match devctx_central::client::spawn_failure_hint(&paths) {
            Some(why) => format!("{base}\n\nThe daemon's log ends with: {why}"),
            None => base.to_string(),
        }
    })
}

/// The root directory of a registered project, given its name or its path.
///
/// Accepting either is deliberate: an agent that has just read `list_projects`
/// has the name, and one repeating a path a human typed has the path. A bare
/// name is looked up in the registry and never treated as a directory, because
/// interpreting it as one would resolve it against this process's working
/// directory — which for a globally-registered server is an accident of how the
/// client was launched, and could bind a same-named directory that happens to
/// sit there. Whatever comes back is absolute.
pub fn resolve_project_root(name_or_path: &str) -> Result<PathBuf, String> {
    let expanded = shellexpand(name_or_path);
    let as_path = PathBuf::from(&expanded);
    let looks_like_path =
        as_path.is_absolute() || expanded.contains(std::path::MAIN_SEPARATOR) || expanded == ".";
    if looks_like_path {
        return absolute(&as_path);
    }
    let row = central()?
        .show(name_or_path)
        .map_err(|e| format!("{name_or_path}: {e}"))?;
    let path = row
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("no path recorded for `{name_or_path}`"))?;
    absolute(std::path::Path::new(path))
}

/// Resolve a project directory, insisting it holds a config and an absolute path.
fn absolute(path: &std::path::Path) -> Result<PathBuf, String> {
    let root = path
        .canonicalize()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if !root.join(devctx_core::CONFIG_FILE_NAME).exists() {
        return Err(format!(
            "{} is not a DevCtxEngine project (no {})",
            root.display(),
            devctx_core::CONFIG_FILE_NAME
        ));
    }
    Ok(root)
}

/// Expand a leading `~` — paths reach us as text an agent copied from a table.
fn shellexpand(path: &str) -> String {
    match path.strip_prefix("~/") {
        Some(rest) => match std::env::var_os("HOME") {
            Some(home) => format!("{}/{rest}", home.to_string_lossy()),
            None => path.to_string(),
        },
        None => path.to_string(),
    }
}

/// A registered project, as the descent needs it: enough to bind, name and rank.
#[derive(Clone, Debug)]
pub struct ProjectRow {
    pub name: String,
    pub path: PathBuf,
    pub group: Option<String>,
    /// Embedding width. Compared before fusing rankings across projects:
    /// vectors of different dimension are not comparable.
    pub embed_dim: i64,
    /// Unix seconds of the last index run, `0` when never indexed. Used only to
    /// pick a group's default member — the most recently indexed repository is
    /// the one most likely being worked on.
    pub last_indexed_at: i64,
}

/// What the registry can say about the directory the server was started in.
#[derive(Clone, Debug)]
pub enum Resolution {
    /// Exactly one registered project lives under this directory.
    Single(ProjectRow),
    /// Several do, and every one of them declares the same non-empty group.
    Group {
        name: String,
        members: Vec<ProjectRow>,
    },
    /// Several do, but they do not agree on a group — binding one would be a
    /// guess, so the caller stays unbound and shows these as the candidates.
    Ambiguous(Vec<ProjectRow>),
    /// None do.
    Empty,
    /// The registry could not be read, so the question was never answered.
    ///
    /// Distinct from `Empty` on purpose: "there is nothing here" and "I could
    /// not look" lead to different fixes, and collapsing them sends the user
    /// hunting for a missing project when the daemon is what is missing.
    RegistryUnavailable(String),
}

/// Is `candidate` strictly below `base`?
///
/// Compared component by component rather than with `starts_with` on the
/// rendered string, because `/a/revfa` is a string prefix of `/a/revfa-otro`
/// while being no ancestor of it at all — and binding a sibling workspace by
/// accident is exactly the failure this descent exists to avoid. Equality is
/// not descent: a directory that *is* a project is already handled by the walk
/// upwards, and letting it match here would bind it twice by two rules.
fn is_strict_descendant(candidate: &std::path::Path, base: &std::path::Path) -> bool {
    candidate != base && candidate.starts_with(base)
}

/// The group declared by a project, read from its own config.
///
/// Read from the file rather than the registry row because group membership
/// lives in `project.group` of the repository's config and the registry does
/// not carry it. Reading N small YAML files once at startup is cheaper than
/// widening the registry schema for a value that is authored per repository.
fn group_of(root: &std::path::Path) -> Option<String> {
    let cfg = ProjectConfig::load(&root.join(devctx_core::CONFIG_FILE_NAME)).ok()?;
    let group = cfg.project.group.trim().to_string();
    (!group.is_empty()).then_some(group)
}

/// Every registered project that lives strictly under `base`.
///
/// This is the query the server never asked. A globally-registered MCP server
/// is launched from whatever directory the client happened to be in, and when
/// that is the root of a multi-repository workspace the walk upwards finds
/// nothing — while the registry has known where every one of those projects is
/// all along.
pub fn projects_under(base: &std::path::Path) -> Result<Vec<ProjectRow>, String> {
    let base = base
        .canonicalize()
        .map_err(|e| format!("{}: {e}", base.display()))?;
    let rows = central().and_then(|c| c.list(false).map_err(|e| e.to_string()))?;
    let mut found: Vec<ProjectRow> = rows
        .iter()
        .filter_map(|r| {
            let name = r.get("name")?.as_str()?.to_string();
            let raw = r.get("path")?.as_str()?;
            let path = std::path::Path::new(raw).canonicalize().ok()?;
            if !is_strict_descendant(&path, &base) {
                return None;
            }
            let last_indexed_at = r
                .get("last_indexed_at")
                .and_then(|v| {
                    v.as_str()
                        .and_then(|s| s.parse().ok())
                        .or_else(|| v.as_i64())
                })
                .unwrap_or(0);
            let group = group_of(&path);
            let embed_dim = r.get("embed_dim").and_then(|v| v.as_i64()).unwrap_or(0);
            Some(ProjectRow {
                name,
                path,
                group,
                embed_dim,
                last_indexed_at,
            })
        })
        .collect();

    // Nested registrations: when one candidate contains another, the outer one
    // is a workspace that happens to be registered too. Keep the shallowest set
    // — binding the outer one is what the user pointed at, and its children stay
    // reachable through the hint and `use_project`.
    let paths: Vec<PathBuf> = found.iter().map(|r| r.path.clone()).collect();
    found.retain(|r| {
        !paths
            .iter()
            .any(|other| is_strict_descendant(&r.path, other))
    });
    found.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(found)
}

/// The registered project a path belongs to, walking upwards from it.
///
/// This is what makes a session usable across a workspace. The MCP process
/// takes its working directory once, at spawn, and never learns that the agent
/// has moved into a different repository — so a binding fixed at startup is
/// fixed for the life of the process. A path carried by the call itself is the
/// only signal that tracks where the work actually is.
///
/// Resolved against the registry rather than by looking for a `.devctx`
/// directory: an unregistered repository is not something this server can open,
/// and silently walking past it to a registered ancestor would answer from the
/// wrong project.
pub fn resolve_hint(hint: &str) -> Option<ProjectRow> {
    // A bare name is a registry lookup, never a directory: resolving it against
    // this process's working directory would bind whatever same-named folder
    // happens to sit there, and that directory is an accident of how the client
    // was launched.
    let looks_like_path =
        hint.contains(std::path::MAIN_SEPARATOR) || hint.starts_with('~') || hint == ".";
    if !looks_like_path {
        let rows = central()
            .and_then(|c| c.list(false).map_err(|e| e.to_string()))
            .ok()?;
        let row = rows
            .iter()
            .find(|r| r.get("name").and_then(|v| v.as_str()) == Some(hint))?;
        let path = std::path::Path::new(row.get("path")?.as_str()?)
            .canonicalize()
            .ok()?;
        return Some(ProjectRow {
            name: hint.to_string(),
            path,
            group: None,
            embed_dim: 0,
            last_indexed_at: 0,
        });
    }
    let expanded = shellexpand(hint);
    let start = std::path::Path::new(&expanded).canonicalize().ok()?;
    let rows = central()
        .and_then(|c| c.list(false).map_err(|e| e.to_string()))
        .ok()?;
    let registered: Vec<ProjectRow> = rows
        .iter()
        .filter_map(|r| {
            let name = r.get("name")?.as_str()?.to_string();
            let path = std::path::Path::new(r.get("path")?.as_str()?)
                .canonicalize()
                .ok()?;
            Some(ProjectRow {
                name,
                path,
                group: None,
                embed_dim: 0,
                last_indexed_at: 0,
            })
        })
        .collect();

    // Deepest match wins: with nested registrations the innermost project is
    // the one that actually owns the file.
    let mut here: Option<&std::path::Path> = Some(start.as_path());
    while let Some(dir) = here {
        if let Some(row) = registered.iter().find(|r| r.path == dir) {
            return Some(row.clone());
        }
        here = dir.parent();
    }
    None
}

/// Classify what lives under `base` into something the caller can bind.
pub fn resolve_under(base: &std::path::Path) -> Resolution {
    let found = match projects_under(base) {
        Ok(f) => f,
        Err(e) => return Resolution::RegistryUnavailable(e),
    };
    match found.len() {
        0 => Resolution::Empty,
        1 => Resolution::Single(found.into_iter().next().expect("len checked")),
        _ => {
            // A group binding is only honest when every member agrees. One
            // repository with a different group — or none at all — makes the
            // product ambiguous, and guessing which product the user meant is
            // worse than saying so.
            let first = found[0].group.clone();
            match first {
                Some(name)
                    if found
                        .iter()
                        .all(|r| r.group.as_deref() == Some(name.as_str())) =>
                {
                    Resolution::Group {
                        name,
                        members: found,
                    }
                }
                _ => Resolution::Ambiguous(found),
            }
        }
    }
}

/// What to tell an agent whose server was started outside any project.
///
/// This has to be a tool result, never a startup failure: a server that refuses
/// to start reports itself to the client as a bare transport error, and a bare
/// transport error is the one kind nobody can act on.
pub fn unbound_help(cwd: &std::path::Path) -> String {
    format!(
        "{}\n\nCall `use_project` with one of those names to bind this session, or \
         restart the server as `devctx mcp --project <path>`.",
        why_unbound(cwd, &resolve_under(cwd))
    )
}

/// How many candidates to spell out before summarising the rest.
const MAX_LISTED: usize = 15;

fn render(rows: &[ProjectRow], with_group: bool) -> String {
    let mut lines: Vec<String> = rows
        .iter()
        .take(MAX_LISTED)
        .map(|r| {
            let suffix = if with_group {
                match &r.group {
                    Some(g) => format!("  [group: {g}]"),
                    None => "  [no group]".to_string(),
                }
            } else {
                String::new()
            };
            format!("  · {} — {}{}", r.name, r.path.display(), suffix)
        })
        .collect();
    if rows.len() > MAX_LISTED {
        lines.push(format!("  … and {} more", rows.len() - MAX_LISTED));
    }
    lines.join("\n")
}

/// Why this directory produced no binding, in the terms that let a caller fix it.
///
/// Two very different situations used to render identically: a workspace whose
/// repositories disagree about their group, and a directory with no registered
/// project anywhere near it. The first needs the candidates and their groups —
/// listing the whole machine buries them. The second needs the whole registry,
/// because nothing local is relevant.
pub fn why_unbound(cwd: &std::path::Path, resolution: &Resolution) -> String {
    match resolution {
        Resolution::Ambiguous(rows) => format!(
            "Found {} DevCtxEngine projects under {}, but they do not share a group, \
             so binding one of them would be a guess.\n\nCandidates:\n{}\n\n\
             Give them the same `project.group` in their config to have this \
             directory bind to the group automatically.",
            rows.len(),
            cwd.display(),
            render(rows, true)
        ),
        // Single and Group never reach here in practice — the caller binds them —
        // but a tool asking after the fact deserves a straight answer.
        Resolution::Single(row) => format!(
            "One project ({}) lives under {} and should have been bound.",
            row.name,
            cwd.display()
        ),
        Resolution::Group { name, members } => format!(
            "Group {} ({} projects) lives under {} and should have been bound.",
            name,
            members.len(),
            cwd.display()
        ),
        Resolution::RegistryUnavailable(e) => format!(
            "The project registry could not be read, so this server could not work out \
             what lives under {}: {e}\n\nThis is not \"no project here\" — nothing was \
             checked. Start the central store (`devctx serve --central`) and reconnect, or \
             name a project explicitly with `devctx mcp --project <path>`.",
            cwd.display()
        ),
        Resolution::Empty => {
            let all = match central().and_then(|c| c.list(false).map_err(|e| e.to_string())) {
                Ok(rows) if !rows.is_empty() => {
                    let mapped: Vec<ProjectRow> = rows
                        .iter()
                        .filter_map(|r| {
                            Some(ProjectRow {
                                name: r.get("name")?.as_str()?.to_string(),
                                path: PathBuf::from(r.get("path")?.as_str()?),
                                group: None,
                                embed_dim: 0,
                                last_indexed_at: 0,
                            })
                        })
                        .collect();
                    render(&mapped, false)
                }
                Ok(_) => "  (none registered yet — run `devctx init` in a repository)".to_string(),
                Err(e) => format!("  (the registry could not be read: {e})"),
            };
            format!(
                "This MCP server is not bound to a project: it was started in {}, which is \
                 neither inside a DevCtxEngine repository nor above one.\n\n\
                 Registered projects:\n{all}",
                cwd.display()
            )
        }
    }
}

/// Run `devctx <args>` inside a member's repository and return its output.
///
/// Every fan-out re-enters this same binary with the member's directory as the
/// working directory, so the child resolves that project and goes through its
/// own server (`ensure_cli`): the MCP never opens a DuckDB itself. (When
/// auto-spawn is disabled the child opens the store directly after a lock
/// check; a brief local open is safe because this process holds no `Store`.)
///
/// The binary is [`devctx_core::self_exe`], not `current_exe()`: after a
/// reinstall the latter ends in " (deleted)" and spawning it fails with ENOENT,
/// which used to surface as a bare "No such file or directory". Every failure
/// here names the member and its path, and says which binary was run.
fn run_in_member<I, S>(
    member: &str,
    path: &std::path::Path,
    args: I,
) -> Result<std::process::Output, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let at = format!("{member} ({})", path.display());
    if !path.is_dir() {
        return Err(format!("{at}: the repository path does not exist"));
    }
    let exe = devctx_core::self_exe()
        .map_err(|e| format!("{at}: could not locate the devctx binary to run: {e}"))?;
    let mut cmd = std::process::Command::new(&exe);
    devctx_core::clean_git_env(&mut cmd);
    cmd.args(args)
        .current_dir(path)
        .output()
        .map_err(|e| format!("{at}: could not run {}: {e}", exe.display()))
}

/// The last line a failed child wrote, prefixed with who it was.
fn child_failure(
    member: &str,
    path: &std::path::Path,
    out: &std::process::Output,
    fallback: &str,
) -> String {
    let err = String::from_utf8_lossy(&out.stderr);
    format!(
        "{member} ({}): {}",
        path.display(),
        err.trim().lines().last().unwrap_or(fallback)
    )
}

/// Members whose repository directory is gone, split from the rest.
///
/// A registry row outliving its checkout is stale data, not an unreachable
/// repository: it is reported on its own (`skipped_missing`) and does not count
/// against "none of the members could be reached".
fn split_missing(members: &[ProjectRow]) -> (Vec<&ProjectRow>, Vec<Value>) {
    let mut present = Vec::new();
    let mut missing = Vec::new();
    for m in members {
        if m.path.is_dir() {
            present.push(m);
        } else {
            missing.push(json!({ "project": m.name, "path": m.path.display().to_string() }));
        }
    }
    (present, missing)
}

/// Run one member's search, returning its raw hit list.
///
/// Separate from `do_search_project` because the group path needs the hits
/// themselves to fuse, not the single-project envelope that wraps them.
fn search_one(
    member: &str,
    path: &std::path::Path,
    query: &str,
    limit: usize,
    language: Option<&str>,
    mode: &str,
) -> Result<devctx_core::SearchHits, String> {
    // Not opening the store directly: DuckDB allows one writing process per
    // file, and a running `devctx serve` for that project owns it. Re-entering
    // our own binary with its working directory set is what the single-project
    // path already does, and it routes through that server when one is up.
    let mut args: Vec<String> = vec![
        "search".into(),
        query.into(),
        "--limit".into(),
        limit.to_string(),
        "--format".into(),
        "json".into(),
    ];
    if let Some(l) = language {
        args.extend(["--language".into(), l.into()]);
    }
    match mode {
        "keyword" => args.push("--keyword".into()),
        "hybrid" => args.push("--hybrid".into()),
        _ => {}
    }
    let out = run_in_member(member, path, &args)?;
    if !out.status.success() {
        return Err(child_failure(member, path, &out, "search failed"));
    }
    // The answer is a bare array or `{results, branch_fallback, ...}`; the
    // helper reads both and never turns the wrapper into a hit.
    let v = serde_json::from_slice::<Value>(&out.stdout).map_err(|e| e.to_string())?;
    Ok(devctx_core::search_hits(&v))
}

/// Search every member of a group and return one fused ranking.
///
/// A session bound to a group is attached to the product, so "search" means the
/// product. Answering from a single member would answer a question nobody asked
/// — and would do it invisibly, which is worse than answering nothing.
///
/// Fused by reciprocal rank rather than by raw score: two stores embed and score
/// independently, so their scores are not on one scale even when the model is
/// identical. Rank survives that; magnitude does not.
/// How many members to search at once.
///
/// Measured, not guessed: a warm project daemon answers in ~300ms and holds
/// ~100MB, a cold one takes 3-6s while it starts. Four keeps the cold case
/// bounded without holding eleven processes open at their peak.
const FANOUT_CONCURRENCY: usize = 4;

/// Search every member of a group and return one fused ranking.
///
/// A session bound to a group is attached to the product, so "search" means the
/// product. Answering from a single member would answer a question nobody asked
/// — and would do it invisibly, which is worse than answering nothing.
pub fn do_search_group(
    members: &[ProjectRow],
    query: &str,
    limit: usize,
    language: Option<String>,
    mode: &str,
    only: Option<&[String]>,
) -> Result<String, String> {
    // Vectors of different width are not comparable, and a ranking fused across
    // them looks exactly as plausible as a correct one. The registry has carried
    // `embed_dim` for this comparison all along.
    let dims: Vec<i64> = members.iter().map(|m| m.embed_dim).collect();
    let majority = dims
        .iter()
        .copied()
        .max_by_key(|d| dims.iter().filter(|x| *x == d).count())
        .unwrap_or(0);

    let (present, skipped_missing) = split_missing(members);
    let reachable = present.len();
    let mut skipped = Vec::new();
    let mut targets: Vec<&ProjectRow> = Vec::new();
    for m in present {
        if let Some(only) = only {
            if !only.iter().any(|n| n == &m.name) {
                continue;
            }
        }
        if m.embed_dim != majority {
            skipped.push(json!({
                "project": m.name,
                "reason": format!(
                    "embedding dimension {} does not match the group's {majority}",
                    m.embed_dim
                ),
            }));
            continue;
        }
        targets.push(m);
    }

    // Run in bounded batches. Sequentially this is eleven round trips one after
    // another; the members are independent, so that latency is pure waste.
    let mut results: Vec<(String, Result<devctx_core::SearchHits, String>)> = Vec::new();
    for batch in targets.chunks(FANOUT_CONCURRENCY) {
        std::thread::scope(|scope| {
            let handles: Vec<_> = batch
                .iter()
                .map(|m| {
                    let lang = language.clone();
                    scope.spawn(move || {
                        (
                            m.name.clone(),
                            search_one(&m.name, &m.path, query, limit, lang.as_deref(), mode),
                        )
                    })
                })
                .collect();
            for h in handles {
                match h.join() {
                    Ok(r) => results.push(r),
                    Err(_) => results.push((
                        "<panicked>".to_string(),
                        Err("the search thread panicked".to_string()),
                    )),
                }
            }
        });
    }

    let mut failed = Vec::new();
    let mut per_member: Vec<(String, Vec<Value>)> = Vec::new();
    // What each member said about its own answer (branch fallback, stale
    // index): a member answered from another branch is still an answer, but the
    // reader has to be told which one it was.
    let mut member_notes = serde_json::Map::new();
    for (name, r) in results {
        match r {
            Ok(answer) => {
                if let Some(notes) = answer.notes_json() {
                    member_notes.insert(name.clone(), notes);
                }
                per_member.push((name, answer.hits));
            }
            // One member being down is not the search failing: the rest still
            // have an answer, and saying which one went missing beats refusing.
            Err(e) => failed.push(json!({ "project": name, "error": e })),
        }
    }

    // How to merge depends on what the scores mean, and they do not all mean the
    // same thing:
    //
    // * `vector` — cosine similarity against the same model, in the same number
    //   of dimensions (the check above guarantees it). That is an absolute
    //   quantity, so the scores from two stores are directly comparable.
    // * `keyword` — BM25, whose IDF and length normalisation are computed from
    //   each repository's own corpus. A small repository inflates them. Across
    //   stores these numbers are not on one scale and comparing them is noise.
    // * `hybrid` — already an internal fusion per member; same problem.
    //
    // Rank fusion is the safe answer for the latter two. It is the *wrong*
    // answer for the first, because these result sets are disjoint by
    // construction — no document appears in two repositories — so reciprocal
    // rank never accumulates and degenerates into round-robin: one hit per
    // member, ordered by nothing.
    let by_score = mode == "vector" || mode.is_empty();
    const K: f64 = 60.0;
    let mut fused: Vec<(f64, Value)> = Vec::new();
    for (project, hits) in per_member {
        for (rank, hit) in hits.into_iter().enumerate() {
            let key = if by_score {
                hit.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0)
            } else {
                1.0 / (K + (rank + 1) as f64)
            };
            let mut hit = hit;
            if let Value::Object(map) = &mut hit {
                // Without this the result is unusable: two repositories can hold
                // the same relative path, and the reader cannot tell them apart.
                map.insert("project".into(), json!(project));
            }
            fused.push((key, hit));
        }
    }
    fused.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let total = fused.len();
    let hits: Vec<Value> = fused.into_iter().take(limit).map(|(_, h)| h).collect();

    let mut out = json!({
        "hits": hits,
        "searched_group": true,
        "fused_by": if by_score { "score" } else { "reciprocal_rank" },
    });
    if total > limit {
        out["omitted_for_budget"] = json!({ "count": total - limit });
    }
    if !skipped.is_empty() {
        out["skipped_projects"] = json!(skipped);
    }
    if !failed.is_empty() {
        out["failed_projects"] = json!(failed);
    }
    if !skipped_missing.is_empty() {
        out["skipped_missing"] = json!(skipped_missing);
    }
    if !member_notes.is_empty() {
        out["member_notes"] = Value::Object(member_notes);
    }
    if let Some(warning) = fan_out_warning(&failed, reachable)?
        .or_else(|| all_missing_warning(reachable, skipped_missing.len(), "searched"))
    {
        out["warning"] = json!(warning);
    }
    serde_json::to_string_pretty(&out).map_err(|e| e.to_string())
}

/// The fan-out had nobody to ask: every member's checkout is missing.
///
/// [`fan_out_warning`] only speaks about members that were tried and failed,
/// and missing ones are deliberately not failures — so with *all* of them
/// missing it stayed silent, and the answer (the shared tier alone, or
/// nothing) read like a complete one. `None` when at least one member was
/// present or none were missing.
fn all_missing_warning(present: usize, missing: usize, verb: &str) -> Option<String> {
    (present == 0 && missing > 0).then(|| {
        format!(
            "No repository of this group could be {verb}: all {missing} registered member(s) \
             point at paths that no longer exist (see `skipped_missing`), so this answer \
             contains nothing from the members' own stores. Re-register the moved checkouts \
             (`devctx projects add <path>`) or remove the stale rows."
        )
    })
}

/// How a fan-out reports members it could not reach.
///
/// `failed_projects` alone is not enough. It is a field inside a JSON blob that
/// gets skimmed, and "nothing recorded about that" is a perfectly plausible
/// answer — so a recall that reached nobody looked exactly like a recall that
/// found nothing. That is how the group fan-out stayed broken for months: every
/// member failed on a flag the subcommand did not have, and the only trace was
/// a key nobody read.
///
/// So: if EVERY member failed, this is not a thin result, it is a query that did
/// not run, and it fails loudly. If some failed, the answer stands but says so
/// in prose that cannot be skimmed past.
fn fan_out_warning(failed: &[Value], members: usize) -> Result<Option<String>, String> {
    if failed.is_empty() {
        return Ok(None);
    }
    // Each error already names its member and path; list up to three so one
    // broken checkout does not hide that the others fail differently.
    let causes: Vec<&str> = failed
        .iter()
        .filter_map(|f| f.get("error").and_then(|e| e.as_str()))
        .take(3)
        .collect();
    let first_error = if causes.is_empty() {
        "no reason given".to_string()
    } else {
        let more = failed.len().saturating_sub(causes.len());
        let mut t = causes.join(" | ");
        if more > 0 {
            t.push_str(&format!(" (and {more} more)"));
        }
        t
    };
    if members > 0 && failed.len() >= members {
        return Err(format!(
            "None of the {members} repositories in this group could be reached, so this \
             answer would come from the shared tier alone — which is not an answer to \
             the question asked. Failures: {first_error}"
        ));
    }
    let names: Vec<&str> = failed
        .iter()
        .filter_map(|f| f.get("project").and_then(|p| p.as_str()))
        .collect();
    Ok(Some(format!(
        "INCOMPLETE: {} of {members} repositories could not be reached ({}), so anything \
         recorded only in them is missing here. Failures: {first_error}",
        failed.len(),
        names.join(", ")
    )))
}

/// Recall from one member's own store, local tier only.
fn recall_one_local(
    member: &str,
    path: &std::path::Path,
    query: &str,
    limit: usize,
) -> Result<Vec<Value>, String> {
    let out = run_in_member(
        member,
        path,
        [
            "recall",
            query,
            "--limit",
            &limit.to_string(),
            "--scope",
            "local",
            "--format",
            "json",
        ],
    )?;
    if !out.status.success() {
        return Err(child_failure(member, path, &out, "recall failed"));
    }
    match serde_json::from_slice::<Value>(&out.stdout)
        .map_err(|e| format!("{member} ({}): unreadable answer: {e}", path.display()))?
    {
        Value::Array(v) => Ok(v),
        Value::Object(m) => Ok(m
            .get("memories")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default()),
        _ => Ok(Vec::new()),
    }
}

/// Recall across a whole group: the shared tier once, every member's local tier.
///
/// Without this, a group-bound session answers "what do we know about X" from
/// one repository's local memories plus the shared ones — and presents that as
/// the whole answer. The other members' local memories are invisible, and their
/// absence is not announced, which is worse than returning nothing.
///
/// The shared tier is queried once, not per member: it is one store, and asking
/// it eleven times would return each group memory eleven times.
pub fn do_recall_group(
    members: &[ProjectRow],
    query: &str,
    limit: usize,
    scope: &str,
    repo: Option<&str>,
) -> Result<String, String> {
    let want_shared = scope == "all" || scope == "group" || scope == "global";
    let want_local = scope == "all" || scope == "local";

    let mut ranked: Vec<(f64, Value)> = Vec::new();
    let mut failed = Vec::new();
    // Memories carry no cross-store comparable score the way vector hits do, so
    // rank fusion is the honest merge here.
    const K: f64 = 60.0;

    if want_shared {
        let shared = do_recall_global(query, limit, repo)?;
        if let Ok(v) = serde_json::from_str::<Value>(&shared) {
            let arr = v
                .get("memories")
                .and_then(|m| m.as_array())
                .cloned()
                .unwrap_or_default();
            for (rank, m) in arr.into_iter().enumerate() {
                ranked.push((1.0 / (K + (rank + 1) as f64), m));
            }
        }
    }

    let (present, skipped_missing) = split_missing(members);
    let reachable = present.len();
    if want_local {
        // Same reasoning as the code fan-out: the members are independent, so
        // querying them one after another buys latency and nothing else. A cold
        // project takes seconds, a warm one milliseconds.
        let mut gathered: Vec<(String, Result<Vec<Value>, String>)> = Vec::new();
        for batch in present.chunks(FANOUT_CONCURRENCY) {
            std::thread::scope(|scope_| {
                let handles: Vec<_> = batch
                    .iter()
                    .map(|m| {
                        scope_.spawn(move || {
                            (
                                m.name.clone(),
                                recall_one_local(&m.name, &m.path, query, limit),
                            )
                        })
                    })
                    .collect();
                for h in handles {
                    match h.join() {
                        Ok(r) => gathered.push(r),
                        Err(_) => gathered.push((
                            "<panicked>".to_string(),
                            Err("the recall thread panicked".to_string()),
                        )),
                    }
                }
            });
        }
        for (name, r) in gathered {
            match r {
                Ok(hits) => {
                    for (rank, mut hit) in hits.into_iter().enumerate() {
                        if let Value::Object(map) = &mut hit {
                            // A local memory only means something with its
                            // repository attached: the same note from two
                            // members is two different facts.
                            map.entry("repo").or_insert_with(|| json!(name.clone()));
                        }
                        ranked.push((1.0 / (K + (rank + 1) as f64), hit));
                    }
                }
                Err(e) => failed.push(json!({ "project": name, "error": e })),
            }
        }
    }

    ranked.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let total = ranked.len();
    let memories: Vec<Value> = ranked.into_iter().take(limit).map(|(_, m)| m).collect();

    let mut out = json!({ "memories": memories, "searched_group": true });
    if total > limit {
        // What did not fit has to be visible; silence here reads as "there was
        // nothing else".
        out["omitted_for_budget"] = json!({ "count": total - limit });
    }
    if !failed.is_empty() {
        out["failed_projects"] = json!(failed);
    }
    if !skipped_missing.is_empty() {
        out["skipped_missing"] = json!(skipped_missing);
    }
    // A scope without a local tier asked no member anything, so none can have
    // failed; the count only matters when members were actually queried.
    let queried = if want_local { reachable } else { 0 };
    let missing_asked = if want_local { skipped_missing.len() } else { 0 };
    if let Some(warning) = fan_out_warning(&failed, queried)?
        .or_else(|| all_missing_warning(queried, missing_asked, "queried"))
    {
        out["warning"] = json!(warning);
    }
    serde_json::to_string_pretty(&out).map_err(|e| e.to_string())
}

/// Search a *different* registered project.
///
/// Federating here is the right call, unlike for memory recall: the caller has
/// named one project, so this wakes exactly one server rather than all of them.
/// The project's own server owns its database and keeps its model warm, so the
/// search runs where it is cheapest.
pub fn do_search_project(
    project: &str,
    query: &str,
    limit: usize,
    language: Option<String>,
    mode: &str,
) -> Result<String, String> {
    let row = central()?
        .show(project)
        .map_err(|e| format!("{project}: {e}"))?;
    let path = row
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("no path recorded for `{project}`"))?;

    let mut args: Vec<String> = vec![
        "search".into(),
        query.into(),
        "--limit".into(),
        limit.to_string(),
        "--format".into(),
        "json".into(),
    ];
    if let Some(l) = &language {
        args.extend(["--language".into(), l.clone()]);
    }
    match mode {
        "keyword" => args.push("--keyword".into()),
        "hybrid" => args.push("--hybrid".into()),
        _ => {}
    }
    let out = run_in_member(project, std::path::Path::new(path), &args)?;
    if !out.status.success() {
        return Err(child_failure(
            project,
            std::path::Path::new(path),
            &out,
            "search failed",
        ));
    }
    let hits: Value = serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())?;
    let answer = devctx_core::search_hits(&hits);
    let items = answer.hits.clone();
    let budget = env_usize("DEVCTX_MAX_OUTPUT_TOKENS", DEFAULT_MAX_OUTPUT_TOKENS);
    let (kept, dropped) = fit_json_array(items, budget, Some("text"), |v| {
        let file = v.get("file").and_then(|f| f.as_str()).unwrap_or("");
        let line = v.get("start_line").and_then(|l| l.as_i64()).unwrap_or(0);
        format!("{file}:{line}")
    });
    let mut out = json!({ "project": project, "path": path, "hits": kept });
    if let Some(o) = merge_omitted(dropped, answer.omitted.as_ref()) {
        out["omitted_for_budget"] = o;
    }
    if let Some(f) = &answer.branch_fallback {
        out["branch_fallback"] = f.clone();
    }
    if let Some(w) = &answer.warning {
        out["warning"] = w.clone();
    }
    serde_json::to_string_pretty(&out).map_err(|e| e.to_string())
}

/// The `omitted_for_budget` note of a proxied answer: what this process dropped
/// (`dropped`, named) plus what the child already dropped for its own budget.
/// Both count, and both lists of names survive, or the caller is told less than
/// was left out.
fn merge_omitted(dropped: Vec<String>, child: Option<&Value>) -> Option<Value> {
    let child_count = child
        .and_then(|o| o.get("count"))
        .and_then(|c| c.as_u64())
        .unwrap_or(0) as usize;
    if dropped.is_empty() {
        return child.cloned();
    }
    let dropped_here = dropped.len();
    let mut items: Vec<Value> = dropped.into_iter().map(Value::String).collect();
    if let Some(theirs) = child
        .and_then(|o| o.get("items"))
        .and_then(|i| i.as_array())
    {
        items.extend(theirs.iter().cloned());
    }
    Some(json!({ "count": dropped_here + child_count, "items": items }))
}

/// `list_projects` tool: every repository DevCtxEngine knows about.
///
/// This is what lets an agent working in one repo discover the others without
/// being told they exist.
pub fn do_list_projects(
    project: Option<&str>,
    group: &str,
    include_inactive: bool,
) -> Result<String, String> {
    let projects = central()?
        .list(include_inactive)
        .map_err(|e| e.to_string())?;
    // Which project is bound, and what group it belongs to, decide where a
    // memory should be saved — and until now nothing reported either, so an
    // agent had to guess a scope it could not see the options for.
    let bound = project.map(|name| {
        json!({
            "project": name,
            "group": group,
            "remember_hint": if group.is_empty() {
                "This repository is in no group: use scope=\"local\" unless the \
                 lesson is true of every project, which is scope=\"global\"."
                    .to_string()
            } else {
                format!(
                    "This repository belongs to group `{group}`: save with \
                     scope=\"group\" anything a sibling repository of that \
                     product would need, scope=\"local\" for what is true only \
                     here, and scope=\"global\" only for what holds beyond this \
                     product."
                )
            },
        })
    });
    // An agent is the one caller that will never look at a terminal, so a
    // notice printed there reaches nobody. It rides along with the call an
    // agent makes to orient itself, and stays a statement of fact: updating is
    // a decision for the person, not something a tool call should perform
    // underneath a running session.
    let update = std::env::var("DEVCTX_UPDATE_AVAILABLE").ok().map(|v| {
        json!({
            "latest": v,
            "note": "A newer devctx is published. Tell the user they can run \
                     `devctx update`; do not update anything yourself.",
        })
    });
    serde_json::to_string_pretty(&json!({ "projects": projects, "bound": bound, "update": update }))
        .map_err(|e| e.to_string())
}

/// `remember` with `scope: global` or `scope: group`: store in the shared
/// central memory instead of this project's, so the other repositories — the
/// group's, or every one of them — can recall it.
#[allow(clippy::too_many_arguments)]
/// Write a shared (group or global) memory to the central store.
///
/// `provenance` overrides the repository recorded as the memory's origin. A
/// session bound to a *group* has no contributing repository: it is attached to
/// the product, not to any member. Deriving provenance from whichever member the
/// descent opened would not be attribution, it would be invention — the same
/// memory would claim a different origin depending on which repository happened
/// to be indexed last. Callers in that position pass the group name, and the
/// record then says what is true: this came from the group.
pub fn do_remember_shared(
    state: &AppState,
    content: &str,
    title: &str,
    memory_type: &str,
    topic: &str,
    tags: &str,
    group: &str,
    files: &str,
    provenance: Option<&str>,
) -> Result<String, String> {
    let project = state.project();
    let (repo, branch) = state.repo_branch().unwrap_or_default();
    // Provenance must never be blank: a directory that is not a git repo still
    // belongs to a named project, and "which project taught me this" is the
    // whole point of recording it.
    let repo = match provenance {
        Some(p) if !p.is_empty() => p.to_string(),
        _ if repo.is_empty() => project.clone(),
        _ => repo,
    };
    let out = central()?
        .remember(
            content,
            title,
            memory_type,
            topic,
            tags,
            &project,
            &repo,
            &branch,
            group,
            files,
        )
        .map_err(|e| e.to_string())?;

    // The memory now lives centrally; its link to the code must not. The graph
    // is per-repository, so the junction row goes in *this* project's store,
    // carrying only the id — which is what makes a group-wide decision findable
    // from the symbol it was made about.
    let mut out = out;
    if let Some(id) = out.get("id").and_then(|i| i.as_str()) {
        if let Ok(store) = state.open_store() {
            let linked = devctx_memory::links::link_memory(
                &store,
                &devctx_store::Memory {
                    id: id.to_string(),
                    content: content.to_string(),
                    files: files.to_string(),
                    repo: repo.clone(),
                    branch: branch.clone(),
                    ..Default::default()
                },
            );
            if let Some(o) = out.as_object_mut() {
                o.insert("linked_symbols".into(), json!(linked));
            }
        }
    }
    serde_json::to_string_pretty(&out).map_err(|e| e.to_string())
}

/// `remember` when no project is bound at all — not even a group — and the
/// caller did not ask for `scope: local`. The only project-less place left to
/// put the content is the central store as global, via the same write path
/// `do_remember_shared` uses and the same store `do_recall_global` reads back
/// from. `group` is left empty so `Central::remember` normalizes the scope to
/// global rather than rejecting it for having nowhere to go.
///
/// There is no `AppState` here, so there is no repository graph to link this
/// against — an unbound session has no project whose symbols it could mean.
pub fn do_remember_unbound(
    content: &str,
    title: &str,
    memory_type: &str,
    topic: &str,
    tags: &str,
    files: &str,
) -> Result<String, String> {
    let mut out = central()?
        .remember(
            content,
            title,
            memory_type,
            topic,
            tags,
            "",
            "unbound",
            "",
            "",
            files,
        )
        .map_err(|e| e.to_string())?;
    if let Some(o) = out.as_object_mut() {
        o.insert(
            "forced_scope".into(),
            json!(
                "global — no project was bound, so \"local\" had nowhere to write to. \
                 Move it later with memory_move if it belongs somewhere narrower."
            ),
        );
    }
    serde_json::to_string_pretty(&out).map_err(|e| e.to_string())
}

/// `recall` across scopes. Local and global results are fused by **rank**, not
/// score: the two stores may embed with different models, so their similarities
/// are not on comparable scales.
pub fn do_recall_scoped(
    state: &AppState,
    query: &str,
    limit: usize,
    scope: &str,
    repo: Option<&str>,
) -> Result<String, String> {
    // Anything that is not one single tier means "every tier", preserving the
    // permissive default an unset or unknown scope has always had.
    let every = !matches!(scope, "local" | "global" | "group");
    let want_local = every || scope == "local";
    let want_global = every || scope == "global";
    let want_group = (every || scope == "group") && !state.group_name().is_empty();

    let local: Vec<Value> = if want_local {
        let store = state.open_store()?;
        let project = state.project();
        let embedder = state.embedder()?;
        recall(
            &store,
            embedder.as_ref(),
            &RecallQuery {
                query,
                project: Some(&project),
                repo: None,
                limit,
            },
        )
        .map_err(|e| e.to_string())?
        .iter()
        .map(|h| {
            // Fall back to the bound project when the stored `repo` is empty.
            // These came out of *this* repository's store, so leaving the field
            // blank loses a fact we hold: a group recall then shows three
            // memories with no origin beside eight that have one, and the reader
            // cannot tell whether that means "unknown" or "everywhere".
            let origin = if h.memory.repo.is_empty() {
                project.clone()
            } else {
                h.memory.repo.clone()
            };
            json!({
                "id": h.memory.id, "title": h.memory.title, "content": h.memory.content,
                "type": h.memory.memory_type, "tags": h.memory.tags, "repo": origin,
            })
        })
        .collect()
    } else {
        Vec::new()
    };

    let global: Vec<Value> = if want_global {
        central()?
            .recall(query, limit, repo)
            .map_err(|e| e.to_string())?
    } else {
        Vec::new()
    };

    let group: Vec<Value> = if want_group {
        central()?
            .recall_scoped(query, limit, repo, Some(&state.group_name()))
            .map_err(|e| e.to_string())?
    } else {
        Vec::new()
    };

    let fused = fuse_by_rank(
        vec![(local, "local"), (group, "group"), (global, "global")],
        limit,
    );
    let budget = env_usize("DEVCTX_MAX_OUTPUT_TOKENS", DEFAULT_MAX_OUTPUT_TOKENS);
    let (fused, dropped) = fit_memories(fused, budget, |content, target| {
        do_summarize(state, content, Some(query.to_string()), target).ok()
    });
    serde_json::to_string_pretty(&json!({
        "memories": fused,
        "omitted_for_budget": { "count": dropped.len(), "titles": dropped },
    }))
    .map_err(|e| e.to_string())
}

/// Recall from the central store alone, for a session with no project bound.
///
/// Global memories are the ones written down precisely because they outlive the
/// project that learned them, so they are worth reaching without one.
pub fn do_recall_global(query: &str, limit: usize, repo: Option<&str>) -> Result<String, String> {
    let global = central()?
        .recall(query, limit, repo)
        .map_err(|e| e.to_string())?;
    let tagged = fuse_by_rank(vec![(global, "global")], limit);
    let budget = env_usize("DEVCTX_MAX_OUTPUT_TOKENS", DEFAULT_MAX_OUTPUT_TOKENS);
    // No project is bound here, so there is no embedder to summarize with: the
    // fallback truncation is the only option, and says so.
    let (tagged, dropped) = fit_memories(tagged, budget, |_, _| None);
    serde_json::to_string_pretty(&json!({
        "memories": tagged,
        "omitted_for_budget": { "count": dropped.len(), "titles": dropped },
    }))
    .map_err(|e| e.to_string())
}

/// Fit ranked memories into the output budget, returning what survived and the
/// titles of what did not.
///
/// The budget is split evenly across the memories rather than spent on the
/// first ones: a memory that fits its share arrives byte-for-byte, and one that
/// does not is summarized against the caller's own query — prose is exactly
/// what an extractive summary handles well, and losing a memory whole loses
/// more than abridging it.
///
/// Summaries are capped ([`MAX_SUMMARIES`]) because each costs an embedding
/// pass over the text — seconds, not milliseconds. Past the cap, memories are
/// dropped rather than silently making every recall slow, and their **titles**
/// come back with the count: a title is enough for the caller to decide whether
/// it needs to ask for that one specifically, which a bare number is not.
fn fit_memories(
    memories: Vec<Value>,
    budget_tokens: usize,
    shorten: impl Fn(&str, usize) -> Option<String>,
) -> (Vec<Value>, Vec<String>) {
    if budget_tokens == 0 || memories.is_empty() {
        return (memories, Vec::new());
    }
    // Below this a "share" is too small to say anything useful, so a long tail
    // of memories yields to a few readable ones rather than all being minced.
    const MIN_SHARE_TOKENS: usize = 128;
    let share = (budget_tokens / memories.len()).max(MIN_SHARE_TOKENS);

    let mut kept: Vec<Value> = Vec::new();
    let mut dropped: Vec<String> = Vec::new();
    let mut summaries = 0usize;
    for m in memories {
        let cost = m
            .get("content")
            .and_then(|c| c.as_str())
            .map(|c| c.len() / CHARS_PER_TOKEN)
            .unwrap_or(0);
        if cost <= share {
            kept.push(m);
            continue;
        }
        if summaries < MAX_SUMMARIES {
            summaries += 1;
            kept.push(shorten_memory(m, share, &shorten));
            continue;
        }
        dropped.push(memory_label(&m));
    }
    (kept, dropped)
}

/// How many memories one recall may summarize before it starts dropping them.
const MAX_SUMMARIES: usize = 3;

/// A memory's title, or its id when it has none — what a caller needs to ask
/// for it by name.
fn memory_label(m: &Value) -> String {
    let field = |k: &str| m.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let title = field("title");
    if title.is_empty() {
        let id = field("id");
        if id.is_empty() {
            "(untitled)".to_string()
        } else {
            id
        }
    } else {
        title
    }
}

/// Shrink one memory to `share` tokens, summarizing when that is possible.
///
/// Truncation is the fallback for when there is no summarizer to hand — an
/// unbound session, a provider that refused — because something readable beats
/// an error. Either way the memory says which happened: a shortened memory that
/// looked complete would be read as the whole argument, and acted on as one.
fn shorten_memory(
    mut m: Value,
    share_tokens: usize,
    shorten: &impl Fn(&str, usize) -> Option<String>,
) -> Value {
    let budget = share_tokens * CHARS_PER_TOKEN;
    let Some(obj) = m.as_object_mut() else {
        return m;
    };
    let Some(content) = obj.get("content").and_then(|c| c.as_str()) else {
        return m;
    };
    // Leave room for the note itself.
    let target = share_tokens.saturating_sub(40).max(1);
    if let Some(summary) = shorten(content, target) {
        // The summarizer may overshoot its target; the budget is the promise.
        let summary = if summary.len() > budget {
            summary.chars().take(budget).collect()
        } else {
            summary
        };
        let marked = format!(
            "{summary}\n\n[devctx] summarized: this memory exceeded its share of the output budget.\n"
        );
        obj.insert("content".to_string(), json!(marked));
        return m;
    }
    let mut out = String::with_capacity(budget + 96);
    for line in content.lines() {
        if out.len() + line.len() + 1 > budget {
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("\n[devctx] truncated: this memory exceeded its share of the output budget.\n");
    obj.insert("content".to_string(), json!(out));
    m
}

/// Fit a JSON array of tool results into the output budget.
///
/// Same shape and the same soft-budget philosophy as [`fit_memories`] — an
/// equal share of the budget per item, with a floor below which a share is
/// not worth enforcing on a long tail of small items — for tools with no
/// summarizer to fall back on: when `text_field` names a string field worth
/// shrinking, an oversized item is truncated in place; otherwise (or when the
/// item has no such field) it is dropped whole. Either way the caller is
/// told: dropped items are named by `label` under `omitted_for_budget`,
/// never silently missing.
fn fit_json_array(
    items: Vec<Value>,
    budget_tokens: usize,
    text_field: Option<&str>,
    label: impl Fn(&Value) -> String,
) -> (Vec<Value>, Vec<String>) {
    if budget_tokens == 0 || items.is_empty() {
        return (items, Vec::new());
    }
    // Below this a "share" is too small to hold anything useful, so a long
    // tail of small items is not minced down to nothing — same floor as
    // `fit_memories`.
    const MIN_SHARE_TOKENS: usize = 64;
    let share = (budget_tokens / items.len()).max(MIN_SHARE_TOKENS) * CHARS_PER_TOKEN;

    let mut kept: Vec<Value> = Vec::new();
    let mut dropped: Vec<String> = Vec::new();
    for mut item in items {
        let full_len = serde_json::to_string(&item).map(|s| s.len()).unwrap_or(0);
        if full_len <= share {
            kept.push(item);
            continue;
        }
        let Some(field) = text_field else {
            dropped.push(label(&item));
            continue;
        };
        let Some(obj) = item.as_object_mut() else {
            dropped.push(label(&item));
            continue;
        };
        let Some(text) = obj.get(field).and_then(|v| v.as_str()).map(str::to_string) else {
            dropped.push(label(&item));
            continue;
        };
        let mut out = String::with_capacity(share + 96);
        for line in text.lines() {
            if out.len() + line.len() + 1 > share {
                break;
            }
            out.push_str(line);
            out.push('\n');
        }
        out.push_str("\n[devctx] truncated: exceeded its share of the output budget.\n");
        obj.insert(field.to_string(), json!(out));
        kept.push(item);
    }
    (kept, dropped)
}

/// Fuse labelled result lists by rank, tagging each survivor with the scope it
/// came from so an agent can tell a project memory from a shared one.
fn fuse_by_rank(lists: Vec<(Vec<Value>, &str)>, limit: usize) -> Vec<Value> {
    let tagged: Vec<Vec<Value>> = lists
        .into_iter()
        .map(|(list, origin)| {
            list.into_iter()
                .map(|mut v| {
                    if let Some(obj) = v.as_object_mut() {
                        obj.insert("scope".to_string(), json!(origin));
                    }
                    v
                })
                .collect()
        })
        .collect();
    devctx_core::fuse_by_rank(
        tagged,
        |v| {
            v.get("id")
                .and_then(|x| x.as_str())
                .unwrap_or_default()
                .to_string()
        },
        limit,
    )
}

/// `memory_stats` tool: memory counts for the project.
pub fn do_memory_stats(state: &AppState) -> Result<String, String> {
    let store = state.open_store()?;
    let stats = memory_stats(&store, &state.project()).map_err(|e| e.to_string())?;
    let by_type: Vec<Value> = stats
        .by_type
        .iter()
        .map(|(ty, n)| json!({ "type": ty, "count": n }))
        .collect();
    Ok(json!({ "total": stats.total, "by_type": by_type }).to_string())
}

/// Link memories saved before the junction existed.
///
/// Sweeps this project's own memories and the shared ones, since a group
/// memory about this repository's code is exactly the case the junction was
/// built for, and it is stored where this process cannot reach it except
/// through the daemon.
pub fn do_backfill_links(
    state: &AppState,
    dry_run: bool,
    from_text: bool,
) -> Result<String, String> {
    let store = state.open_store()?;
    let mut memories = store.all_memories(&state.project()).unwrap_or_default();
    let local = memories.len();

    // The shared half arrives as JSON from the daemon; only the fields the
    // linker reads are needed, so a partial `Memory` is honest here.
    let mut shared = 0usize;
    if let Ok(c) = central() {
        for v in c.all_shared_memories().unwrap_or_default() {
            let s = |k: &str| {
                v.get(k)
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string()
            };
            let id = s("id");
            if id.is_empty() {
                continue;
            }
            shared += 1;
            memories.push(devctx_store::Memory {
                id,
                content: s("content"),
                files: s("files"),
                repo: s("repo"),
                branch: s("branch"),
                ..Default::default()
            });
        }
    }

    let r = devctx_memory::links::backfill_links(&store, &memories, dry_run, from_text);
    Ok(json!({
        "dry_run": dry_run,
        "from_text": from_text,
        "sources": { "local": local, "shared": shared },
        "examined": r.examined,
        "matched": r.matched,
        "linked_with_symbols": r.linked,
        "rows_written": r.rows,
        "linked_from_text": r.from_text,
        "skipped_no_files": r.without_files,
        "skipped_not_in_this_repo": r.not_in_repo,
    })
    .to_string())
}

/// `memory_forget` tool: delete one memory for good, wherever it lives.
///
/// An agent that can write a memory and not unwrite one accumulates its own
/// mistakes: a wrong root cause, a decision that was reversed an hour later. It
/// looks up both tiers because the id alone does not say which store holds it,
/// and the caller — reading an id out of a recall result — has no reason to know.
pub fn do_memory_forget(state: &AppState, id: &str) -> Result<String, String> {
    if let Ok(store) = state.open_store() {
        if store.forget_memory(id).unwrap_or(false) {
            return Ok(json!({ "id": id, "forgotten": true, "from": "project" }).to_string());
        }
    }
    // Not local, or this project's store could not be opened: the shared store
    // is as likely to hold it, and only the daemon may touch that one.
    match central() {
        Ok(c) => {
            let gone = c.forget_memory(id).map_err(|e| e.to_string())?;
            Ok(json!({ "id": id, "forgotten": gone, "from": "shared" }).to_string())
        }
        Err(e) => Err(format!(
            "`{id}` is not in this project, and the shared store is unreachable: {e}"
        )),
    }
}

/// `memory_move` tool: move a memory to another tier, or another repository.
///
/// A memory saved local that a sibling repository needs, or global when it only
/// ever concerned one product, is in the wrong place — invisible where it is
/// wanted, or noise where it is not. Rewriting it by hand loses its history and
/// usually its `files`.
///
/// Written to the destination **before** the original is removed. The other
/// order trades a recoverable duplicate for an unrecoverable loss, and there is
/// no version of this worth losing a memory over.
pub fn do_memory_move(state: &AppState, id: &str, to: &str) -> Result<String, String> {
    let m = load_memory_anywhere(state, id)?;

    // `local`, `group` and `global` are tiers of this project. Anything else is
    // read as the name of another registered project.
    let created = match to {
        "local" => {
            let store = state.open_store()?;
            let req = RememberRequest {
                title: m.title.clone(),
                content: m.content.clone(),
                memory_type: m.memory_type.clone(),
                project: state.project(),
                topic_key: m.topic_key.clone(),
                tags: m.tags.clone(),
                files: m.files.clone(),
                repo: m.repo.clone(),
                branch: m.branch.clone(),
                now: now_epoch(),
                ..Default::default()
            };
            let embedder = state.embedder()?;
            let res = remember(&store, embedder.as_ref(), &req).map_err(|e| e.to_string())?;
            devctx_memory::links::link_memory(&store, &res.memory);
            res.memory.id
        }
        "group" | "global" => {
            let group = if to == "group" {
                let g = state.group_name();
                if g.is_empty() {
                    return Err(
                        "this project declares no group; set `project.group` or move to `global`"
                            .into(),
                    );
                }
                g
            } else {
                String::new()
            };
            let out = do_remember_shared(
                state,
                &m.content,
                &m.title,
                &m.memory_type,
                &m.topic_key,
                &m.tags,
                &group,
                &m.files,
                // Moving a memory must not rewrite where it came from: its
                // provenance is a fact about its past, not about this session.
                Some(m.repo.as_str()).filter(|r| !r.is_empty()),
            )?;
            serde_json::from_str::<Value>(&out)
                .ok()
                .and_then(|v| v.get("id").and_then(|i| i.as_str()).map(str::to_string))
                .unwrap_or_default()
        }
        project => move_to_project(project, &m)?,
    };

    if created.is_empty() {
        return Err(format!(
            "the destination `{to}` did not report a new id; nothing was removed"
        ));
    }
    // Only now is it safe to drop the original.
    let removed = do_memory_forget(state, id).is_ok();
    Ok(json!({
        "moved": id,
        "to": to,
        "new_id": created,
        "original_removed": removed,
        // The id is derived from the project and the content, so moving a
        // memory necessarily renames it. A caller holding the old one needs to
        // be told rather than left to discover it.
        "note": "the id changes with the tier: it is derived from project + content",
    })
    .to_string())
}

/// Read a memory from this project's store, else the shared one.
fn load_memory_anywhere(state: &AppState, id: &str) -> Result<devctx_store::Memory, String> {
    if let Ok(store) = state.open_store() {
        if let Ok(Some(m)) = store.get_memory(id) {
            if m.deleted_at.is_none() {
                return Ok(m);
            }
        }
    }
    let c = central()?;
    let v = c
        .memories_by_id(&[id.to_string()])
        .map_err(|e| e.to_string())?
        .into_iter()
        .next()
        .ok_or_else(|| format!("no memory `{id}` in this project or the shared store"))?;
    serde_json::from_value(v).map_err(|e| format!("reading `{id}`: {e}"))
}

/// Hand a memory to another registered project, by running `remember` inside it.
///
/// The same route `search_project` takes: that project's database belongs to
/// its own server, and reaching across to write it directly is how two writers
/// end up on one DuckDB file.
fn move_to_project(project: &str, m: &devctx_store::Memory) -> Result<String, String> {
    let row = central()?
        .show(project)
        .map_err(|e| format!("{project}: {e}"))?;
    let path = row
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("no path recorded for `{project}`"))?;

    let mut args: Vec<String> = vec![
        "remember".into(),
        m.content.clone(),
        "--type".into(),
        m.memory_type.clone(),
    ];
    for (flag, val) in [
        ("--title", &m.title),
        ("--topic", &m.topic_key),
        ("--tags", &m.tags),
        ("--files", &m.files),
    ] {
        if !val.is_empty() {
            args.extend([flag.into(), val.clone()]);
        }
    }
    let out = run_in_member(project, std::path::Path::new(path), &args)?;
    if !out.status.success() {
        return Err(format!(
            "`{project}` refused the memory: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    // `remember` prints `<status>: <id> — <title>`.
    let stdout = String::from_utf8_lossy(&out.stdout);
    Ok(stdout
        .split_whitespace()
        .find(|w| w.starts_with("mem_"))
        .unwrap_or_default()
        .to_string())
}

/// `memory_context` tool: the most recent memories, with no query.
///
/// The recovery path, not the search path. After a context reset an agent does
/// not yet know what to ask about — asking `recall` requires already having the
/// words. This answers "what has been going on here" instead, which is the
/// question you actually have at that moment.
pub fn do_memory_context(state: &AppState, scope: &str, limit: usize) -> Result<String, String> {
    let mut out: Vec<Value> = Vec::new();

    if scope != "global" && scope != "group" {
        let store = state.open_store()?;
        for m in memory_context(&store, &state.project(), limit).map_err(|e| e.to_string())? {
            out.push(memory_json(&m, "local"));
        }
    }
    if scope != "local" {
        // Shared memories live behind the daemon; losing it costs that half of
        // the answer, not the whole call.
        if let Ok(c) = central() {
            for v in c.recent_memories(limit).unwrap_or_default() {
                out.push(value_json(v, "shared"));
            }
        }
    }

    out.sort_by(|a, b| {
        let key = |v: &Value| {
            v.get("updated_at")
                .and_then(|u| u.as_str())
                .unwrap_or_default()
                .to_string()
        };
        key(b).cmp(&key(a))
    });
    out.truncate(limit);
    let budget = env_usize("DEVCTX_MAX_OUTPUT_TOKENS", DEFAULT_MAX_OUTPUT_TOKENS);
    let (out, dropped) = fit_memories(out, budget, |content, target| {
        do_summarize(state, content, None, target).ok()
    });
    let mut resp = json!({ "scope": scope, "memories": out });
    if !dropped.is_empty() {
        resp["omitted_for_budget"] = json!({ "count": dropped.len(), "titles": dropped });
    }
    Ok(resp.to_string())
}

/// `impact_analysis` tool: blast radius (transitive callers/callees) of a symbol.
pub fn do_impact(state: &AppState, symbol: &str, depth: usize) -> Result<String, String> {
    let store = state.open_store()?;
    let chosen = graph_branch(state, &store)?;
    let (repo, branch) = (&chosen.repo, &chosen.branch);
    let resolved = store
        .resolve_symbol(repo, branch, symbol)
        .map_err(|e| e.to_string())?;
    let impact = store
        .impact_analysis(repo, branch, symbol, depth)
        .map_err(|e| e.to_string())?;
    let to_json = |v: &[(String, usize)]| -> Vec<Value> {
        v.iter()
            .map(|(s, d)| json!({ "symbol": s, "depth": d }))
            .collect()
    };
    let budget = env_usize("DEVCTX_MAX_OUTPUT_TOKENS", DEFAULT_MAX_OUTPUT_TOKENS);
    // Each direction gets half the budget: a symbol with a huge upstream and a
    // tiny downstream (or the reverse) is common, and a single shared pool
    // would let one side starve the other silently.
    let half = budget / 2;
    let label = |v: &Value| {
        v.get("symbol")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string()
    };
    let (upstream, up_dropped) = fit_json_array(to_json(&impact.upstream), half, None, label);
    let (downstream, down_dropped) = fit_json_array(to_json(&impact.downstream), half, None, label);
    let mut out = json!({
        "symbol": symbol,
        "upstream": upstream,
        "downstream": downstream,
    });
    // A bare name can stand for several methods, and the radius below merges
    // them. Say so: an unannounced merge reads as one method with a wide blast
    // radius, which is a different and much more alarming fact.
    if let Some(names) = merged_declarations(symbol, &resolved) {
        out["resolved_symbols"] = json!(names);
    }
    chosen.annotate(&mut out);
    let dropped_total = up_dropped.len() + down_dropped.len();
    if dropped_total > 0 {
        out["omitted_for_budget"] = json!({
            "count": dropped_total,
            "upstream": up_dropped,
            "downstream": down_dropped,
        });
    }
    Ok(out.to_string())
}

/// The declarations a bare name was expanded into, when that is worth telling
/// the caller: more than one, or one that is not the name they asked with.
/// `None` when the question already named exactly one thing.
pub fn merged_declarations(symbol: &str, resolved: &[String]) -> Option<Vec<String>> {
    match resolved {
        [only] if only.as_str() == symbol => None,
        [] => None,
        names => Some(names.to_vec()),
    }
}

/// `build_context` tool: one budgeted brief assembled for a question.
///
/// Three passes, in this order because each narrows what the next needs to say:
///
/// 1. **Recalled memories.** What was already decided about this area. First
///    because it is the part no amount of reading the code recovers, and
///    because it is small.
/// 2. **Code.** The chunks the search ranks highest, skipping files a memory
///    already brought in.
/// 3. **Linked memories.** Memories attached to the *files that pass one and
///    two chose* — the knowledge documented against exactly this code, which a
///    semantic recall on the question's wording would never have surfaced. This
///    is what the memory↔graph junction is for.
///
/// Returns prose rather than JSON: the result is meant to be read into a
/// model's context, and a JSON envelope around code and prose spends budget on
/// punctuation. The budget is a hard stop; whatever did not fit is named at the
/// end rather than silently dropped.
pub fn do_build_context(
    state: &AppState,
    query: &str,
    max_tokens: usize,
    include_memories: bool,
) -> Result<String, String> {
    let mut out = String::new();
    let mut used = 0usize;
    let mut dropped = 0usize;
    let budget_chars = max_tokens * CHARS_PER_TOKEN;

    // Append if it fits, else count it as dropped. Returns whether it fit.
    let push = |out: &mut String, used: &mut usize, dropped: &mut usize, s: &str| -> bool {
        if *used + s.len() > budget_chars {
            *dropped += 1;
            return false;
        }
        out.push_str(s);
        *used += s.len();
        true
    };

    let mut memory_files: Vec<String> = Vec::new();
    if include_memories {
        if let Ok(raw) = do_recall_scoped(state, query, 5, "all", None) {
            let mems = parse_memories(&raw);
            // The heading rides with the first item that fits. Emitted on its
            // own it survives a budget the items underneath did not, and an
            // empty section reads as "nothing here" — the one thing it must
            // never mean.
            let mut head = "## What is already known\n\n";
            for m in &mems {
                let title = m.get("title").and_then(|v| v.as_str()).unwrap_or("");
                let content = m.get("content").and_then(|v| v.as_str()).unwrap_or("");
                let files = m.get("files").and_then(|v| v.as_str()).unwrap_or("");
                for f in files.split(',').map(str::trim).filter(|f| !f.is_empty()) {
                    memory_files.push(f.to_string());
                }
                if push(
                    &mut out,
                    &mut used,
                    &mut dropped,
                    &format!("{head}[memory] {title}\n{content}\n\n"),
                ) {
                    head = "";
                }
            }
        }
    }

    // Fetch more than will fit: the budget, not the limit, decides where to stop.
    let raw = do_search(state, query, 30, None, SearchMode::Vector, false)?;
    let hits: Vec<Value> = parse_memories(&raw);
    // `do_search` says when it answered from another branch; a prose answer
    // must say so too, since the code below may differ from what is checked out.
    let fallback_note = fallback_note(&raw);
    let mut code_files: Vec<String> = Vec::new();
    let mut head = "## Code\n\n";
    for h in &hits {
        let file = h.get("file").and_then(|v| v.as_str()).unwrap_or("");
        let line = h.get("start_line").and_then(|v| v.as_i64()).unwrap_or(0);
        let text = h.get("text").and_then(|v| v.as_str()).unwrap_or("");
        // A file a memory already pulled in is not worth paying for twice.
        if memory_files.iter().any(|f| f == file) {
            continue;
        }
        if !code_files.iter().any(|f| f == file) {
            code_files.push(file.to_string());
        }
        if !push(
            &mut out,
            &mut used,
            &mut dropped,
            &format!("{head}// {file}:{line}\n{text}\n\n"),
        ) {
            break;
        }
        head = "";
    }

    if include_memories {
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut head = "## Recorded against this code\n\n";
        for file in code_files.iter().take(5) {
            let Ok(raw) = do_memories_by_file(state, file, 5) else {
                continue;
            };
            for m in parse_memories(&raw) {
                let id = m
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if !seen.insert(id) {
                    continue;
                }
                let title = m.get("title").and_then(|v| v.as_str()).unwrap_or("");
                let content = m.get("content").and_then(|v| v.as_str()).unwrap_or("");
                let src = m.get("link_sources").and_then(|v| v.as_str()).unwrap_or("");
                if push(
                    &mut out,
                    &mut used,
                    &mut dropped,
                    &format!("{head}[memory · {src} · about {file}] {title}\n{content}\n\n"),
                ) {
                    head = "";
                }
            }
        }
    }

    if dropped > 0 {
        out.push_str(&format!(
            "\n[devctx] {dropped} further item(s) did not fit in {max_tokens} tokens. \
             Raise max_tokens, or narrow the query.\n"
        ));
    }
    close_context(&mut out, fallback_note);
    Ok(out)
}

/// The tail of a `build_context` answer. The "nothing matched" line goes first:
/// the branch note is context for an answer, and must not hide that there was
/// none.
fn close_context(out: &mut String, fallback_note: Option<String>) {
    if out.is_empty() {
        out.push_str("[devctx] nothing indexed matched this query.\n");
    }
    if let Some(note) = fallback_note {
        out.push('\n');
        out.push_str(&note);
    }
}

/// The prose line for `do_search`'s `branch_fallback`, if its answer has one.
fn fallback_note(raw: &str) -> Option<String> {
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|v| v.get("branch_fallback").cloned())
        .map(|f| {
            format!(
                "[devctx] branch_fallback: {}\n",
                f.get("why")
                    .and_then(|w| w.as_str())
                    .unwrap_or("answered from another branch")
            )
        })
}

/// Pull the `memories` array out of one of our own JSON answers.
///
/// The recall and by-file answers differ in shape — one is a bare array, the
/// other wraps it — so both are accepted rather than making the caller care.
/// Also accepts `results` (`do_search`'s wrapped shape once budget-trimmed),
/// so `do_build_context` keeps working whether or not `do_search` had to trim.
fn parse_memories(raw: &str) -> Vec<Value> {
    let parsed: Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    if let Some(a) = parsed.as_array() {
        return a.clone();
    }
    parsed
        .get("memories")
        .or_else(|| parsed.get("results"))
        .and_then(|m| m.as_array())
        .cloned()
        .unwrap_or_default()
}

/// `read_symbol` tool: a symbol's definition and code.
///
/// Distinct from `search`, which answers "what code is about this idea" with
/// ranked fragments. Here the caller already knows the name and wants the thing
/// itself — so an exact name beats a similar one, and a miss is reported as a
/// miss rather than padded with the nearest neighbours.
pub fn do_read_symbol(state: &AppState, name: &str, limit: usize) -> Result<String, String> {
    let store = state.open_store()?;
    let chosen = graph_branch(state, &store)?;
    let found = store
        .symbol_definitions(&chosen.repo, &chosen.branch, name, limit)
        .map_err(|e| e.to_string())?;
    let mut out = json!({
        "symbol": name,
        "definitions": found.iter().map(|p| json!({
            "symbol": p.metadata.symbol,
            "type": p.metadata.symbol_type,
            "language": p.metadata.language,
            "file": p.metadata.file,
            "start_line": p.metadata.start_line,
            "end_line": p.metadata.end_line,
            "code": p.text,
        })).collect::<Vec<_>>(),
    });
    chosen.annotate(&mut out);
    Ok(out.to_string())
}

/// `memories_by_symbol` tool: the decisions recorded about a symbol.
///
/// Answers the question a call graph cannot: not "what calls this" but "what
/// did we decide about this, and why". Reaches both stores because the graph is
/// per-repository while a global or group memory is not — see
/// `devctx_memory::links` for why the junction row lives with the graph.
pub fn do_memories_by_symbol(
    state: &AppState,
    symbol: &str,
    limit: usize,
) -> Result<String, String> {
    let store = state.open_store()?;
    // Same branch rule as the graph tools: the junction rows are filed under
    // the branch that was indexed, which is not the checked-out one on a branch
    // nobody has indexed yet.
    let (repo, branch, fallback) = symbol_branch(state, &store);
    let linked = store
        .memory_ids_for_symbol(symbol, &repo, &branch, limit)
        .map_err(|e| e.to_string())?;
    let raw = linked_response(
        &store,
        symbol,
        linked,
        devctx_store::short_label(symbol),
        limit,
    )?;
    let Some(f) = fallback else {
        return Ok(raw);
    };
    let mut v: Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    // A text-inference match never looked at a branch, so saying which branch
    // answered would be false precision.
    if v["matched_by"] != "text-inference" {
        v["branch_fallback"] = f.to_json();
    }
    Ok(v.to_string())
}

/// The `(repo, branch, fallback)` junction rows are looked up under: the branch
/// the graph tools use (the junction is filed under the indexed branch, which
/// is not the checked-out one on a branch nobody has indexed yet), or the
/// checked-out one when nothing is indexed at all.
fn symbol_branch(
    state: &AppState,
    store: &devctx_store::Store,
) -> (String, String, Option<BranchFallback>) {
    match pick_branch(state, store) {
        Ok(c) if c.indexed => (c.repo, c.branch, c.fallback),
        _ => {
            let (r, b) = state.repo_branch().unwrap_or_default();
            (r, b, None)
        }
    }
}

/// `memories_by_file` tool: the decisions recorded about a file, plus the plan tasks that
/// mention it (`plan_tasks`, PLAN-005 TASK-007) — computed at call time from `plans/`, never
/// stored, so a parser failure never breaks the memory half of the answer.
pub fn do_memories_by_file(state: &AppState, file: &str, limit: usize) -> Result<String, String> {
    let store = state.open_store()?;
    let mut linked = store
        .memory_ids_for_file(file, limit)
        .map_err(|e| e.to_string())?;

    // The junction stores the path the index uses, and a caller who has a bare
    // file name — which is how anyone refers to a file in conversation — would
    // otherwise get nothing while the links sit right there. Resolve the name
    // the same way the writer did, so both spellings reach the same rows — and
    // the same resolved path is what plan-task matching below also tries.
    let resolved: Option<String> = if file.contains('/') {
        None
    } else {
        store
            .file_index()
            .ok()
            .and_then(|index| index.resolve(&[file.to_string()]).into_iter().next())
    };
    if linked.is_empty() {
        if let Some(r) = &resolved {
            linked = store
                .memory_ids_for_file(r, limit)
                .map_err(|e| e.to_string())?;
        }
    }

    let out = linked_response(&store, file, linked, file, limit)?;
    let plan_tasks = plan_tasks_for_file(&state.plans_root.root, file, resolved.as_deref());
    Ok(with_plan_tasks_field(out, plan_tasks))
}

/// Plan tasks (across all plans in `plans/`) whose `Archivos`/inline-code file references match
/// `query` (or `resolved`, the index's resolution of a bare filename), per the matching rule in
/// [`devctx_core::plans::FileRef::matches`]. Non-done first, then by plan number descending,
/// capped at 10. Never errors: an unreadable `plans/` yields an empty list.
fn plan_tasks_for_file(root: &std::path::Path, query: &str, resolved: Option<&str>) -> Vec<Value> {
    let loaded = plans::load_plans(root);
    let mut hits: Vec<(&Plan, &devctx_core::plans::Task, Option<u32>)> = Vec::new();
    for p in &loaded {
        for t in &p.tasks {
            let m = t
                .files
                .iter()
                .find(|f| f.matches(query) || resolved.map(|r| f.matches(r)).unwrap_or(false));
            if let Some(fref) = m {
                hits.push((p, t, fref.line));
            }
        }
    }
    hits.sort_by(|a, b| {
        let by_done = a.1.status.is_resolved().cmp(&b.1.status.is_resolved());
        if by_done != std::cmp::Ordering::Equal {
            return by_done;
        }
        plan_number(&b.0.id).cmp(&plan_number(&a.0.id))
    });
    hits.truncate(10);
    hits.into_iter()
        .map(|(p, t, line)| {
            json!({
                "plan": p.id,
                "task": t.id,
                "title": t.title,
                "status": plans::status_label(&t.status),
                "line": line,
            })
        })
        .collect()
}

/// Adds `plan_tasks` to a `memories_by_file`-shaped JSON object. Always present (an empty array
/// means "none", never "not computed") — the same rule as `omitted_for_budget` elsewhere in this
/// module, so an older client cannot mistake a missing field for a question never asked.
fn with_plan_tasks_field(out: String, plan_tasks: Vec<Value>) -> String {
    match serde_json::from_str::<Value>(&out) {
        Ok(Value::Object(mut map)) => {
            map.insert("plan_tasks".to_string(), Value::Array(plan_tasks));
            serde_json::to_string(&Value::Object(map)).unwrap_or(out)
        }
        _ => out,
    }
}

/// `memory_refs` tool: the inverse — what code one memory concerns.
pub fn do_memory_refs(state: &AppState, memory_id: &str) -> Result<String, String> {
    let store = state.open_store()?;
    let refs = store.memory_refs(memory_id).map_err(|e| e.to_string())?;
    Ok(json!({
        "memory_id": memory_id,
        "references": refs.iter().map(|r| json!({
            "symbol": r.symbol,
            "file": r.file,
            "line": r.line,
            "repo": r.repo,
            "branch": r.branch,
            "source": r.source,
        })).collect::<Vec<_>>(),
    })
    .to_string())
}

/// Resolve junction hits into memories, falling back to a literal text search
/// when the junction has nothing.
///
/// `link_sources` is in every result on purpose: `files-field` and
/// `content-mention` mean something connected this memory to this code at write
/// time, while `inference` means only that the words match. A caller weighing
/// how much to trust a memory needs to see the difference.
fn linked_response(
    store: &devctx_store::Store,
    subject: &str,
    linked: Vec<(String, String)>,
    fallback_label: &str,
    limit: usize,
) -> Result<String, String> {
    let mut out: Vec<Value> = Vec::new();

    // Resolve locally first; whatever is left is a shared memory, which only
    // the central daemon may read.
    let mut unresolved: Vec<String> = Vec::new();
    let mut sources_of: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for (id, sources) in linked {
        sources_of.insert(id.clone(), sources.clone());
        match store.get_memory(&id) {
            Ok(Some(m)) if m.deleted_at.is_none() => out.push(memory_json(&m, &sources)),
            _ => unresolved.push(id),
        }
    }
    if !unresolved.is_empty() {
        // A central store that cannot be reached costs the shared half of the
        // answer, not the whole answer.
        if let Ok(c) = central() {
            for v in c.memories_by_id(&unresolved).unwrap_or_default() {
                let id = v.get("id").and_then(|i| i.as_str()).unwrap_or_default();
                let sources = sources_of.get(id).cloned().unwrap_or_default();
                out.push(value_json(v, &sources));
            }
        }
    }

    let mut fallback_used = false;
    if out.is_empty() {
        fallback_used = true;
        let mut seen = std::collections::HashSet::new();
        for m in store
            .memories_mentioning(fallback_label, limit)
            .unwrap_or_default()
        {
            if seen.insert(m.id.clone()) {
                out.push(memory_json(&m, "inference"));
            }
        }
        if let Ok(c) = central() {
            for v in c
                .memories_mentioning(fallback_label, limit)
                .unwrap_or_default()
            {
                let id = v.get("id").and_then(|i| i.as_str()).unwrap_or_default();
                if seen.insert(id.to_string()) {
                    out.push(value_json(v, "inference"));
                }
            }
        }
    }

    out.sort_by(|a, b| {
        let key = |v: &Value| {
            v.get("updated_at")
                .and_then(|u| u.as_str())
                .unwrap_or_default()
                .to_string()
        };
        key(b).cmp(&key(a))
    });
    out.truncate(limit);

    let budget = env_usize("DEVCTX_MAX_OUTPUT_TOKENS", DEFAULT_MAX_OUTPUT_TOKENS);
    let (out, dropped) = fit_memories(out, budget, |_, _| None);
    let mut resp = json!({
        "subject": subject,
        "memories": out,
        // Named so a reader knows the results are word matches, not recorded links.
        "matched_by": if fallback_used { "text-inference" } else { "junction" },
    });
    if !dropped.is_empty() {
        resp["omitted_for_budget"] = json!({ "count": dropped.len(), "titles": dropped });
    }
    Ok(resp.to_string())
}

fn memory_json(m: &devctx_store::Memory, sources: &str) -> Value {
    json!({
        "id": m.id,
        "title": m.title,
        "content": m.content,
        "type": m.memory_type,
        "scope": m.scope,
        "project": m.project,
        "tags": m.tags,
        "repo": m.repo,
        "files": m.files,
        "updated_at": m.updated_at,
        "link_sources": sources,
    })
}

/// The same shape, from a memory the central daemon serialized.
fn value_json(v: Value, sources: &str) -> Value {
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or_default();
    json!({
        "id": s("id"),
        "title": s("title"),
        "content": s("content"),
        "type": s("memory_type"),
        "scope": s("scope"),
        "project": s("project"),
        "tags": s("tags"),
        "repo": s("repo"),
        "files": s("files"),
        "updated_at": s("updated_at"),
        "link_sources": sources,
    })
}

/// `get_references` tool: all call sites of a symbol.
pub fn do_references(state: &AppState, symbol: &str) -> Result<String, String> {
    let store = state.open_store()?;
    let chosen = graph_branch(state, &store)?;
    let resolved = store
        .resolve_symbol(&chosen.repo, &chosen.branch, symbol)
        .map_err(|e| e.to_string())?;
    let refs = store
        .find_references(&chosen.repo, &chosen.branch, symbol)
        .map_err(|e| e.to_string())?;
    let arr: Vec<Value> = refs
        .iter()
        .map(|r| json!({ "file": r.file, "line": r.line, "source": r.source }))
        .collect();
    let budget = env_usize("DEVCTX_MAX_OUTPUT_TOKENS", DEFAULT_MAX_OUTPUT_TOKENS);
    let (kept, dropped) = fit_json_array(arr, budget, Some("source"), |v| {
        let file = v.get("file").and_then(|f| f.as_str()).unwrap_or("");
        let line = v.get("line").and_then(|l| l.as_i64()).unwrap_or(0);
        format!("{file}:{line}")
    });
    let mut out = json!({ "symbol": symbol, "references": kept });
    if let Some(names) = merged_declarations(symbol, &resolved) {
        out["resolved_symbols"] = json!(names);
    }
    chosen.annotate(&mut out);
    if !dropped.is_empty() {
        out["omitted_for_budget"] = json!({ "count": dropped.len(), "items": dropped });
    }
    serde_json::to_string_pretty(&out).map_err(|e| e.to_string())
}

/// `search_routes` tool: find HTTP routes by optional method + path substring.
pub fn do_search_routes(
    state: &AppState,
    method: Option<String>,
    path: Option<String>,
) -> Result<String, String> {
    let store = state.open_store()?;
    let chosen = graph_branch(state, &store)?;
    let routes = store
        .search_routes(
            &chosen.repo,
            &chosen.branch,
            method.as_deref(),
            path.as_deref(),
        )
        .map_err(|e| e.to_string())?;
    routes_to_json(&routes, &chosen)
}

/// `routes_for_handler` tool: routes served by a handler symbol.
pub fn do_routes_for_handler(state: &AppState, handler: &str) -> Result<String, String> {
    let store = state.open_store()?;
    let chosen = graph_branch(state, &store)?;
    let routes = store
        .routes_for_handler(&chosen.repo, &chosen.branch, handler)
        .map_err(|e| e.to_string())?;
    routes_to_json(&routes, &chosen)
}

fn routes_to_json(
    routes: &[devctx_store::StoredRoute],
    chosen: &BranchChoice,
) -> Result<String, String> {
    let arr: Vec<Value> = routes
        .iter()
        .map(|r| {
            json!({
                "framework": r.framework,
                "method": r.http_method,
                "path": r.path,
                "handler": r.handler_symbol,
                "file": r.file,
                "line": r.line,
            })
        })
        .collect();
    let budget = env_usize("DEVCTX_MAX_OUTPUT_TOKENS", DEFAULT_MAX_OUTPUT_TOKENS);
    let (kept, dropped) = fit_json_array(arr, budget, None, |v| {
        let method = v.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let path = v.get("path").and_then(|p| p.as_str()).unwrap_or("");
        format!("{method} {path}")
    });
    if dropped.is_empty() && chosen.fallback.is_none() && !chosen.extractor_stale {
        return serde_json::to_string_pretty(&Value::Array(kept)).map_err(|e| e.to_string());
    }
    let mut out = json!({ "routes": kept });
    if !dropped.is_empty() {
        out["omitted_for_budget"] = json!({ "count": dropped.len(), "items": dropped });
    }
    chosen.annotate(&mut out);
    serde_json::to_string_pretty(&out).map_err(|e| e.to_string())
}

/// `summarize` tool: condense `content`, optionally focused on `query`.
pub fn do_summarize(
    state: &AppState,
    content: &str,
    query: Option<String>,
    target_tokens: usize,
) -> Result<String, String> {
    let s = &state.cfg.summarization;
    let extractive = s.provider.is_empty() || s.provider == "extractive";
    let embedder = if extractive {
        Some(state.embedder()?)
    } else {
        None
    };
    let summarizer = create_summarizer(
        &SummarizeSettings {
            provider: s.provider.clone(),
            require_local: s.require_local,
            target_tokens,
            model: s.model.clone(),
            api_key: None,
        },
        embedder,
    )
    .map_err(|e| e.to_string())?;
    summarizer
        .summarize(content, query.as_deref(), target_tokens)
        .map_err(|e| e.to_string())
}

/// The trailing segment of a symbol id (after the last `::` or `/`).
fn short_label(id: &str) -> &str {
    if let Some(i) = id.rfind("::") {
        return &id[i + 2..];
    }
    if let Some(i) = id.rfind('/') {
        return &id[i + 1..];
    }
    id
}

/// Placeholder nodes the parsers emit for un-resolvable receivers.
fn is_synthetic(id: &str) -> bool {
    matches!(short_label(id), "<unknown>" | "<module>" | "<anonymous>")
}

fn now_epoch() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_default()
}

fn hits_to_json(hits: &[SearchResult]) -> Value {
    Value::Array(
        hits.iter()
            .map(|h| {
                let m = &h.point.metadata;
                json!({
                    "score": h.score,
                    "file": m.file,
                    "start_line": m.start_line,
                    "end_line": m.end_line,
                    "symbol": m.symbol,
                    "symbol_type": m.symbol_type,
                    "level": m.chunk_level,
                    "text": h.point.text,
                })
            })
            .collect(),
    )
}

/// Extract a 1-based inclusive line range (both bounds optional).
fn slice_lines(content: &str, start: Option<usize>, end: Option<usize>) -> String {
    if start.is_none() && end.is_none() {
        return content.to_string();
    }
    let lines: Vec<&str> = content.lines().collect();
    let from = start.unwrap_or(1).max(1);
    let to = end.unwrap_or(lines.len()).min(lines.len());
    if from > to {
        return String::new();
    }
    lines[from - 1..to].join("\n")
}

#[cfg(test)]
mod tests {
    use super::fan_out_warning;

    /// The failure this exists for. A fan-out where every member fell over used
    /// to answer from the shared tier alone and put the eleven failures in a
    /// key nobody reads — indistinguishable from "nothing recorded about that",
    /// which is why it went unnoticed for months.
    #[test]
    fn every_member_failing_is_an_error_not_a_thin_answer() {
        let failed: Vec<serde_json::Value> = (0..11)
            .map(|i| serde_json::json!({ "project": format!("repo{i}"), "error": "boom" }))
            .collect();
        let err = fan_out_warning(&failed, 11).expect_err("should refuse to answer");
        assert!(err.contains("None of the 11"), "{err}");
        assert!(
            err.contains("boom"),
            "the cause has to travel with it: {err}"
        );
    }

    /// Some failing is still an answer — but one that says what is missing, in
    /// prose, where a reader cannot skim past it.
    #[test]
    fn some_members_failing_warns_without_refusing() {
        let failed = vec![serde_json::json!({ "project": "api", "error": "timed out" })];
        let warning = fan_out_warning(&failed, 11)
            .expect("a partial answer is still an answer")
            .expect("and it has to carry a warning");
        assert!(warning.contains("1 of 11"), "{warning}");
        assert!(warning.contains("api"), "name who is missing: {warning}");
        assert!(warning.contains("timed out"), "{warning}");
    }

    /// B5: a failure names the member and path, and the refusal lists up to
    /// three of them rather than only the first.
    #[test]
    fn the_refusal_lists_up_to_three_failures() {
        let failed: Vec<serde_json::Value> = (0..5)
            .map(|i| serde_json::json!({ "project": format!("r{i}"), "error": format!("r{i} (/p/r{i}): boom{i}") }))
            .collect();
        let err = fan_out_warning(&failed, 5).expect_err("all failed");
        for i in 0..3 {
            assert!(err.contains(&format!("(/p/r{i}): boom{i}")), "{err}");
        }
        assert!(!err.contains("boom3"), "{err}");
        assert!(err.contains("2 more"), "{err}");
    }

    #[test]
    fn a_missing_member_path_is_named_before_any_spawn() {
        let err = super::run_in_member("web", std::path::Path::new("/no/such/dir"), ["x"])
            .expect_err("path does not exist");
        assert!(err.contains("web (/no/such/dir)"), "{err}");
        assert!(err.contains("does not exist"), "{err}");
    }

    #[test]
    fn nothing_failing_says_nothing() {
        assert!(fan_out_warning(&[], 11).unwrap().is_none());
    }

    /// A group with no members cannot have "all of them" fail: guarding on
    /// `members > 0` keeps an empty group from turning every call into an error.
    #[test]
    fn an_empty_group_does_not_refuse() {
        let failed = vec![serde_json::json!({ "project": "x", "error": "boom" })];
        assert!(fan_out_warning(&failed, 0).is_ok());
    }

    /// TASK-008 fixup D1: every member missing on disk used to produce no
    /// warning at all (missing is not "failed", and nothing was reachable).
    #[test]
    fn all_members_missing_is_said_out_loud() {
        let w = super::all_missing_warning(0, 3, "queried").expect("must warn");
        assert!(w.contains("all 3 registered member(s)"), "{w}");
        assert!(w.contains("skipped_missing"), "{w}");
        assert!(super::all_missing_warning(1, 3, "queried").is_none());
        assert!(super::all_missing_warning(0, 0, "queried").is_none());
    }

    use super::*;
    use devctx_core::{VectorMetadata, VectorPoint};

    fn mem(id: &str, content_len: usize) -> Value {
        json!({ "id": id, "title": id, "content": "x".repeat(content_len) })
    }

    /// Under budget, nothing is touched and nothing is reported missing.
    #[test]
    fn memories_that_fit_are_all_returned() {
        let (kept, dropped) = fit_memories(vec![mem("a", 100), mem("b", 100)], 8000, |_, _| None);
        assert_eq!(kept.len(), 2);
        assert!(dropped.is_empty());
        assert_eq!(kept[0]["content"].as_str().unwrap(), "x".repeat(100));
    }

    /// The budget is shared out, so a memory within its slice survives intact
    /// even when a *later* one is enormous — the first ones no longer eat the
    /// whole allowance.
    #[test]
    fn a_memory_within_its_share_is_untouched_however_big_its_neighbours_are() {
        let mems = vec![mem("small", 100), mem("huge", 200_000)];
        let (kept, dropped) = fit_memories(mems, 8000, |_, _| Some("gist".into()));
        assert_eq!(kept.len(), 2, "both come back");
        assert!(dropped.is_empty());
        assert_eq!(kept[0]["content"].as_str().unwrap(), "x".repeat(100));
        assert!(kept[1]["content"].as_str().unwrap().starts_with("gist"));
    }

    /// Past the summary cap, memories are dropped rather than making every
    /// recall pay for another embedding pass — and their titles come back, so
    /// the caller can ask for one by name instead of guessing what it missed.
    #[test]
    fn beyond_the_cap_memories_are_dropped_and_named() {
        let big = 200_000;
        let mems: Vec<Value> = ["a", "b", "c", "d", "e"]
            .iter()
            .map(|n| mem(n, big))
            .collect();
        let calls = std::cell::Cell::new(0);
        let (kept, dropped) = fit_memories(mems, 8000, |_, _| {
            calls.set(calls.get() + 1);
            Some("gist".into())
        });
        assert_eq!(calls.get(), MAX_SUMMARIES, "the cap bounds the cost");
        assert_eq!(kept.len(), MAX_SUMMARIES);
        assert_eq!(dropped, vec!["d".to_string(), "e".to_string()]);
    }

    /// With no summarizer, an oversized memory is truncated and says so — the
    /// two outcomes must never look alike.
    #[test]
    fn without_a_summarizer_an_oversized_memory_is_truncated_and_says_so() {
        let big = json!({ "id": "a", "title": "a", "content": "line\n".repeat(5000) });
        let (kept, dropped) = fit_memories(vec![big], 200, |_, _| None);
        assert!(dropped.is_empty());
        let content = kept[0]["content"].as_str().unwrap();
        assert!(content.contains("[devctx] truncated"), "{content:?}");
        assert!(!content.contains("summarized"));
    }

    /// With one, it is summarized against the caller's query instead — the
    /// difference between "the first paragraph" and "what this says about what
    /// you asked".
    #[test]
    fn an_oversized_memory_is_summarized_when_a_summarizer_exists() {
        let big = json!({ "id": "a", "title": "a", "content": "line\n".repeat(5000) });
        let (kept, _) = fit_memories(vec![big], 200, |content, target| {
            assert!(!content.is_empty());
            assert!(target > 0, "the summarizer needs room for the note");
            Some("the gist of the whole thing".to_string())
        });
        let content = kept[0]["content"].as_str().unwrap();
        assert!(
            content.starts_with("the gist of the whole thing"),
            "{content:?}"
        );
        assert!(content.contains("[devctx] summarized"), "{content:?}");
        assert!(!content.contains("truncated"), "must not claim it was cut");
    }

    /// A memory with no title falls back to its id: the point of the list is to
    /// be able to ask for one, and an empty string cannot be asked for.
    #[test]
    fn a_dropped_memory_without_a_title_is_named_by_id() {
        assert_eq!(
            memory_label(&json!({ "id": "mem_x", "title": "" })),
            "mem_x"
        );
        assert_eq!(memory_label(&json!({ "title": "T" })), "T");
    }

    /// A file that fits comes back byte-for-byte: the cap must be invisible
    /// until it is needed.
    #[test]
    fn a_small_file_is_returned_whole() {
        let src = "fn main() {\n    println!(\"hi\");\n}\n";
        assert_eq!(cap_whole_file(src, 8000), src);
    }

    /// Over budget, the cut lands on a line boundary and the note carries the
    /// file's real length — the one fact needed to ask for the rest.
    #[test]
    fn an_oversized_file_is_cut_on_a_line_and_says_what_is_missing() {
        let src: String = (1..=200).map(|i| format!("line {i}\n")).collect();
        let out = cap_whole_file(&src, 10); // ~40 chars

        let body: Vec<&str> = out.lines().take_while(|l| l.starts_with("line ")).collect();
        assert!(!body.is_empty(), "kept nothing");
        assert!(body.len() < 200, "kept everything: {}", body.len());
        // Whole lines only — never a fragment of one.
        assert_eq!(body[0], "line 1");
        assert_eq!(body[body.len() - 1], format!("line {}", body.len()));
        assert!(out.contains("of 200"), "note must carry the total: {out:?}");
        assert!(
            out.contains("start_line"),
            "note must say how to get the rest"
        );
    }

    /// `0` is the escape hatch for anyone who wants the old behaviour back.
    #[test]
    fn a_zero_budget_disables_the_cap() {
        let src: String = (1..=500).map(|i| format!("line {i}\n")).collect();
        assert_eq!(cap_whole_file(&src, 0), src);
    }

    /// The timestamp is the whole mechanism behind
    /// [`AppState::release_idle_models`]: a model handed out again has to look
    /// fresh, or the sweep would drop one that is in active use and make every
    /// other request pay to reload it.
    #[test]
    fn handing_out_a_cached_model_resets_its_clock() {
        let mut cached = Cached {
            value: Arc::new(7u32),
            last_used: Instant::now()
                .checked_sub(Duration::from_secs(600))
                .expect("an instant ten minutes ago"),
        };
        assert!(cached.last_used.elapsed() >= Duration::from_secs(600));

        let handed_out = cached.touch();

        assert_eq!(*handed_out, 7, "the caller still gets the model");
        assert!(
            cached.last_used.elapsed() < Duration::from_secs(1),
            "and it no longer looks idle"
        );
    }

    #[test]
    fn slice_lines_ranges() {
        let c = "a\nb\nc\nd\ne";
        assert_eq!(slice_lines(c, None, None), c);
        assert_eq!(slice_lines(c, Some(2), Some(4)), "b\nc\nd");
        assert_eq!(slice_lines(c, Some(4), None), "d\ne");
        assert_eq!(slice_lines(c, None, Some(2)), "a\nb");
        assert_eq!(slice_lines(c, Some(10), Some(20)), "");
    }

    #[test]
    fn short_label_and_synthetic() {
        assert_eq!(short_label("crate::mod::func"), "func");
        assert_eq!(short_label("pkg/Class"), "Class");
        assert_eq!(short_label("plain"), "plain");
        assert!(is_synthetic("mod::<unknown>"));
        assert!(is_synthetic("<module>"));
        assert!(!is_synthetic("mod::real"));
    }

    #[test]
    fn hits_to_json_shape() {
        let hit = SearchResult {
            score: 0.5,
            point: VectorPoint {
                id: "x".into(),
                vector: vec![],
                text: "fn foo(){}".into(),
                metadata: VectorMetadata {
                    file: "a.rs".into(),
                    symbol: "foo".into(),
                    chunk_level: "function".into(),
                    start_line: 1,
                    end_line: 2,
                    ..Default::default()
                },
            },
        };
        let v = hits_to_json(&[hit]);
        assert_eq!(v[0]["file"], "a.rs");
        assert_eq!(v[0]["symbol"], "foo");
        assert_eq!(v[0]["start_line"], 1);
        assert!(v[0].get("vector").is_none());
    }

    /// The reported bar read 788/647: a second run took the slot mid-flight,
    /// and its files kept counting against the first run's total.
    #[test]
    fn a_second_run_cannot_overwrite_the_one_being_watched() {
        let shared = Arc::new(Mutex::new(IndexProgress::default()));
        let full = SharedProgress::new(shared.clone());
        let watcher = SharedProgress::new(shared.clone());

        full.start(647);
        for i in 0..600 {
            full.file(&format!("src/{i}.ts"));
        }
        // A save lands while the full run is still going.
        watcher.start(1);
        watcher.file("package-lock.json");
        full.file("src/600.ts");

        let p = shared.lock().unwrap().clone();
        assert_eq!(
            p.total, 647,
            "the watcher must not resize the run on screen"
        );
        assert_eq!(p.done, 601, "only the owning run may advance the count");
        assert!(p.done <= p.total, "the bar must never pass its own total");
        assert_eq!(p.run, 1, "the watcher started no run anyone is watching");

        // And it may not end a run it never owned.
        watcher.finish();
        assert!(
            shared.lock().unwrap().running,
            "the full run is still going"
        );
        full.finish();
        assert!(!shared.lock().unwrap().running);
    }

    /// A run that follows a finished one is a different run, and says so, so a
    /// poller redraws instead of measuring new counts against an old total.
    #[test]
    fn each_run_gets_its_own_number() {
        let shared = Arc::new(Mutex::new(IndexProgress::default()));

        let first = SharedProgress::new(shared.clone());
        first.start(647);
        first.finish();
        assert_eq!(shared.lock().unwrap().run, 1);

        let second = SharedProgress::new(shared.clone());
        second.start(788);
        let p = shared.lock().unwrap().clone();
        assert_eq!(p.run, 2);
        assert_eq!(p.total, 788);
        assert_eq!(p.done, 0);
    }

    // --- PLAN-008 TASK-009 fixup D1: idle exemption, cancellation, vanishing ---

    fn lifecycle_state(tag: &str) -> (AppState, PathBuf) {
        let root =
            std::env::temp_dir().join(format!("devctx_mcp_life_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".devctx/state")).unwrap();
        let mut cfg = ProjectConfig::default();
        cfg.project.path = root.to_string_lossy().into_owned();
        cfg.state_dir = root.join(".devctx/state").to_string_lossy().into_owned();
        (AppState::build(cfg).unwrap(), root)
    }

    fn set_progress(state: &AppState, running: bool, advanced_ago: Duration) {
        let mut p = state.index_progress.lock().unwrap();
        p.running = running;
        p.advanced = Instant::now().checked_sub(advanced_ago);
    }

    /// Item 7: the idle exemption is about *progress*. A run whose last sign of
    /// life is older than the stall limit (900 s by default) is stuck and must
    /// not hold the idle timer off; one that moved recently must.
    #[test]
    fn only_an_index_that_moved_recently_is_exempt_from_idle() {
        let (state, root) = lifecycle_state("exempt");
        assert!(!state.is_indexing(), "nothing running");

        set_progress(&state, true, Duration::from_secs(5));
        assert!(state.is_indexing(), "recent progress exempts the server");

        set_progress(&state, true, Duration::from_secs(901));
        assert!(
            !state.is_indexing(),
            "a run silent for longer than the stall limit is stuck"
        );

        set_progress(&state, false, Duration::from_secs(1));
        assert!(!state.is_indexing(), "a finished run exempts nothing");
        drop(state);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Item 6: phases after the file loop (prune, HNSW, FTS, checkpoint) and the
    /// model load report themselves, which is what keeps the exemption alive
    /// through them.
    #[test]
    fn a_phase_report_renews_the_exemption() {
        let (state, root) = lifecycle_state("phase");
        let sink =
            SharedProgress::with_cancel(state.index_progress.clone(), state.index_cancel.clone());
        sink.begin(LOADING_MODEL);
        assert!(
            state.is_indexing(),
            "a run is visible before its model loads"
        );
        assert_eq!(state.index_progress.lock().unwrap().phase, "loading model");

        sink.start(3);
        {
            let p = state.index_progress.lock().unwrap();
            assert_eq!((p.run, p.total), (1, 3), "begin + start is one run");
        }
        set_progress(&state, true, Duration::from_secs(901));
        assert!(!state.is_indexing());
        sink.phase("hnsw");
        assert!(state.is_indexing(), "the HNSW phase counts as progress");
        assert!(state.index_winding_down(Duration::from_secs(15)));
        assert_eq!(state.index_progress.lock().unwrap().phase, "hnsw");
        sink.finish();
        assert!(!state.index_running());
        drop(state);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Item 1: a stop request reaches the run through its sink.
    #[test]
    fn cancelling_reaches_every_run_through_its_sink() {
        let (state, root) = lifecycle_state("cancel");
        let sink =
            SharedProgress::with_cancel(state.index_progress.clone(), state.index_cancel.clone());
        assert!(!sink.cancelled());
        state.cancel_indexing();
        assert!(sink.cancelled());
        drop(state);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Item 4: only repeated, definite "not found" means the project is gone.
    #[test]
    fn a_project_counts_as_vanished_only_after_repeated_not_found() {
        let (state, root) = lifecycle_state("vanish");
        assert!(!state.project_vanished());
        let devctx = root.join(".devctx");
        let aside = root.join(".devctx-renamed");
        std::fs::rename(&devctx, &aside).unwrap();
        assert!(!state.project_vanished(), "one miss is not enough");
        assert!(!state.project_vanished(), "two misses are not enough");
        // Back before the third poll (a rename in progress): the count resets.
        std::fs::rename(&aside, &devctx).unwrap();
        assert!(!state.project_vanished());
        std::fs::rename(&devctx, &aside).unwrap();
        for _ in 1..VANISH_STRIKES {
            assert!(!state.project_vanished());
        }
        assert!(state.project_vanished(), "{VANISH_STRIKES} misses in a row");
        drop(state);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Item 4: an unreadable path (EACCES, as drvfs/9p can report transiently)
    /// is not a missing one.
    #[cfg(unix)]
    #[test]
    fn a_permission_error_is_not_a_missing_directory() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("devctx_mcp_eacces_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("locked/.devctx")).unwrap();
        std::fs::set_permissions(dir.join("locked"), std::fs::Permissions::from_mode(0o000))
            .unwrap();
        let probe = dir.join("locked/.devctx");
        let denied = std::fs::metadata(&probe)
            .err()
            .is_some_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied);
        let missing = super::dir_definitely_missing(&probe);
        std::fs::set_permissions(dir.join("locked"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        if denied {
            assert!(!missing, "EACCES must not read as deleted");
        }
        assert!(super::dir_definitely_missing(&dir.join("nope")));
    }

    // --- plan_status (TASK-002) ---

    fn write_plan_fixture(root: &std::path::Path) {
        let older = root.join("plans/PLAN-001-old");
        let newer = root.join("plans/PLAN-002-new");
        std::fs::create_dir_all(older.join("tasks")).unwrap();
        std::fs::create_dir_all(newer.join("tasks")).unwrap();

        std::fs::write(
            older.join("PLAN-001-old.md"),
            "# PLAN-001 — todo terminado\n\n| Task | Estado |\n|---|---|\n| TASK-001 | `done` |\n",
        )
        .unwrap();
        std::fs::write(
            older.join("tasks/TASK-001-x.md"),
            "# TASK-001 — x\n\n- **Estado:** `done`\n\n## Objetivo\n",
        )
        .unwrap();

        std::fs::write(
            newer.join("PLAN-002-new.md"),
            "# PLAN-002 — con pendientes\n\n| Task | Estado |\n|---|---|\n\
             | TASK-001 | `pending` |\n| TASK-002 | `pending` |\n",
        )
        .unwrap();
        std::fs::write(
            newer.join("tasks/TASK-001-a.md"),
            "# TASK-001 — a\n\n- **Depende de:** —\n- **Estado:** `pending`\n\n## Objetivo\n",
        )
        .unwrap();
        std::fs::write(
            newer.join("tasks/TASK-002-b.md"),
            "# TASK-002 — b\n\n- **Depende de:** TASK-001\n- **Estado:** `pending`\n\n## Objetivo\n",
        )
        .unwrap();

        // Make the older plan's files strictly older than the newer plan's, even though
        // "older" is only true by mtime here (both were just written) — set the older
        // plan's mtimes back to be sure the test exercises the mtime rule and not luck.
        let past = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        for f in [
            older.join("PLAN-001-old.md"),
            older.join("tasks/TASK-001-x.md"),
        ] {
            let file = std::fs::File::open(&f).unwrap();
            file.set_modified(past).unwrap();
        }
    }

    #[test]
    fn active_plan_is_the_newest_with_pending_tasks_even_if_another_plan_is_newer_and_done() {
        let root =
            std::env::temp_dir().join(format!("devctx_plan_status_active_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        write_plan_fixture(&root);

        let value = plan_status_value(&root, None).expect("list should succeed");
        assert_eq!(value["active"].as_str(), Some("PLAN-002"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn blocked_waiting_on_names_exactly_the_pending_deps() {
        let root =
            std::env::temp_dir().join(format!("devctx_plan_status_blocked_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        write_plan_fixture(&root);

        let value = plan_status_value(&root, Some("PLAN-002")).expect("detail should succeed");
        assert_eq!(value["plan"].as_str(), Some("PLAN-002"));
        let ready: Vec<&str> = value["ready"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["id"].as_str().unwrap())
            .collect();
        assert_eq!(ready, vec!["TASK-001"]);
        let blocked = value["blocked"].as_array().unwrap();
        assert_eq!(blocked.len(), 1);
        assert_eq!(blocked[0]["id"].as_str(), Some("TASK-002"));
        assert_eq!(
            blocked[0]["waiting_on"].as_array().unwrap(),
            &vec![serde_json::json!("TASK-001")]
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn budget_of_fifty_tokens_on_a_forty_task_plan_keeps_ready_and_reports_omitted() {
        let root =
            std::env::temp_dir().join(format!("devctx_plan_status_budget_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("plans/PLAN-009-grande/tasks");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            root.join("plans/PLAN-009-grande/PLAN-009-grande.md"),
            "# PLAN-009 — grande\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("TASK-001-ready.md"),
            "# TASK-001 — ready\n\n- **Depende de:** —\n- **Estado:** `pending`\n\n## Objetivo\n",
        )
        .unwrap();
        for n in 2..=40 {
            let body = format!(
                "# TASK-{n:03} — bloqueada\n\n- **Depende de:** TASK-001\n- **Estado:** `pending`\n\n## Objetivo\n"
            );
            std::fs::write(dir.join(format!("TASK-{n:03}-x.md")), body).unwrap();
        }

        let value = plan_status_value(&root, Some("PLAN-009")).unwrap();
        let budgeted = fit_plan_status_budget(value, 50);
        let parsed: Value = serde_json::from_str(&budgeted).unwrap();
        assert!(parsed.get("omitted").is_some(), "{budgeted}");
        assert!(
            !parsed["ready"].as_array().unwrap().is_empty(),
            "ready must survive the cut: {budgeted}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_repo_without_plans_dir_gives_an_empty_list_not_an_error() {
        let root =
            std::env::temp_dir().join(format!("devctx_plan_status_none_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let value = plan_status_value(&root, None).expect("no plans/ dir is not an error");
        assert!(value["plans"].as_array().unwrap().is_empty());
        assert!(value["active"].is_null());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn plan_status_of_a_group_member_reads_the_workspace_plans_and_says_so() {
        let home =
            std::env::temp_dir().join(format!("devctx_plan_status_ws_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let ws = home.join("ws");
        write_plan_fixture(&ws);
        let member = ws.join("api");
        std::fs::create_dir_all(&member).unwrap();

        let resolved = plans::resolve_plans_root(Some(&member), None, true, Some(&home));
        let value = plan_status_value_in(&resolved, None).expect("list should succeed");
        assert_eq!(value["plans"].as_array().unwrap().len(), 2);
        assert_eq!(value["plans_root"]["source"], "ancestor");
        assert_eq!(value["plans_root"]["path"], ws.to_string_lossy().as_ref());

        let detail = plan_status_value_in(&resolved, Some("PLAN-002")).unwrap();
        assert_eq!(detail["plans_root"]["source"], "ancestor");

        // The path-only wrapper keeps working and reports `project`.
        let legacy = plan_status_value(&ws, None).unwrap();
        assert_eq!(legacy["plans_root"]["source"], "project");

        let _ = std::fs::remove_dir_all(&home);
    }

    // --- plan_graph (TASK-006) ---

    #[test]
    fn plan_graph_has_a_node_per_task_and_an_edge_per_dependency() {
        let root = std::env::temp_dir().join(format!("devctx_plan_graph_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        write_plan_fixture(&root); // PLAN-001 (done, 1 task), PLAN-002 (2 tasks, 1 dep edge)

        let raw = plan_graph_value(&root, Some("PLAN-002")).expect("graph should succeed");
        let value: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(value["plan"].as_str(), Some("PLAN-002"));
        assert_eq!(value["nodes"].as_array().unwrap().len(), 2);
        assert_eq!(value["edges"].as_array().unwrap().len(), 1);
        let edge = &value["edges"][0]["data"];
        assert_eq!(edge["source"].as_str(), Some("TASK-001"));
        assert_eq!(edge["target"].as_str(), Some("TASK-002"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn plan_graph_marks_a_missing_dependency_as_a_node_instead_of_dropping_it() {
        let root =
            std::env::temp_dir().join(format!("devctx_plan_graph_missing_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("plans/PLAN-003-x/tasks");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            root.join("plans/PLAN-003-x/PLAN-003-x.md"),
            "# PLAN-003 — x\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("TASK-001-a.md"),
            "# TASK-001 — a\n\n- **Depende de:** TASK-099\n- **Estado:** `pending`\n\n## Objetivo\n",
        )
        .unwrap();

        let raw = plan_graph_value(&root, Some("PLAN-003")).unwrap();
        let value: Value = serde_json::from_str(&raw).unwrap();
        let nodes = value["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 2, "TASK-001 plus the missing TASK-099 node");
        let missing = nodes
            .iter()
            .find(|n| n["data"]["id"] == "TASK-099")
            .unwrap();
        assert_eq!(missing["data"]["status"].as_str(), Some("missing"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn plan_graph_on_a_plan_with_no_task_files_says_so_without_an_empty_canvas() {
        let root =
            std::env::temp_dir().join(format!("devctx_plan_graph_empty_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("plans/PLAN-004-sin-tasks");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("PLAN-004-sin-tasks.md"),
            "# PLAN-004 — sin tasks\n",
        )
        .unwrap();

        let raw = plan_graph_value(&root, Some("PLAN-004")).unwrap();
        let value: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(value["nodes"].as_array().unwrap().len(), 0);
        assert_eq!(value["message"].as_str(), Some("sin tasks"));

        let _ = std::fs::remove_dir_all(&root);
    }

    // --- plan_tasks_for_file (TASK-007) ---

    fn write_file_ref_fixture(root: &std::path::Path) {
        let dir = root.join("plans/PLAN-002-x/tasks");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            root.join("plans/PLAN-002-x/PLAN-002-x.md"),
            "# PLAN-002 — x\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("TASK-001-a.md"),
            "# TASK-001 — a\n\n- **Depende de:** —\n- **Estado:** `pending`\n\n## Archivos\n\n\
             - **Modificar:** `crates/a/src/x.rs:10`\n\n\
             Referencia pelada: `lib.rs`.\n",
        )
        .unwrap();
    }

    #[test]
    fn a_full_path_reference_with_a_line_matches_the_same_full_path() {
        let root =
            std::env::temp_dir().join(format!("devctx_plan_tasks_file_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        write_file_ref_fixture(&root);

        let hits = plan_tasks_for_file(&root, "crates/a/src/x.rs", None);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["task"].as_str(), Some("TASK-001"));
        assert_eq!(hits[0]["line"].as_u64(), Some(10));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_bare_lib_rs_reference_does_not_match_a_full_path_query() {
        let root =
            std::env::temp_dir().join(format!("devctx_plan_tasks_bare_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        write_file_ref_fixture(&root);

        let hits = plan_tasks_for_file(&root, "crates/a/src/lib.rs", None);
        assert!(
            hits.is_empty(),
            "a bare `lib.rs` must not match a full path: {hits:?}"
        );

        // But it matches an equally bare query.
        let hits = plan_tasks_for_file(&root, "lib.rs", None);
        assert_eq!(hits.len(), 1);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn with_plan_tasks_field_adds_the_field_without_disturbing_the_rest() {
        let original = r#"{"subject":"x.rs","memories":[],"matched_by":"junction"}"#.to_string();
        let out = with_plan_tasks_field(
            original,
            vec![json!({"plan": "PLAN-002", "task": "TASK-001"})],
        );
        let value: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(value["subject"].as_str(), Some("x.rs"));
        assert_eq!(value["plan_tasks"].as_array().unwrap().len(), 1);
    }

    // --- TASK-001 (PLAN-006): output budget on the tools that lacked it ---

    fn hit(file: &str, line: i64, text_len: usize) -> Value {
        json!({ "file": file, "start_line": line, "text": "x".repeat(text_len) })
    }

    /// Under budget, `fit_json_array` is a no-op: nothing is touched, nothing
    /// is reported missing — same contract as `fit_memories`.
    #[test]
    fn fit_json_array_under_budget_returns_everything_untouched() {
        let items = vec![hit("a.rs", 1, 50), hit("b.rs", 2, 50)];
        let (kept, dropped) = fit_json_array(items, 8000, Some("text"), |_| String::new());
        assert_eq!(kept.len(), 2);
        assert!(dropped.is_empty());
    }

    /// An oversized item with a shrinkable field is truncated in place, not
    /// dropped whole — the same "shrink before you drop" rule as memories.
    #[test]
    fn fit_json_array_truncates_an_oversized_item_in_place() {
        let items = vec![hit("huge.rs", 1, 200_000)];
        let (kept, dropped) = fit_json_array(items, 100, Some("text"), |v| {
            v["file"].as_str().unwrap().to_string()
        });
        assert!(dropped.is_empty());
        let text = kept[0]["text"].as_str().unwrap();
        assert!(text.contains("[devctx] truncated"), "{text:?}");
    }

    /// With no shrinkable field, an item bigger than its share is dropped and
    /// named under `omitted_for_budget` — silence about what was cut is never
    /// acceptable. A tiny item alongside it survives untouched.
    #[test]
    fn fit_json_array_drops_and_names_items_it_cannot_shrink() {
        let items = vec![
            json!({ "framework": "x", "method": "GET", "path": "/ping" }),
            json!({ "framework": "x", "method": "GET", "path": "/".to_string() + &"x".repeat(2000) }),
        ];
        let label = |v: &Value| {
            format!(
                "{} {}",
                v["method"].as_str().unwrap(),
                v["path"].as_str().unwrap()
            )
        };
        let (kept, dropped) = fit_json_array(items, 20, None, label);
        assert_eq!(kept.len(), 1, "the small route survives: {kept:?}");
        assert_eq!(kept[0]["path"], "/ping");
        assert_eq!(dropped.len(), 1);
        assert!(dropped[0].starts_with("GET /xxx"), "{dropped:?}");
    }

    /// `routes_to_json` (feeds `search_routes`/`routes_for_handler`) wraps its
    /// answer with `omitted_for_budget` when a huge result set does not fit,
    /// and stays a bare array otherwise — never silent truncation.
    #[test]
    fn routes_to_json_reports_omissions_under_a_tiny_budget() {
        // Many small routes plus one with a wildly long path: the small ones
        // fit their (floor-protected) share, the long one does not and has
        // no field this tool knows how to shrink, so it alone is dropped.
        std::env::set_var("DEVCTX_MAX_OUTPUT_TOKENS", "20");
        let mut routes: Vec<devctx_store::StoredRoute> = (0..5)
            .map(|i| devctx_store::StoredRoute {
                framework: "fastapi".into(),
                http_method: "GET".into(),
                path: format!("/r{i}"),
                file: "app.py".into(),
                line: i,
                ..Default::default()
            })
            .collect();
        routes.push(devctx_store::StoredRoute {
            framework: "fastapi".into(),
            http_method: "GET".into(),
            path: "/".to_string() + &"segment/".repeat(200),
            file: "app.py".into(),
            line: 999,
            ..Default::default()
        });
        let out = routes_to_json(&routes, &on_current_branch()).unwrap();
        std::env::remove_var("DEVCTX_MAX_OUTPUT_TOKENS");
        let value: Value = serde_json::from_str(&out).unwrap();
        let omitted = value["omitted_for_budget"]["count"].as_u64().unwrap();
        assert_eq!(
            omitted, 1,
            "only the oversized route should be dropped: {out}"
        );
        assert_eq!(value["routes"].as_array().unwrap().len(), 5);
    }

    /// The same call under a generous budget stays the plain array shape the
    /// tool has always returned — the wrapper only appears when it is needed.
    #[test]
    fn routes_to_json_stays_a_bare_array_when_everything_fits() {
        let routes = vec![devctx_store::StoredRoute {
            framework: "flask".into(),
            http_method: "GET".into(),
            path: "/ping".into(),
            file: "app.py".into(),
            line: 1,
            ..Default::default()
        }];
        let out = routes_to_json(&routes, &on_current_branch()).unwrap();
        let value: Value = serde_json::from_str(&out).unwrap();
        assert!(value.is_array(), "{out}");
    }

    /// `do_impact`'s upstream/downstream split each get half the budget, so a
    /// symbol with a huge blast radius on one side does not starve the other:
    /// a mix of small and very deep (long-symbol) callers on one side is
    /// trimmed by dropping the ones that do not fit their share, while a
    /// handful of small callees on the other side is untouched.
    #[test]
    fn fit_json_array_splits_a_shared_budget_fairly_between_two_lists() {
        let mut upstream: Vec<Value> = (0..5)
            .map(|i| json!({ "symbol": format!("Caller{i}"), "depth": 1 }))
            .collect();
        upstream.push(json!({ "symbol": "x".repeat(2000), "depth": 1 }));
        let downstream: Vec<Value> = (0..3)
            .map(|i| json!({ "symbol": format!("Callee{i}"), "depth": 1 }))
            .collect();
        let label = |v: &Value| v["symbol"].as_str().unwrap().to_string();
        let half = 20usize;
        let (up_kept, up_dropped) = fit_json_array(upstream, half, None, label);
        let (down_kept, down_dropped) = fit_json_array(downstream, half, None, label);
        assert_eq!(up_dropped.len(), 1, "only the oversized caller is dropped");
        assert_eq!(up_kept.len(), 5);
        assert!(down_dropped.is_empty(), "3 small callees fit easily");
        assert_eq!(down_kept.len(), 3);
    }

    /// `memories_by_symbol`/`memories_by_file` (via `linked_response`) reuse
    /// `fit_memories`: an oversized linked memory is truncated, not dropped
    /// whole, and the JSON keeps its `subject`/`matched_by` shape either way.
    #[test]
    fn linked_response_style_output_budgets_an_oversized_memory() {
        let out = vec![mem("m1", 200_000)];
        let (kept, dropped) = fit_memories(out, 200, |_, _| None);
        assert!(dropped.is_empty());
        let content = kept[0]["content"].as_str().unwrap();
        assert!(content.contains("[devctx] truncated"), "{content:?}");
    }

    fn on_current_branch() -> BranchChoice {
        BranchChoice {
            repo: "demo".into(),
            branch: "main".into(),
            fallback: None,
            indexed: true,
            extractor_stale: false,
        }
    }

    // --- graph branch fallback (PLAN-008 TASK-006) ---

    const GRAPH_DIM: usize = 3;
    const REPO_PATH: &str = "/tmp/devctx-graph-branch-demo";

    /// A store indexed only on `main`: one definition, one call edge, one route.
    fn store_indexed_on(branch: &str) -> Store {
        let store = Store::open_in_memory(GRAPH_DIM).unwrap();
        store
            .set_index_meta(
                REPO_PATH,
                branch,
                devctx_store::EXTRACTOR_META_KEY,
                &devctx_index::extractor_fingerprint(),
            )
            .unwrap();
        store
            .upsert(&[VectorPoint {
                id: "p1".into(),
                vector: vec![0.0; GRAPH_DIM],
                text: "fn greet() {}".into(),
                metadata: VectorMetadata {
                    repo: "demo".into(),
                    branch: branch.into(),
                    file: "a.rs".into(),
                    symbol: "greet".into(),
                    symbol_type: "function".into(),
                    language: "rust".into(),
                    start_line: 1,
                    end_line: 1,
                    ..Default::default()
                },
            }])
            .unwrap();
        store
            .replace_file_edges(
                "demo",
                branch,
                "b.rs",
                &[devctx_store::StoredEdge {
                    source: "main_fn".into(),
                    target: "greet".into(),
                    kind: "calls".into(),
                    source_file: "b.rs".into(),
                    line: 3,
                }],
            )
            .unwrap();
        store
            .replace_file_routes(
                "demo",
                branch,
                "r.rs",
                &[devctx_store::StoredRoute {
                    framework: "axum".into(),
                    http_method: "GET".into(),
                    path: "/hello".into(),
                    handler_class: String::new(),
                    handler_method: "greet".into(),
                    handler_symbol: "greet".into(),
                    file: "r.rs".into(),
                    line: 1,
                }],
                "2026-10-03T00:00:00Z",
            )
            .unwrap();
        store
            .save_index_record(&devctx_store::IndexRecord {
                repo_path: REPO_PATH.into(),
                branch: branch.into(),
                last_commit: "abc".into(),
                model_name: "m".into(),
                model_dimension: GRAPH_DIM as i64,
                file_count: 1,
                symbol_count: 1,
                chunk_count: 1,
                indexed_at: "2026-10-03T00:00:00Z".into(),
            })
            .unwrap();
        store
    }

    #[test]
    fn a_branch_nobody_indexed_answers_the_graph_from_the_indexed_one() {
        let store = store_indexed_on("main");
        let c = pick_graph_branch(&store, "demo", REPO_PATH, "feat/x", None);
        assert!(c.indexed);
        assert_eq!(c.branch, "main");
        let f = c.fallback.as_ref().expect("a fallback must be reported");
        assert_eq!((f.current.as_str(), f.used.as_str()), ("feat/x", "main"));

        // Every graph query, run on the chosen branch, finds what `main` has.
        let defs = store
            .symbol_definitions(&c.repo, &c.branch, "greet", 5)
            .unwrap();
        assert_eq!(defs.len(), 1);
        let refs = store.find_references(&c.repo, &c.branch, "greet").unwrap();
        assert_eq!(refs.len(), 1);
        let routes = store
            .search_routes(&c.repo, &c.branch, None, Some("hello"))
            .unwrap();
        assert_eq!(routes.len(), 1);
        let impact = store
            .impact_analysis(&c.repo, &c.branch, "greet", 2)
            .unwrap();
        assert!(!impact.upstream.is_empty());

        // And the output says so.
        let mut out = json!({ "symbol": "greet" });
        c.annotate(&mut out);
        assert_eq!(out["branch_fallback"]["current"], "feat/x");
        assert_eq!(out["branch_fallback"]["used"], "main");
        assert!(out["branch_fallback"]["why"]
            .as_str()
            .unwrap()
            .contains("feat/x is not indexed"));
        let routes_out = routes_to_json(&routes, &c).unwrap();
        assert!(routes_out.contains("branch_fallback"), "{routes_out}");
    }

    #[test]
    fn the_configured_default_wins_over_the_most_recent() {
        let store = store_indexed_on("main");
        let c = pick_graph_branch(&store, "demo", REPO_PATH, "feat/x", Some("main"));
        assert_eq!(c.branch, "main");
        assert!(c.fallback.is_some());
        // A default with no rows is skipped, not trusted.
        let c = pick_graph_branch(&store, "demo", REPO_PATH, "feat/x", Some("develop"));
        assert_eq!(c.branch, "main");
    }

    #[test]
    fn the_indexed_current_branch_adds_no_field() {
        let store = store_indexed_on("main");
        let c = pick_graph_branch(&store, "demo", REPO_PATH, "main", Some("other"));
        assert_eq!(c.branch, "main");
        assert!(c.fallback.is_none());
        let mut out = json!({ "symbol": "greet" });
        c.annotate(&mut out);
        assert!(out.get("branch_fallback").is_none());
        let routes = store.search_routes("demo", "main", None, None).unwrap();
        assert!(
            routes_to_json(&routes, &c)
                .unwrap()
                .trim_start()
                .starts_with('['),
            "shape unchanged when the current branch is indexed"
        );
    }

    #[test]
    fn an_index_without_extractor_record_warns_in_every_graph_answer() {
        let store = store_indexed_on("main");
        // An index stamped by some other extractor (a missing record is the
        // same answer, covered in the store's own test).
        store
            .set_index_meta(
                REPO_PATH,
                "main",
                devctx_store::EXTRACTOR_META_KEY,
                "v0-old",
            )
            .unwrap();
        let c = pick_graph_branch(&store, "demo", REPO_PATH, "main", None);
        assert!(c.extractor_stale);
        let mut out = json!({ "symbol": "greet" });
        c.annotate(&mut out);
        assert!(out["warning"].as_str().unwrap().contains("older extractor"));
        let routes = store.search_routes("demo", "main", None, None).unwrap();
        let routed = routes_to_json(&routes, &c).unwrap();
        assert!(routed.contains("older extractor"), "{routed}");

        store
            .set_index_meta(
                REPO_PATH,
                "main",
                devctx_store::EXTRACTOR_META_KEY,
                &devctx_index::extractor_fingerprint(),
            )
            .unwrap();
        let fresh = pick_graph_branch(&store, "demo", REPO_PATH, "main", None);
        assert!(!fresh.extractor_stale);
        let mut out = json!({});
        fresh.annotate(&mut out);
        assert!(out.get("warning").is_none());
    }

    #[test]
    fn an_empty_store_is_an_explicit_error_not_an_empty_answer() {
        let store = Store::open_in_memory(GRAPH_DIM).unwrap();
        let c = pick_graph_branch(&store, "demo", REPO_PATH, "main", None);
        assert!(!c.indexed);
        let err = c
            .ready()
            .expect_err("nothing indexed must not look like []");
        assert!(err.contains("demo has no index for any branch"), "{err}");
        assert!(err.contains("devctx index"), "{err}");
    }

    #[test]
    fn the_most_recently_indexed_branch_is_the_last_resort() {
        let store = store_indexed_on("main");
        // A second branch, indexed later.
        store
            .upsert(&[VectorPoint {
                id: "p2".into(),
                vector: vec![0.0; GRAPH_DIM],
                text: "fn other() {}".into(),
                metadata: VectorMetadata {
                    repo: "demo".into(),
                    branch: "develop".into(),
                    file: "c.rs".into(),
                    symbol: "other".into(),
                    ..Default::default()
                },
            }])
            .unwrap();
        store
            .save_index_record(&devctx_store::IndexRecord {
                repo_path: REPO_PATH.into(),
                branch: "develop".into(),
                last_commit: "def".into(),
                model_name: "m".into(),
                model_dimension: GRAPH_DIM as i64,
                file_count: 1,
                symbol_count: 1,
                chunk_count: 1,
                indexed_at: "2026-10-04T00:00:00Z".into(),
            })
            .unwrap();
        let c = pick_graph_branch(&store, "demo", REPO_PATH, "feat/x", None);
        assert_eq!(c.branch, "develop");
        assert_eq!(
            store.branches_by_recency(REPO_PATH).unwrap(),
            vec!["develop".to_string(), "main".to_string()]
        );
    }

    // --- repo_path key and branch rule shared by search and graph (review fixup) ---

    fn sh(dir: &std::path::Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }

    /// A project living in `<repo>/sub` (git's toplevel is `<repo>`), checked out
    /// on `feat/x`, whose store was indexed on `main` the way the pipeline does:
    /// keyed by the git toplevel.
    fn subdir_project(tag: &str) -> (AppState, PathBuf) {
        let repo =
            std::env::temp_dir().join(format!("devctx_mcp_subdir_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&repo);
        std::fs::create_dir_all(repo.join("sub")).unwrap();
        sh(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("sub/a.rs"), "pub fn greet() {}\n").unwrap();
        sh(&repo, &["add", "-A"]);
        sh(&repo, &["commit", "-q", "-m", "init"]);
        sh(&repo, &["checkout", "-q", "-b", "feat/x"]);

        let mut cfg = ProjectConfig::default();
        cfg.project.path = repo.join("sub").to_string_lossy().into_owned();
        cfg.state_dir = repo.join("state").to_string_lossy().into_owned();
        let state = AppState::build(cfg).unwrap();

        let git = GitRepo::open(&repo).unwrap();
        let repo_path = git.root().to_string_lossy().into_owned();
        let store = state.open_store().unwrap();
        let dim = configured_dimension(&state.cfg);
        store
            .set_index_meta(
                &repo_path,
                "main",
                devctx_store::EXTRACTOR_META_KEY,
                &devctx_index::extractor_fingerprint(),
            )
            .unwrap();
        store
            .upsert(&[VectorPoint {
                id: "p1".into(),
                vector: vec![0.0; dim],
                text: "pub fn greet() {}".into(),
                metadata: VectorMetadata {
                    repo: git.short_name(),
                    branch: "main".into(),
                    file: "sub/a.rs".into(),
                    symbol: "greet".into(),
                    symbol_type: "function".into(),
                    language: "rust".into(),
                    start_line: 1,
                    end_line: 1,
                    ..Default::default()
                },
            }])
            .unwrap();
        store
            .save_index_record(&devctx_store::IndexRecord {
                repo_path,
                branch: "main".into(),
                last_commit: "abc".into(),
                model_name: "m".into(),
                model_dimension: dim as i64,
                file_count: 1,
                symbol_count: 1,
                chunk_count: 1,
                indexed_at: "2026-10-03T00:00:00Z".into(),
            })
            .unwrap();
        (state, repo)
    }

    #[test]
    fn a_project_in_a_repo_subdirectory_keys_the_store_by_the_git_toplevel() {
        let (state, repo) = subdir_project("key");
        assert_ne!(state.root, repo, "the project is not the toplevel");
        assert_eq!(
            state.repo_path(),
            GitRepo::open(&repo).unwrap().root().to_string_lossy()
        );
        let store = state.open_store().unwrap();
        // Before the fix this read `state.root`, found no record, and so
        // reported a fresh index as stale, and the recency fallback found nothing.
        let c = graph_branch(&state, &store).expect("main is indexed");
        assert_eq!(c.branch, "main");
        assert!(!c.extractor_stale, "{c:?}");
        assert!(c.fallback.is_some());
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// The project path IS the toplevel, reached through a symlink: not a
    /// subdirectory, so only the symlink can explain a mismatch. `AppState`
    /// does not canonicalize its root; git reports the real toplevel.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_project_path_keys_the_store_by_the_real_toplevel() {
        let (state, repo) = subdir_project("link");
        sh(&repo, &["checkout", "-q", "main"]);
        let link = std::env::temp_dir().join(format!("devctx_mcp_link_{}", std::process::id()));
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&repo, &link).unwrap();
        let mut cfg = state.cfg.clone();
        cfg.project.path = link.to_string_lossy().into_owned();
        drop(state);
        let state = AppState::build(cfg).unwrap();
        assert_eq!(state.root, link, "the root stays the symlink");
        assert_eq!(
            state.repo_path(),
            GitRepo::open(&repo).unwrap().root().to_string_lossy()
        );
        let store = state.open_store().unwrap();
        let c = graph_branch(&state, &store).expect("main is indexed");
        assert_eq!(c.branch, "main");
        assert!(!c.extractor_stale, "{c:?}");
        assert!(c.fallback.is_none(), "{c:?}");
        let _ = std::fs::remove_file(&link);
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// `memories_by_symbol` looks the junction up under the branch that was
    /// indexed, not the checked-out one (`feat/x`, which has nothing).
    #[test]
    fn memories_by_symbol_queries_the_fallback_branch() {
        let (state, repo) = subdir_project("symbranch");
        let store = state.open_store().unwrap();
        let (r, branch, fallback) = symbol_branch(&state, &store);
        assert_eq!(r, GitRepo::open(&repo).unwrap().short_name());
        assert_eq!(branch, "main");
        let f = fallback.expect("the fallback is reported");
        assert_eq!((f.current.as_str(), f.used.as_str()), ("feat/x", "main"));
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn build_context_fallback_note() {
        let raw = r#"{"results": [], "branch_fallback": {"why": "x"}}"#;
        let note = fallback_note(raw).expect("a note");
        assert_eq!(note, "[devctx] branch_fallback: x\n");
        assert_eq!(fallback_note("[]"), None);
        assert_eq!(fallback_note("not json"), None);

        // With nothing to show, the note must not replace "nothing matched".
        let mut out = String::new();
        close_context(&mut out, Some(note));
        let nothing = out.find("nothing indexed matched").expect("{out}");
        let branch = out.find("branch_fallback: x").expect("{out}");
        assert!(nothing < branch, "{out}");
        let mut some = String::from("## Code\n");
        close_context(&mut some, None);
        assert_eq!(some, "## Code\n");
    }

    #[test]
    fn merge_omitted_counts_both_sides_and_keeps_both_item_lists() {
        assert_eq!(merge_omitted(vec![], None), None);
        let child = json!({ "count": 3 });
        assert_eq!(merge_omitted(vec![], Some(&child)), Some(child.clone()));
        let merged = merge_omitted(vec!["a".into()], Some(&child)).unwrap();
        assert_eq!(merged["count"], 4);
        assert_eq!(merged["items"], json!(["a"]));
        let child = json!({ "count": 2, "items": ["x:1", "y:2"] });
        let merged = merge_omitted(vec!["a".into()], Some(&child)).unwrap();
        assert_eq!(merged["count"], 3);
        assert_eq!(merged["items"], json!(["a", "x:1", "y:2"]));
    }

    /// `projects list` reads the totals under the git toplevel; reading them
    /// under the project path shows a monorepo subdirectory as empty.
    #[test]
    fn report_totals_are_read_under_the_git_toplevel() {
        let (state, repo) = subdir_project("totals");
        let store = state.open_store().unwrap();
        store
            .save_file_state(&devctx_store::FileState {
                repo_path: state.repo_path(),
                branch: "main".into(),
                file_path: "sub/a.rs".into(),
                content_hash: "h".into(),
                language: "rust".into(),
                symbol_count: 2,
                chunk_count: 3,
            })
            .unwrap();
        assert_eq!(totals_for_root(&store, &state.root, "main"), (1, 2, 3));
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// A branch with an index record but no rows is "indexed but empty", the
    /// same thing `index_status` says, not "not indexed".
    #[test]
    fn an_indexed_but_empty_current_branch_is_described_as_such() {
        let store = store_indexed_on("main");
        store
            .save_index_record(&devctx_store::IndexRecord {
                repo_path: REPO_PATH.into(),
                branch: "feat/x".into(),
                last_commit: "abc".into(),
                model_name: "m".into(),
                model_dimension: GRAPH_DIM as i64,
                file_count: 0,
                symbol_count: 0,
                chunk_count: 0,
                indexed_at: "2026-10-04T00:00:00Z".into(),
            })
            .unwrap();
        let c = pick_graph_branch(&store, "demo", REPO_PATH, "feat/x", None);
        let why = c.fallback.expect("falls back").to_json()["why"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(why.contains("indexed but empty"), "{why}");
        let never = pick_graph_branch(&store, "demo", REPO_PATH, "feat/y", None);
        let why = never.fallback.unwrap().to_json()["why"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(why.contains("is not indexed"), "{why}");
    }

    #[test]
    fn search_and_the_graph_tools_pick_the_same_branch_and_say_so() {
        let (state, repo) = subdir_project("rule");
        let store = state.open_store().unwrap();
        // No default configured, nothing indexed on `feat/x`: before the fix
        // search ran unfiltered and said nothing.
        let (filter, fallback) = search_branch(&state, &store);
        assert_eq!(filter.branch.as_deref(), Some("main"));
        let f = fallback.expect("search must report the fallback");
        assert_eq!((f.current.as_str(), f.used.as_str()), ("feat/x", "main"));

        let graph: Value =
            serde_json::from_str(&do_graph(&state, None, None, 10, false, false).unwrap()).unwrap();
        assert_eq!(graph["branch"], "main");
        assert_eq!(graph["branch_fallback"]["used"], "main");
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// B2: an index that is both stale (old extractor) and empty reports both
    /// hints; the second used to overwrite the first.
    #[test]
    fn a_stale_and_empty_index_reports_both_hints() {
        let (state, repo) = subdir_project("b2");
        let (_, branch) = state.repo_branch().unwrap();
        {
            let store = state.open_store().unwrap();
            store
                .save_index_record(&devctx_store::IndexRecord {
                    repo_path: state.repo_path(),
                    branch: branch.clone(),
                    last_commit: "abc".into(),
                    model_name: "m".into(),
                    model_dimension: GRAPH_DIM as i64,
                    file_count: 0,
                    symbol_count: 0,
                    chunk_count: 0,
                    indexed_at: "2026-10-04T00:00:00Z".into(),
                })
                .unwrap();
        }
        let v: Value = serde_json::from_str(&do_index_status(&state).unwrap()).unwrap();
        assert_eq!(v["extractor_stale"], true, "{v}");
        assert_eq!(v["empty"], true, "{v}");
        let hint = v["hint"].as_str().unwrap();
        assert!(hint.contains("older extractor"), "{hint}");
        assert!(hint.contains("indexed but empty"), "{hint}");
        let _ = std::fs::remove_dir_all(&repo);
    }

    fn choice(branch: &str, indexed: bool) -> BranchChoice {
        BranchChoice {
            repo: "r".into(),
            branch: branch.into(),
            fallback: None,
            indexed,
            extractor_stale: false,
        }
    }

    /// N3: a repository that was never indexed says so, instead of "branch X
    /// is not indexed; answering from branch X".
    #[test]
    fn a_repository_never_indexed_gets_its_own_message() {
        let hint = no_record_hint(&[], &choice("main", true), "main");
        assert!(hint.contains("nothing indexed yet"), "{hint}");
        assert!(hint.contains("devctx index"), "{hint}");
        let hint = no_record_hint(&[], &choice("main", false), "main");
        assert!(hint.contains("nothing indexed yet"), "{hint}");
        // Rows without a record: not the same sentence as a fallback.
        let hint = no_record_hint(&["dev".into()], &choice("main", true), "main");
        assert!(hint.contains("no completed index record"), "{hint}");
        assert!(!hint.contains("answer from branch main"), "{hint}");
        // A real fallback still names the branch it answers from.
        let hint = no_record_hint(&["dev".into()], &choice("dev", true), "main");
        assert!(hint.contains("answer from branch dev"), "{hint}");
    }
}
