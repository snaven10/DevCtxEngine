//! Shared server state and the blocking tool implementations.
//!
//! The embedder and reranker are built **lazily** on first use (model load is
//! expensive and would otherwise block server startup — e.g. an MCP client's
//! connect handshake times out while the model downloads). The DuckDB store is
//! opened once and cloned per call (a `Connection` is not `Sync`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use devctx_core::config::ProjectConfig;
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
    /// Changes the run expects to process, known once it has diffed.
    pub total: usize,
    /// Changes it has started on. Started, not finished: the sink is called
    /// before each file, so this counts what is under way.
    pub done: usize,
    /// The file it reached last.
    pub file: String,
}

/// Writes an indexing run's progress where a request handler can read it.
struct SharedProgress(Arc<Mutex<IndexProgress>>);

impl SharedProgress {
    /// A poisoned lock must never take an indexing run down with it: this is a
    /// counter for a progress bar, not part of the work. Recover and carry on,
    /// the same way [`AppState::checkpoint`] does.
    fn lock(&self) -> std::sync::MutexGuard<'_, IndexProgress> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Mark the run finished, keeping the final counts for whoever polls last.
    fn finish(&self) {
        self.lock().running = false;
    }
}

impl ProgressSink for SharedProgress {
    fn start(&self, total: usize) {
        let mut p = self.lock();
        p.running = true;
        p.total = total;
        p.done = 0;
        p.file.clear();
    }

    fn file(&self, path: &str) {
        let mut p = self.lock();
        p.done += 1;
        p.file.clear();
        p.file.push_str(path);
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
        };
        let root = if cfg.project.path.is_empty() {
            std::env::current_dir()?
        } else {
            PathBuf::from(&cfg.project.path)
        };
        let dim = configured_dimension(&cfg);
        let primary = Store::open(&cfg.db_path(), dim)?;
        Ok(Self {
            cfg,
            root,
            primary: Arc::new(Mutex::new(primary)),
            embed_settings,
            rerank_settings,
            rerank_enabled,
            embedder: Mutex::new(None),
            reranker: Mutex::new(None),
            index_progress: Arc::new(Mutex::new(IndexProgress::default())),
        })
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

    /// The short repo name + branch (for graph queries), from git.
    fn repo_branch(&self) -> Result<(String, String), String> {
        let git = GitRepo::open(&self.root).map_err(|e| e.to_string())?;
        Ok((git.short_name(), git.state().branch))
    }
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
    let filter = SearchFilter {
        languages: language.into_iter().collect(),
        exclude_deletions: true,
        ..Default::default()
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
    serde_json::to_string_pretty(&hits_to_json(&hits)).map_err(|e| e.to_string())
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
    do_index_inner(state, full, None)
}

/// Index exactly these repo-relative paths — what a file watcher needs, since a
/// save moves no commit and the commit diff would therefore be empty.
pub fn do_index_paths(state: &AppState, paths: &[String]) -> Result<String, String> {
    do_index_inner(state, false, Some(paths))
}

fn do_index_inner(
    state: &AppState,
    full: bool,
    paths: Option<&[String]>,
) -> Result<String, String> {
    let store = state.open_store()?;
    let embedder = state.embedder()?;
    let sink = SharedProgress(state.index_progress.clone());
    let run = index_run(IndexRequest {
        store: &store,
        embedder: embedder.as_ref(),
        repo_root: &state.root,
        incremental: !full,
        model_name: &state.cfg.embeddings.model,
        progress: Some(&sink),
        paths,
        exclude: &state.cfg.indexing.exclude,
    });
    // Before the `?`: a run that fails still has to stop reporting itself as
    // running, or the next poller waits on something that is already over.
    sink.finish();
    let res = run.map_err(|e| e.to_string())?;
    report_index(&store, &state.root, &res);
    Ok(json!({
        "commit": res.commit,
        "branch": res.branch,
        "full_reindex": res.full_reindex,
        "files_indexed": res.files_indexed,
        "files_skipped": res.files_skipped,
        "files_deleted": res.files_deleted,
        "files_pruned": res.files_pruned,
        "files_renamed": res.files_renamed,
        "symbols": res.symbols,
        "chunks": res.chunks,
    })
    .to_string())
}

/// How far the indexing run in this server has got.
///
/// Deliberately cheap: it copies four fields under a short lock and touches no
/// database. It is polled *while* an index is running, so anything heavier
/// would queue behind the very work it reports on and arrive too late to be
/// worth reporting.
impl AppState {
    /// Whether an indexing run is in flight right now.
    ///
    /// The idle watchdog asks before shutting the server down: indexing happens
    /// *inside* the server, so a run whose client has stopped asking about it is
    /// still real work, and exiting would throw away everything it has done.
    pub fn is_indexing(&self) -> bool {
        self.index_progress
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .running
    }
}

pub fn do_index_progress(state: &AppState) -> Result<String, String> {
    let p = state
        .index_progress
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    Ok(json!({
        "running": p.running,
        "total": p.total,
        "done": p.done,
        "file": p.file,
    })
    .to_string())
}

/// `index_status` tool: report the last-indexed record for the repo/branch.
pub fn do_index_status(state: &AppState) -> Result<String, String> {
    let store = state.open_store()?;
    let git = GitRepo::open(&state.root).map_err(|e| e.to_string())?;
    let state_git = git.state();
    let repo_path = git.root().to_string_lossy().to_string();
    let record = store
        .get_index_record(&repo_path, &state_git.branch)
        .map_err(|e| e.to_string())?;
    let value = match record {
        None => json!({ "indexed": false, "branch": state_git.branch }),
        Some(r) => json!({
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
        }),
    };
    Ok(value.to_string())
}

/// `remember` tool: save a memory (deduplicated).
pub fn do_remember(
    state: &AppState,
    content: String,
    title: String,
    memory_type: String,
    topic: String,
    tags: String,
) -> Result<String, String> {
    let store = state.open_store()?;
    let req = RememberRequest {
        title,
        content,
        memory_type,
        project: state.project(),
        topic_key: topic,
        tags,
        now: now_epoch(),
        ..Default::default()
    };
    let embedder = state.embedder()?;
    let res = remember(&store, embedder.as_ref(), &req).map_err(|e| e.to_string())?;
    Ok(json!({
        "status": format!("{:?}", res.status).to_lowercase(),
        "id": res.memory.id,
        "title": res.memory.title,
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

/// `memory_context` tool: the most recent memories for the project (no query).
pub fn do_memory_context(state: &AppState, limit: usize) -> Result<String, String> {
    let store = state.open_store()?;
    let mems = memory_context(&store, &state.project(), limit).map_err(|e| e.to_string())?;
    let arr: Vec<Value> = mems
        .iter()
        .map(|m| {
            json!({
                "id": m.id,
                "title": m.title,
                "type": m.memory_type,
                "tags": m.tags,
                "content": m.content,
                "created_at": m.created_at,
                "updated_at": m.updated_at,
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
    let (repo, branch) = state.repo_branch()?;
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

    Ok(json!({
        "repo": repo,
        "branch": branch,
        "nodes": nodes,
        "edges": out_edges,
    })
    .to_string())
}

/// Tell the registry what an indexing run produced, so `projects list` reflects
/// reality instead of always reading "never indexed".
///
/// Best-effort: a repository need not be registered at all, and a central store
/// that cannot be reached is no reason to fail an index that already succeeded.
pub fn report_index(store: &Store, root: &std::path::Path, res: &devctx_index::IndexResult) {
    let Ok(path) = std::fs::canonicalize(root) else {
        return;
    };
    let repo_path = path.to_string_lossy().into_owned();
    // Totals, not this run's deltas: an incremental run that found nothing
    // changed would otherwise report the project as empty.
    let (files, symbols, chunks) = store
        .index_totals(&repo_path, &res.branch)
        .unwrap_or((0, 0, 0));
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

/// Reach the central store, auto-spawning the daemon if needed.
///
/// The project server must never open the central database itself — DuckDB
/// allows one writing process per file, and that process is the daemon.
fn central() -> Result<devctx_central::CentralClient, String> {
    let paths = devctx_central::CentralPaths::resolve().map_err(|e| e.to_string())?;
    devctx_central::client::ensure(&paths).ok_or_else(|| {
        "no central store daemon and one could not be started; run `devctx serve --central`"
            .to_string()
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

/// What to tell an agent whose server was started outside any project.
///
/// This has to be a tool result, never a startup failure: a server that refuses
/// to start reports itself to the client as a bare transport error, and a bare
/// transport error is the one kind nobody can act on.
pub fn unbound_help(cwd: &std::path::Path) -> String {
    let listed = match central().and_then(|c| c.list(false).map_err(|e| e.to_string())) {
        Ok(rows) if !rows.is_empty() => rows
            .iter()
            .filter_map(|r| {
                let name = r.get("name")?.as_str()?;
                let path = r.get("path").and_then(|v| v.as_str()).unwrap_or("?");
                Some(format!("  · {name} — {path}"))
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Ok(_) => "  (none registered yet — run `devctx init` in a repository)".to_string(),
        Err(e) => format!("  (the registry could not be read: {e})"),
    };
    format!(
        "This MCP server is not bound to a project: it was started in {}, which \
         is not inside a DevCtxEngine repository.\n\nRegistered projects:\n{listed}\n\n\
         Call `use_project` with one of those names to bind this session, or \
         restart the server as `devctx mcp --project <path>`.",
        cwd.display()
    )
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

    let out = std::process::Command::new(std::env::current_exe().map_err(|e| e.to_string())?)
        .args([
            "search",
            query,
            "--limit",
            &limit.to_string(),
            "--format",
            "json",
        ])
        .args(language.iter().flat_map(|l| ["--language", l]))
        .args(match mode {
            "keyword" => vec!["--keyword"],
            "hybrid" => vec!["--hybrid"],
            _ => vec![],
        })
        .current_dir(path)
        .output()
        .map_err(|e| e.to_string())?;

    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(err
            .trim()
            .lines()
            .last()
            .unwrap_or("search failed")
            .to_string());
    }
    let hits: Value = serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())?;
    serde_json::to_string_pretty(&json!({ "project": project, "path": path, "hits": hits }))
        .map_err(|e| e.to_string())
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
    serde_json::to_string_pretty(
        &json!({ "projects": projects, "bound": bound, "update": update }),
    )
    .map_err(|e| e.to_string())
}

/// `remember` with `scope: global` or `scope: group`: store in the shared
/// central memory instead of this project's, so the other repositories — the
/// group's, or every one of them — can recall it.
pub fn do_remember_shared(
    state: &AppState,
    content: &str,
    title: &str,
    memory_type: &str,
    topic: &str,
    tags: &str,
    group: &str,
) -> Result<String, String> {
    let project = state.project();
    let (repo, branch) = state.repo_branch().unwrap_or_default();
    // Provenance must never be blank: a directory that is not a git repo still
    // belongs to a named project, and "which project taught me this" is the
    // whole point of recording it.
    let repo = if repo.is_empty() {
        project.clone()
    } else {
        repo
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
        )
        .map_err(|e| e.to_string())?;
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
            json!({
                "id": h.memory.id, "title": h.memory.title, "content": h.memory.content,
                "type": h.memory.memory_type, "tags": h.memory.tags, "repo": h.memory.repo,
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

/// `impact_analysis` tool: blast radius (transitive callers/callees) of a symbol.
pub fn do_impact(state: &AppState, symbol: &str, depth: usize) -> Result<String, String> {
    let store = state.open_store()?;
    let (repo, branch) = state.repo_branch()?;
    let impact = store
        .impact_analysis(&repo, &branch, symbol, depth)
        .map_err(|e| e.to_string())?;
    let to_json = |v: &[(String, usize)]| -> Vec<Value> {
        v.iter()
            .map(|(s, d)| json!({ "symbol": s, "depth": d }))
            .collect()
    };
    Ok(json!({
        "symbol": symbol,
        "upstream": to_json(&impact.upstream),
        "downstream": to_json(&impact.downstream),
    })
    .to_string())
}

/// `get_references` tool: all call sites of a symbol.
pub fn do_references(state: &AppState, symbol: &str) -> Result<String, String> {
    let store = state.open_store()?;
    let (repo, branch) = state.repo_branch()?;
    let refs = store
        .find_references(&repo, &branch, symbol)
        .map_err(|e| e.to_string())?;
    let arr: Vec<Value> = refs
        .iter()
        .map(|r| json!({ "file": r.file, "line": r.line, "source": r.source }))
        .collect();
    serde_json::to_string_pretty(&Value::Array(arr)).map_err(|e| e.to_string())
}

/// `search_routes` tool: find HTTP routes by optional method + path substring.
pub fn do_search_routes(
    state: &AppState,
    method: Option<String>,
    path: Option<String>,
) -> Result<String, String> {
    let store = state.open_store()?;
    let (repo, branch) = state.repo_branch()?;
    let routes = store
        .search_routes(&repo, &branch, method.as_deref(), path.as_deref())
        .map_err(|e| e.to_string())?;
    routes_to_json(&routes)
}

/// `routes_for_handler` tool: routes served by a handler symbol.
pub fn do_routes_for_handler(state: &AppState, handler: &str) -> Result<String, String> {
    let store = state.open_store()?;
    let (repo, branch) = state.repo_branch()?;
    let routes = store
        .routes_for_handler(&repo, &branch, handler)
        .map_err(|e| e.to_string())?;
    routes_to_json(&routes)
}

fn routes_to_json(routes: &[devctx_store::StoredRoute]) -> Result<String, String> {
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
    serde_json::to_string_pretty(&Value::Array(arr)).map_err(|e| e.to_string())
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
}
