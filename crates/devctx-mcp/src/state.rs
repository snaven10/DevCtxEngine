//! Shared server state and the blocking tool implementations.
//!
//! The embedder and reranker are built once (model load is expensive) and shared
//! behind `Arc`; the DuckDB store is opened per call (a `Connection` is not
//! `Sync`), which is cheap relative to embedding.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use devctx_core::config::ProjectConfig;
use devctx_core::{SearchFilter, SearchResult};
use devctx_embed::{create_provider, EmbedSettings, EmbeddingProvider};
use devctx_index::{run as index_run, GitRepo, IndexRequest};
use devctx_memory::{memory_context, memory_stats, recall, remember, RememberRequest};
use devctx_rerank::{create_reranker, NoopReranker, RerankSettings, Reranker};
use devctx_search::SearchMode;
use devctx_store::Store;
use devctx_summarize::{create_summarizer, SummarizeSettings};
use serde_json::{json, Value};

/// Immutable server state shared across tool calls.
pub struct AppState {
    cfg: ProjectConfig,
    root: PathBuf,
    db_path: PathBuf,
    dim: usize,
    embedder: Arc<dyn EmbeddingProvider>,
    reranker: Arc<dyn Reranker>,
    rerank_enabled: bool,
}

impl AppState {
    /// Build state from a project config (constructs the embedder + reranker).
    pub fn build(cfg: ProjectConfig) -> anyhow::Result<Self> {
        let embedder: Arc<dyn EmbeddingProvider> = Arc::from(create_provider(
            &EmbedSettings::from_config(&cfg.embeddings),
        )?);
        let rerank_enabled = cfg.reranking.enabled;
        let reranker: Arc<dyn Reranker> = if rerank_enabled {
            Arc::from(create_reranker(&RerankSettings {
                enabled: true,
                model: cfg.reranking.model.clone(),
            })?)
        } else {
            Arc::new(NoopReranker)
        };
        let root = if cfg.project.path.is_empty() {
            std::env::current_dir()?
        } else {
            PathBuf::from(&cfg.project.path)
        };
        let db_path = cfg.db_path();
        let dim = embedder.dimension();
        Ok(Self {
            cfg,
            root,
            db_path,
            dim,
            embedder,
            reranker,
            rerank_enabled,
        })
    }

    fn open_store(&self) -> Result<Store, String> {
        Store::open(&self.db_path, self.dim).map_err(|e| e.to_string())
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

/// `search` tool: vector / keyword / hybrid search, then rerank, return JSON hits.
pub fn do_search(
    state: &AppState,
    query: &str,
    limit: usize,
    language: Option<String>,
    mode: SearchMode,
) -> Result<String, String> {
    let store = state.open_store()?;
    let filter = SearchFilter {
        languages: language.into_iter().collect(),
        exclude_deletions: true,
        ..Default::default()
    };
    let embedder = if mode == SearchMode::Keyword {
        None
    } else {
        Some(state.embedder.as_ref())
    };
    let reranker = if state.rerank_enabled && mode != SearchMode::Keyword {
        Some(state.reranker.as_ref())
    } else {
        None
    };
    let hits = devctx_search::search(&store, query, &filter, limit, mode, embedder, reranker)
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
    let store = state.open_store()?;
    let res = index_run(IndexRequest {
        store: &store,
        embedder: state.embedder.as_ref(),
        repo_root: &state.root,
        incremental: !full,
        model_name: &state.cfg.embeddings.model,
        progress: None,
    })
    .map_err(|e| e.to_string())?;
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
    let res = remember(&store, state.embedder.as_ref(), &req).map_err(|e| e.to_string())?;
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
    let hits = recall(
        &store,
        state.embedder.as_ref(),
        query,
        Some(&project),
        limit,
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
        Some(state.embedder.clone())
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
