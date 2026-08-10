//! `devctx-mcp` — Model Context Protocol server (stdio) exposing DevCtxEngine to agents.
//!
//! F6: a starter tool set over the indexing pipeline — `search`, `read_file`,
//! `index_repo`, `index_status`. Memory/route/graph tools follow. Built on the
//! official `rmcp` SDK. See `docs/rust-rewrite-plan.md` §8 (F6).

pub mod backend;
pub mod state;

use std::sync::Arc;

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

/// The DevCtxEngine MCP server. The backend is either a local DB owner or a
/// client of a shared server.
#[derive(Clone)]
pub struct DevctxServer {
    backend: Arc<Backend>,
    tool_router: ToolRouter<Self>,
}

impl DevctxServer {
    /// Create a server over the given backend.
    pub fn new(backend: Arc<Backend>) -> Self {
        Self {
            backend,
            tool_router: Self::tool_router(),
        }
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
        let backend = self.backend.clone();
        run_blocking(move || {
            backend.search(&req.query, req.limit.unwrap_or(10), req.language, req.mode)
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
        let backend = self.backend.clone();
        run_blocking(move || backend.read_file(&req.path, req.start_line, req.end_line)).await
    }

    /// Index (or reindex) the repository.
    #[tool(description = "Index the repository: git diff -> parse -> chunk -> \
        embed -> store. Returns a summary.")]
    async fn index_repo(&self, Parameters(req): Parameters<IndexReq>) -> Result<String, ErrorData> {
        let backend = self.backend.clone();
        run_blocking(move || backend.index(req.full.unwrap_or(false))).await
    }

    /// Report index freshness for the current repo/branch.
    #[tool(description = "Report the last-indexed commit/counts for the current \
        repo and branch, and whether the index is up to date.")]
    async fn index_status(&self) -> Result<String, ErrorData> {
        let backend = self.backend.clone();
        run_blocking(move || backend.index_status()).await
    }

    /// Save a memory (decision, insight, note) for later recall.
    #[tool(
        description = "Save a memory (decision/insight/note/bug/…) so it can be \
        recalled across sessions. Deduplicated by topic key or content."
    )]
    async fn remember(
        &self,
        Parameters(req): Parameters<RememberReq>,
    ) -> Result<String, ErrorData> {
        let backend = self.backend.clone();
        run_blocking(move || {
            backend.remember(
                req.content,
                req.title.unwrap_or_default(),
                req.memory_type.unwrap_or_else(|| "note".to_string()),
                req.topic.unwrap_or_default(),
                req.tags.unwrap_or_default(),
            )
        })
        .await
    }

    /// Recall memories relevant to a query.
    #[tool(description = "Recall previously saved memories relevant to a query \
        (semantic + intro/chunk blend). Returns JSON.")]
    async fn recall(&self, Parameters(req): Parameters<RecallReq>) -> Result<String, ErrorData> {
        let backend = self.backend.clone();
        run_blocking(move || backend.recall(&req.query, req.limit.unwrap_or(5))).await
    }

    /// Memory counts for the current project.
    #[tool(
        description = "Report memory counts for the current project (total and \
        per type)."
    )]
    async fn memory_stats(&self) -> Result<String, ErrorData> {
        let backend = self.backend.clone();
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
        let backend = self.backend.clone();
        run_blocking(move || backend.impact(&req.symbol, req.depth.unwrap_or(3))).await
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
        let backend = self.backend.clone();
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
        let backend = self.backend.clone();
        run_blocking(move || backend.search_routes(req.method, req.path)).await
    }

    /// Reverse lookup: which routes a handler serves.
    #[tool(description = "Find the HTTP routes served by a handler symbol. Returns JSON.")]
    async fn routes_for_handler(
        &self,
        Parameters(req): Parameters<RoutesForHandlerReq>,
    ) -> Result<String, ErrorData> {
        let backend = self.backend.clone();
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
        let backend = self.backend.clone();
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
                 over your git repository.",
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

/// Serve the DevCtxEngine MCP server over stdio until the client disconnects.
/// When `server` is `Some`, every tool routes to that shared server (no direct
/// DB access); otherwise the MCP owns the DB locally.
pub async fn serve_stdio(cfg: ProjectConfig, server: Option<ServerConn>) -> anyhow::Result<()> {
    let backend = match server {
        Some(conn) => Arc::new(Backend::remote(conn)),
        None => Arc::new(Backend::local(Arc::new(AppState::build(cfg)?))),
    };
    let service = DevctxServer::new(backend)
        .serve(rmcp::transport::stdio())
        .await?;
    service.waiting().await?;
    Ok(())
}

/// Blocking entry point: build a Tokio runtime and serve over stdio.
pub fn run_stdio(cfg: ProjectConfig, server: Option<ServerConn>) -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(serve_stdio(cfg, server))
}
