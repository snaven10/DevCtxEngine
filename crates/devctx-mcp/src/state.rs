//! Shared server state and the blocking tool implementations.
//!
//! The embedder and reranker are built **lazily** on first use (model load is
//! expensive and would otherwise block server startup — e.g. an MCP client's
//! connect handshake times out while the model downloads). The DuckDB store is
//! opened once and cloned per call (a `Connection` is not `Sync`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use devctx_core::config::ProjectConfig;
use devctx_core::{SearchFilter, SearchResult};
use devctx_embed::{create_provider, EmbedSettings, EmbeddingProvider};
use devctx_index::{run as index_run, GitRepo, IndexRequest};
use devctx_memory::{memory_context, memory_stats, recall, remember, RecallQuery, RememberRequest};
use devctx_rerank::{create_reranker, RerankSettings, Reranker};
use devctx_search::SearchMode;
use devctx_store::Store;
use devctx_summarize::{create_summarizer, SummarizeSettings};
use serde_json::{json, Value};

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
    /// Built on first use (see [`AppState::embedder`]).
    embedder: Mutex<Option<Arc<dyn EmbeddingProvider>>>,
    /// Built on first use (see [`AppState::reranker`]).
    reranker: Mutex<Option<Arc<dyn Reranker>>>,
}

impl AppState {
    /// Build state from a project config. Cheap: opens the store (dimension read
    /// from the registry) but does **not** load any model — those come lazily.
    pub fn build(cfg: ProjectConfig) -> anyhow::Result<Self> {
        let embed_settings = EmbedSettings::from_config(&cfg.embeddings);
        let rerank_enabled = cfg.reranking.enabled;
        let rerank_settings = RerankSettings {
            enabled: rerank_enabled,
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
        })
    }

    /// The embedding provider, built (and cached) on first use.
    fn embedder(&self) -> Result<Arc<dyn EmbeddingProvider>, String> {
        let mut guard = self.embedder.lock().map_err(|e| e.to_string())?;
        if let Some(e) = guard.as_ref() {
            return Ok(e.clone());
        }
        let e: Arc<dyn EmbeddingProvider> =
            Arc::from(create_provider(&self.embed_settings).map_err(|e| e.to_string())?);
        *guard = Some(e.clone());
        Ok(e)
    }

    /// The reranker, built (and cached) on first use. Only called when enabled.
    fn reranker(&self) -> Result<Arc<dyn Reranker>, String> {
        let mut guard = self.reranker.lock().map_err(|e| e.to_string())?;
        if let Some(r) = guard.as_ref() {
            return Ok(r.clone());
        }
        let r: Arc<dyn Reranker> =
            Arc::from(create_reranker(&self.rerank_settings).map_err(|e| e.to_string())?);
        *guard = Some(r.clone());
        Ok(r)
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
    fn project(&self) -> String {
        if self.cfg.project.name.is_empty() {
            "default".to_string()
        } else {
            self.cfg.project.name.clone()
        }
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
    Ok(slice_lines(&content, start_line, end_line))
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
    let res = index_run(IndexRequest {
        store: &store,
        embedder: embedder.as_ref(),
        repo_root: &state.root,
        incremental: !full,
        model_name: &state.cfg.embeddings.model,
        progress: None,
        paths,
        exclude: &state.cfg.indexing.exclude,
    })
    .map_err(|e| e.to_string())?;
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
        let _ = c.record_index(&repo_path, &res.commit, &res.branch, files, symbols, chunks);
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
pub fn do_list_projects(include_inactive: bool) -> Result<String, String> {
    let projects = central()?
        .list(include_inactive)
        .map_err(|e| e.to_string())?;
    serde_json::to_string_pretty(&json!({ "projects": projects })).map_err(|e| e.to_string())
}

/// `remember` with `scope: global`: store in the shared central memory instead
/// of this project's, so every other project can recall it.
pub fn do_remember_global(
    state: &AppState,
    content: &str,
    title: &str,
    memory_type: &str,
    topic: &str,
    tags: &str,
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
    let want_local = scope != "global";
    let want_global = scope != "local";

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

    let fused = fuse_by_rank(vec![(local, "local"), (global, "global")], limit);
    serde_json::to_string_pretty(&json!({ "memories": fused })).map_err(|e| e.to_string())
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
