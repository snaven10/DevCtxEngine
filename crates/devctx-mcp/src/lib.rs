//! `devctx-mcp` — Model Context Protocol server (stdio) exposing DevCtxEngine to agents.
//!
//! 23 tools over the indexing pipeline: code (`search`, `read_file`,
//! `read_symbol`, `get_references`, `impact_analysis`, `summarize`), routes,
//! memory, and project/index management. Built on the official `rmcp` SDK.
//! See `docs/architecture-spec.md` §8 for the process model.

pub mod backend;
pub mod state;

use std::sync::{Arc, Mutex};

use devctx_core::config::ProjectConfig;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{Implementation, ServerCapabilities, ServerInfo};
use rmcp::{tool, tool_handler, tool_router, ErrorData, ServerHandler, ServiceExt};

pub use backend::{Backend, ServerConn};
use state::AppState;

/// Parameters for the `search` tool.
#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct SearchReq {
    /// The search query (natural language or code).
    query: String,
    /// Maximum number of results (default 10).
    #[serde(default)]
    limit: Option<usize>,
    /// Restrict to a language, e.g. "rust" or "python".
    #[serde(default)]
    language: Option<String>,
    /// Retrieval mode: "vector" (default), "keyword" (BM25), or "hybrid".
    #[serde(default)]
    mode: Option<String>,
    /// Reorder results with a cross-encoder (default true). Much slower —
    /// seconds rather than milliseconds — so set false when latency matters
    /// more than the exact ordering.
    #[serde(default)]
    rerank: Option<bool>,
}

/// Parameters for the `read_file` tool.
#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct ReadFileReq {
    /// Repo-relative (or absolute) path to read.
    path: String,
    /// 1-based first line to include (optional).
    #[serde(default)]
    start_line: Option<usize>,
    /// 1-based last line to include (optional).
    #[serde(default)]
    end_line: Option<usize>,
}

/// Parameters for the `index_repo` tool.
#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct IndexReq {
    /// Force a full reindex instead of incremental (default false).
    #[serde(default)]
    full: Option<bool>,
}

/// Parameters for the `remember` tool.
#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct RememberReq {
    /// The memory content to save.
    content: String,
    /// Short title (optional).
    #[serde(default)]
    title: Option<String>,
    /// Memory type: decision/note/bug/insight/architecture/… (default "note").
    #[serde(default)]
    memory_type: Option<String>,
    /// Topic key for upsert-by-topic (optional).
    #[serde(default)]
    topic: Option<String>,
    /// Comma-separated tags (optional).
    #[serde(default)]
    tags: Option<String>,
    /// Where the memory belongs: "local" (this repository, the default),
    /// "group" (every repository of this product — see `project.group`), or
    /// "global" (every project on the machine).
    #[serde(default)]
    scope: Option<String>,
    /// Comma-separated files this memory is about. Worth filling in: it is what
    /// links the memory to the symbols in those files, so `memories_by_symbol`
    /// can surface it to whoever lands on that code later.
    #[serde(default)]
    files: Option<String>,
}

/// Parameters for the `recall` tool.
#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct RecallReq {
    /// The query to recall memories for.
    query: String,
    /// Maximum results (default 5).
    #[serde(default)]
    limit: Option<usize>,
    /// Which memories to search: "local" (this repository), "group" (this
    /// product's repositories), "global" (every project), or "all" — the
    /// default, which searches every tier that applies and fuses them by rank.
    #[serde(default)]
    scope: Option<String>,
    /// Only global memories contributed by this repository (see list_projects).
    #[serde(default)]
    repo: Option<String>,
}

/// Parameters for the `search_project` tool.
#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct SearchProjectReq {
    /// Project name, as reported by list_projects.
    project: String,
    /// The search query.
    query: String,
    /// Maximum results (default 10).
    #[serde(default)]
    limit: Option<usize>,
    /// Restrict to a language.
    #[serde(default)]
    language: Option<String>,
    /// "vector" (default), "keyword", or "hybrid".
    #[serde(default)]
    mode: Option<String>,
}

/// Parameters for the `list_projects` tool.
#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct ListProjectsReq {
    /// Include deactivated projects (default false).
    #[serde(default)]
    include_inactive: Option<bool>,
}

/// Parameters for the `use_project` tool.
#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct UseProjectReq {
    /// Project name (as reported by list_projects) or a path to its root.
    project: String,
}

/// Parameters for the `impact_analysis` tool.
#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct ImpactReq {
    /// The symbol to analyze.
    symbol: String,
    /// Traversal depth (default 3).
    #[serde(default)]
    depth: Option<usize>,
}

/// Parameters for the `memory_context` tool.
#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct MemoryContextReq {
    /// Which memories: "local", "global"/"group", or "all" (the default).
    #[serde(default)]
    scope: Option<String>,
    /// Maximum memories to return (default 20).
    #[serde(default)]
    limit: Option<usize>,
}

/// Parameters for the `read_symbol` tool.
#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct ReadSymbolReq {
    /// The symbol name. A bare name (`charge`) or a qualified one
    /// (`Card.charge`, `src/pay.rs::charge`) both work.
    name: String,
    /// Maximum definitions to return (default 5) — a name can be defined more
    /// than once across a repository.
    #[serde(default)]
    limit: Option<usize>,
}

/// Parameters for the `build_context` tool.
#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct BuildContextReq {
    /// What context is needed, in natural language.
    query: String,
    /// Token budget for the whole brief (default 4096). A hard stop: whatever
    /// does not fit is counted and named, never silently dropped.
    #[serde(default)]
    max_tokens: Option<usize>,
    /// Include recalled and linked memories (default true).
    #[serde(default)]
    include_memories: Option<bool>,
}

/// Parameters for the `memories_by_symbol` tool.
///
/// Named for what it takes rather than sharing one generic struct with
/// `memories_by_file`: the parameter name is what an agent reads to decide what
/// to pass, and `subject` tells it nothing about whether a symbol or a path
/// belongs there.
#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct MemoriesBySymbolReq {
    /// The symbol name. Bare (`charge`) or qualified (`src/pay.rs::charge`).
    symbol: String,
    /// Maximum memories to return (default 10).
    #[serde(default)]
    limit: Option<usize>,
}

/// Parameters for the `memories_by_file` tool.
#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct MemoriesByFileReq {
    /// Repository-relative path, as reported by `search` or `read_symbol`.
    file: String,
    /// Maximum memories to return (default 10).
    #[serde(default)]
    limit: Option<usize>,
}

/// Parameters for the `memory_forget` tool.
#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct MemoryForgetReq {
    /// The memory id, as reported by `remember` or `recall`.
    id: String,
}

/// Parameters for the `memory_move` tool.
#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct MemoryMoveReq {
    /// The memory id to move.
    id: String,
    /// Where to: `local`, `group`, `global`, or the name of another registered
    /// project (see `list_projects`).
    to: String,
}

/// Parameters for the `memory_refs` tool.
#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct MemoryRefsReq {
    /// The memory id, as reported by `remember` or `recall`.
    memory_id: String,
}

/// Parameters for the `get_references` tool.
#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct ReferencesReq {
    /// The symbol whose call sites to list.
    symbol: String,
}

/// Parameters for the `search_routes` tool.
#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct SearchRoutesReq {
    /// Restrict to an HTTP method (GET/POST/…), optional.
    #[serde(default)]
    method: Option<String>,
    /// Restrict to routes whose path contains this substring, optional.
    #[serde(default)]
    path: Option<String>,
}

/// Parameters for the `routes_for_handler` tool.
#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct RoutesForHandlerReq {
    /// The handler symbol (`Class.method` or `method`).
    handler: String,
}

/// Parameters for the `summarize` tool.
#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct SummarizeReq {
    /// The text to summarize.
    content: String,
    /// Focus the summary on a query (optional).
    #[serde(default)]
    query: Option<String>,
    /// Target length in tokens (default 200).
    #[serde(default)]
    max_tokens: Option<usize>,
}

/// Builds a backend for a project root.
///
/// Supplied by the CLI, which owns the parts the MCP crate cannot see: finding
/// or auto-spawning that project's shared server, its port and its token.
pub type Connect = Arc<dyn Fn(&std::path::Path) -> Result<Backend, String> + Send + Sync>;

/// The DevCtxEngine MCP server.
///
/// The backend is either a local DB owner or a client of a shared server — and
/// it is optional, because a globally-registered server is launched from
/// whatever directory the client happens to be in. Starting unbound and saying
/// so through a tool beats dying during the handshake, which reaches the user
/// as an unattributable transport error.
#[derive(Clone)]
pub struct DevctxServer {
    backend: Arc<Mutex<Option<Arc<Backend>>>>,
    connect: Connect,
    cwd: std::path::PathBuf,
    tool_router: ToolRouter<Self>,
}

impl DevctxServer {
    /// Create a server, bound to `backend` when one could be resolved at start.
    pub fn new(backend: Option<Arc<Backend>>, connect: Connect) -> Self {
        Self {
            backend: Arc::new(Mutex::new(backend)),
            connect,
            cwd: std::env::current_dir().unwrap_or_default(),
            tool_router: Self::tool_router(),
        }
    }

    /// The bound backend, or an explanation of how to bind one.
    fn bound(&self) -> Result<Arc<Backend>, ErrorData> {
        match self.backend.lock().ok().and_then(|b| b.clone()) {
            Some(b) => Ok(b),
            None => Err(ErrorData::invalid_request(
                state::unbound_help(&self.cwd),
                None,
            )),
        }
    }

    /// The bound backend, if any — for the tools that read the registry rather
    /// than a project, and so work perfectly well without one.
    fn maybe_bound(&self) -> Option<Arc<Backend>> {
        self.backend.lock().ok().and_then(|b| b.clone())
    }
}

#[tool_router]
impl DevctxServer {
    /// Code search over the indexed repository (vector/keyword/hybrid).
    #[tool(
        description = "Search the indexed repository — mode \"vector\" (semantic, \
        default), \"keyword\" (BM25), or \"hybrid\" (RRF fusion). Returns ranked \
        chunks (file, lines, symbol, text) as JSON."
    )]
    async fn search(&self, Parameters(req): Parameters<SearchReq>) -> Result<String, ErrorData> {
        let backend = self.bound()?;
        run_blocking(move || {
            backend.search(
                &req.query,
                req.limit.unwrap_or(10),
                req.language,
                req.mode,
                req.rerank.unwrap_or(true),
            )
        })
        .await
    }

    /// Read a file (optionally a line range) from the repository.
    #[tool(description = "Read a file from the repository, optionally a 1-based \
        inclusive line range.")]
    async fn read_file(
        &self,
        Parameters(req): Parameters<ReadFileReq>,
    ) -> Result<String, ErrorData> {
        let backend = self.bound()?;
        run_blocking(move || backend.read_file(&req.path, req.start_line, req.end_line)).await
    }

    /// Index (or reindex) the repository.
    #[tool(description = "Index the repository: git diff -> parse -> chunk -> \
        embed -> store. Returns a summary.")]
    async fn index_repo(&self, Parameters(req): Parameters<IndexReq>) -> Result<String, ErrorData> {
        let backend = self.bound()?;
        run_blocking(move || backend.index(req.full.unwrap_or(false))).await
    }

    /// Report index freshness for the current repo/branch.
    #[tool(description = "Report the last-indexed commit/counts for the current \
        repo and branch, and whether the index is up to date.")]
    async fn index_status(&self) -> Result<String, ErrorData> {
        let backend = self.bound()?;
        run_blocking(move || backend.index_status()).await
    }

    /// Save a memory (decision, insight, note) for later recall.
    #[tool(
        description = "Save a memory (decision/insight/note/bug/…) so it can be \
        recalled across sessions. Deduplicated by topic key or content.\n\n\
        Pick the scope by asking who needs this later:\n\
        · \"local\" (default) — true of this repository only: a file, a \
        symbol, a fix in this codebase.\n\
        · \"group\" — true of this product, whose repositories are listed by \
        list_projects and share a `project.group`. A backend contract the \
        frontend must honour, a decision spanning services, a bug whose cause \
        is in one repo and whose symptom is in another. If the project belongs \
        to a group, this is usually the right answer for anything a sibling \
        repository would want.\n\
        · \"global\" — true regardless of project: a lesson about a language, \
        a tool, a way of working. Rare. Everything saved here is recalled by \
        every unrelated project forever, so prefer \"group\" when the knowledge \
        is about one product."
    )]
    async fn remember(
        &self,
        Parameters(req): Parameters<RememberReq>,
    ) -> Result<String, ErrorData> {
        let backend = self.bound()?;
        run_blocking(move || {
            backend.remember(
                req.content,
                req.title.unwrap_or_default(),
                req.memory_type.unwrap_or_else(|| "note".to_string()),
                req.topic.unwrap_or_default(),
                req.tags.unwrap_or_default(),
                req.scope.unwrap_or_else(|| "local".to_string()),
                req.files.unwrap_or_default(),
            )
        })
        .await
    }

    /// Recall memories relevant to a query.
    #[tool(description = "Recall previously saved memories relevant to a query \
        (semantic + intro/chunk blend). Searches every tier by default — this \
        repository, this product's group, and the global store — and tags each \
        hit with the one it came from. When the budget cannot fit them all, the \
        least relevant are dropped and their titles are returned under \
        omitted_for_budget, so ask again with a narrower query if one of those \
        titles is what you needed. Returns JSON.")]
    async fn recall(&self, Parameters(req): Parameters<RecallReq>) -> Result<String, ErrorData> {
        // Global memories are written down because they outlive the project that
        // learned them, so an unbound session can still reach them. Only the
        // local half genuinely needs a project.
        let Some(backend) = self.maybe_bound() else {
            if req.scope.as_deref() == Some("local") {
                self.bound()?;
            }
            return run_blocking(move || {
                state::do_recall_global(&req.query, req.limit.unwrap_or(5), req.repo.as_deref())
            })
            .await;
        };
        run_blocking(move || {
            backend.recall(
                &req.query,
                req.limit.unwrap_or(5),
                req.scope.as_deref().unwrap_or("all"),
                req.repo.as_deref(),
            )
        })
        .await
    }

    /// Search a different project's code.
    #[tool(description = "Search the code of another registered project by name \
        (see list_projects). Use this when the answer lives in a different \
        repository than the one you are working in. Returns JSON.")]
    async fn search_project(
        &self,
        Parameters(req): Parameters<SearchProjectReq>,
    ) -> Result<String, ErrorData> {
        // Naming another project is enough to answer: this needs the registry,
        // not a project of our own. It therefore works while unbound, which is
        // exactly when an agent reaches for it.
        let Some(backend) = self.maybe_bound() else {
            return run_blocking(move || {
                state::do_search_project(
                    &req.project,
                    &req.query,
                    req.limit.unwrap_or(10),
                    req.language,
                    req.mode.as_deref().unwrap_or("vector"),
                )
            })
            .await;
        };
        run_blocking(move || {
            backend.search_project(
                &req.project,
                &req.query,
                req.limit.unwrap_or(10),
                req.language,
                req.mode,
            )
        })
        .await
    }

    /// Every project DevCtxEngine knows about.
    #[tool(
        description = "List every repository DevCtxEngine tracks: name, path, \
        description, embedding model and how fresh its index is. Use this to \
        discover which other projects exist before recalling from them or \
        reading their code. Returns JSON."
    )]
    async fn list_projects(
        &self,
        Parameters(req): Parameters<ListProjectsReq>,
    ) -> Result<String, ErrorData> {
        let all = req.include_inactive.unwrap_or(false);
        // The registry is what an unbound server is *for*: this is the tool that
        // tells an agent which projects exist and what to bind to.
        match self.maybe_bound() {
            Some(backend) => run_blocking(move || backend.list_projects(all)).await,
            None => run_blocking(move || state::do_list_projects(None, "", all)).await,
        }
    }

    /// Bind this session to a registered project.
    #[tool(description = "Bind this session to a project, by name (see \
        list_projects) or by path. Needed when the server was started outside \
        any repository — a globally-registered MCP server inherits whatever \
        directory the client was launched from. Also switches an already-bound \
        session to a different project.")]
    async fn use_project(
        &self,
        Parameters(req): Parameters<UseProjectReq>,
    ) -> Result<String, ErrorData> {
        let connect = self.connect.clone();
        let slot = self.backend.clone();
        let target = req.project.clone();
        run_blocking(move || {
            let root = state::resolve_project_root(&target)?;
            let backend = connect(&root)?;
            *slot.lock().map_err(|e| e.to_string())? = Some(Arc::new(backend));
            Ok(serde_json::json!({
                "bound": target,
                "path": root.to_string_lossy(),
            })
            .to_string())
        })
        .await
    }

    /// Memory counts for the current project.
    #[tool(
        description = "Report memory counts for the current project (total and \
        per type)."
    )]
    async fn memory_stats(&self) -> Result<String, ErrorData> {
        let backend = self.bound()?;
        run_blocking(move || backend.memory_stats()).await
    }

    /// Blast radius of a symbol (transitive callers + callees).
    #[tool(
        description = "Impact analysis: transitive callers (blast radius) and \
        callees of a symbol from the call graph. Returns JSON."
    )]
    async fn impact_analysis(
        &self,
        Parameters(req): Parameters<ImpactReq>,
    ) -> Result<String, ErrorData> {
        let backend = self.bound()?;
        run_blocking(move || backend.impact(&req.symbol, req.depth.unwrap_or(3))).await
    }

    /// The most recent memories, with no query.
    #[tool(
        description = "The most recently written memories, with no query — for \
        recovering context after a reset, when you do not yet know what to ask \
        `recall` about. Returns JSON."
    )]
    async fn memory_context(
        &self,
        Parameters(req): Parameters<MemoryContextReq>,
    ) -> Result<String, ErrorData> {
        let backend = self.bound()?;
        run_blocking(move || {
            backend.memory_context(
                req.scope.as_deref().unwrap_or("all"),
                req.limit.unwrap_or(20),
            )
        })
        .await
    }

    /// A symbol's definition and code.
    #[tool(
        description = "Read a symbol's definition: its code, file, line range \
        and kind. Use this when you know the name and want the thing itself; \
        use `search` when you want code about an idea. Returns JSON."
    )]
    async fn read_symbol(
        &self,
        Parameters(req): Parameters<ReadSymbolReq>,
    ) -> Result<String, ErrorData> {
        let backend = self.bound()?;
        run_blocking(move || backend.read_symbol(&req.name, req.limit.unwrap_or(5))).await
    }

    /// One budgeted brief assembled for a question.
    #[tool(description = "Assemble one budgeted brief for a question: what is \
        already known (recalled memories), the code that ranks highest, and the \
        memories recorded against exactly those files. Returns prose ready to \
        read into context, capped at `max_tokens`.")]
    async fn build_context(
        &self,
        Parameters(req): Parameters<BuildContextReq>,
    ) -> Result<String, ErrorData> {
        let backend = self.bound()?;
        run_blocking(move || {
            backend.build_context(
                &req.query,
                req.max_tokens.unwrap_or(4096),
                req.include_memories.unwrap_or(true),
            )
        })
        .await
    }

    /// Memories recorded about a symbol.
    #[tool(
        description = "The decisions, bugs and insights recorded about a symbol \
        — why the code is the way it is, which the call graph cannot answer. \
        Searches both this project's memories and the shared ones. Each result \
        carries `link_sources`: `files-field`/`content-mention` mean a link was \
        recorded when the memory was written, `inference` means only that the \
        text mentions the name. Returns JSON."
    )]
    async fn memories_by_symbol(
        &self,
        Parameters(req): Parameters<MemoriesBySymbolReq>,
    ) -> Result<String, ErrorData> {
        let backend = self.bound()?;
        run_blocking(move || backend.memories_by_symbol(&req.symbol, req.limit.unwrap_or(10))).await
    }

    /// Memories recorded about a file.
    #[tool(
        description = "The memories recorded about a file. Same result shape and \
        same `link_sources` semantics as `memories_by_symbol`. Returns JSON."
    )]
    async fn memories_by_file(
        &self,
        Parameters(req): Parameters<MemoriesByFileReq>,
    ) -> Result<String, ErrorData> {
        let backend = self.bound()?;
        run_blocking(move || backend.memories_by_file(&req.file, req.limit.unwrap_or(10))).await
    }

    /// Delete one memory for good.
    #[tool(description = "Permanently delete a memory — a wrong root cause, a \
        decision since reversed. Looks in this project and in the shared store, \
        so the id is enough. Not reversible.")]
    async fn memory_forget(
        &self,
        Parameters(req): Parameters<MemoryForgetReq>,
    ) -> Result<String, ErrorData> {
        let backend = self.bound()?;
        run_blocking(move || backend.memory_forget(&req.id)).await
    }

    /// Move a memory to another tier or repository.
    #[tool(description = "Move a memory to another tier (`local`, `group`, \
        `global`) or to another registered project by name. Use it when a \
        memory is invisible where it is needed or noise where it is not. The id \
        changes, because it is derived from the project and the content — the \
        new one is in the result.")]
    async fn memory_move(
        &self,
        Parameters(req): Parameters<MemoryMoveReq>,
    ) -> Result<String, ErrorData> {
        let backend = self.bound()?;
        run_blocking(move || backend.memory_move(&req.id, &req.to)).await
    }

    /// What code a memory concerns.
    #[tool(
        description = "The inverse of `memories_by_symbol`: given a memory id, \
        the symbols and files it concerns, and how each link was derived. \
        Returns JSON."
    )]
    async fn memory_refs(
        &self,
        Parameters(req): Parameters<MemoryRefsReq>,
    ) -> Result<String, ErrorData> {
        let backend = self.bound()?;
        run_blocking(move || backend.memory_refs(&req.memory_id)).await
    }

    /// All call sites (references) of a symbol.
    #[tool(
        description = "Find all call sites (references) of a symbol across the \
        indexed code. Returns JSON."
    )]
    async fn get_references(
        &self,
        Parameters(req): Parameters<ReferencesReq>,
    ) -> Result<String, ErrorData> {
        let backend = self.bound()?;
        run_blocking(move || backend.references(&req.symbol)).await
    }

    /// Find HTTP routes (framework-aware) by method and/or path.
    #[tool(
        description = "Find HTTP routes across frameworks (FastAPI/Flask/Express/\
        NestJS/Spring/Quarkus/Angular) by method and/or path substring. Returns JSON."
    )]
    async fn search_routes(
        &self,
        Parameters(req): Parameters<SearchRoutesReq>,
    ) -> Result<String, ErrorData> {
        let backend = self.bound()?;
        run_blocking(move || backend.search_routes(req.method, req.path)).await
    }

    /// Reverse lookup: which routes a handler serves.
    #[tool(description = "Find the HTTP routes served by a handler symbol. Returns JSON.")]
    async fn routes_for_handler(
        &self,
        Parameters(req): Parameters<RoutesForHandlerReq>,
    ) -> Result<String, ErrorData> {
        let backend = self.bound()?;
        run_blocking(move || backend.routes_for_handler(&req.handler)).await
    }

    /// Summarize text (extractive by default; query-focusable).
    #[tool(
        description = "Summarize text to roughly `max_tokens`, optionally focused \
        on a query. Extractive by default (preserves identifiers)."
    )]
    async fn summarize(
        &self,
        Parameters(req): Parameters<SummarizeReq>,
    ) -> Result<String, ErrorData> {
        let backend = self.bound()?;
        run_blocking(move || {
            backend.summarize(&req.content, req.query, req.max_tokens.unwrap_or(200))
        })
        .await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for DevctxServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(
                Implementation::new("devctx", env!("CARGO_PKG_VERSION")).with_title("DevCtxEngine"),
            )
            .with_instructions(
                "DevCtxEngine: semantic code search, file reading, and incremental indexing \
                 over your git repository. If a tool reports that no project is bound, call \
                 list_projects to see what is registered and use_project to bind one — a \
                 globally-registered server starts in whatever directory the client was \
                 launched from, which is often none of them.",
            )
    }
}

/// Run a blocking tool body on the blocking pool, mapping errors to MCP errors.
async fn run_blocking<F>(f: F) -> Result<String, ErrorData>
where
    F: FnOnce() -> Result<String, String> + Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(Ok(s)) => Ok(s),
        Ok(Err(e)) => Err(ErrorData::internal_error(e, None)),
        Err(e) => Err(ErrorData::internal_error(format!("task failed: {e}"), None)),
    }
}

/// Build a backend for a project already resolved to a config: route through
/// its shared server when there is one, else own the database here.
pub fn backend_for(cfg: ProjectConfig, server: Option<ServerConn>) -> anyhow::Result<Backend> {
    Ok(match server {
        Some(conn) => Backend::remote(
            conn,
            crate::backend::ProjectIdentity {
                name: if cfg.project.name.is_empty() {
                    "default".to_string()
                } else {
                    cfg.project.name.clone()
                },
                group: cfg.project.group.clone(),
            },
        ),
        None => Backend::local(Arc::new(AppState::build(cfg)?)),
    })
}

/// Serve the DevCtxEngine MCP server over stdio until the client disconnects.
///
/// `backend` is `None` when no project could be resolved at start — the server
/// still comes up, and `list_projects`/`use_project` are how a session gets one.
pub async fn serve_stdio(backend: Option<Backend>, connect: Connect) -> anyhow::Result<()> {
    let service = DevctxServer::new(backend.map(Arc::new), connect)
        .serve(rmcp::transport::stdio())
        .await?;
    service.waiting().await?;
    Ok(())
}

/// Blocking entry point: build a Tokio runtime and serve over stdio.
pub fn run_stdio(backend: Option<Backend>, connect: Connect) -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(serve_stdio(backend, connect))
}
