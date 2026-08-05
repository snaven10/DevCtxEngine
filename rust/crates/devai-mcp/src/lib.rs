//! `devai-mcp` — Model Context Protocol server (stdio) exposing DevAI to agents.
//!
//! F6: a starter tool set over the indexing pipeline — `search`, `read_file`,
//! `index_repo`, `index_status`. Memory/route/graph tools follow. Built on the
//! official `rmcp` SDK. See `docs/rust-rewrite-plan.md` §8 (F6).

mod state;

use std::sync::Arc;

use devai_core::config::ProjectConfig;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{Implementation, ServerCapabilities, ServerInfo};
use rmcp::{tool, tool_handler, tool_router, ErrorData, ServerHandler, ServiceExt};

use state::{
    do_index, do_index_status, do_memory_stats, do_read_file, do_recall, do_remember, do_search,
    AppState,
};

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

/// The DevAI MCP server.
#[derive(Clone)]
pub struct DevaiServer {
    state: Arc<AppState>,
    tool_router: ToolRouter<Self>,
}

impl DevaiServer {
    /// Create a server over the given state.
    pub fn new(state: Arc<AppState>) -> Self {
        Self {
            state,
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_router]
impl DevaiServer {
    /// Semantic code search over the indexed repository (returns ranked JSON hits).
    #[tool(description = "Semantic code search over the indexed repository. \
        Returns ranked chunks (file, lines, symbol, text) as JSON.")]
    async fn search(&self, Parameters(req): Parameters<SearchReq>) -> Result<String, ErrorData> {
        let state = self.state.clone();
        run_blocking(move || do_search(&state, &req.query, req.limit.unwrap_or(10), req.language))
            .await
    }

    /// Read a file (optionally a line range) from the repository.
    #[tool(description = "Read a file from the repository, optionally a 1-based \
        inclusive line range.")]
    async fn read_file(
        &self,
        Parameters(req): Parameters<ReadFileReq>,
    ) -> Result<String, ErrorData> {
        let state = self.state.clone();
        run_blocking(move || do_read_file(&state, &req.path, req.start_line, req.end_line)).await
    }

    /// Index (or reindex) the repository.
    #[tool(description = "Index the repository: git diff -> parse -> chunk -> \
        embed -> store. Returns a summary.")]
    async fn index_repo(&self, Parameters(req): Parameters<IndexReq>) -> Result<String, ErrorData> {
        let state = self.state.clone();
        run_blocking(move || do_index(&state, req.full.unwrap_or(false))).await
    }

    /// Report index freshness for the current repo/branch.
    #[tool(description = "Report the last-indexed commit/counts for the current \
        repo and branch, and whether the index is up to date.")]
    async fn index_status(&self) -> Result<String, ErrorData> {
        let state = self.state.clone();
        run_blocking(move || do_index_status(&state)).await
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
        let state = self.state.clone();
        run_blocking(move || {
            do_remember(
                &state,
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
        let state = self.state.clone();
        run_blocking(move || do_recall(&state, &req.query, req.limit.unwrap_or(5))).await
    }

    /// Memory counts for the current project.
    #[tool(
        description = "Report memory counts for the current project (total and \
        per type)."
    )]
    async fn memory_stats(&self) -> Result<String, ErrorData> {
        let state = self.state.clone();
        run_blocking(move || do_memory_stats(&state)).await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for DevaiServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(
                Implementation::new("devai", env!("CARGO_PKG_VERSION")).with_title("DevAI"),
            )
            .with_instructions(
                "DevAI: semantic code search, file reading, and incremental indexing \
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

/// Serve the DevAI MCP server over stdio until the client disconnects.
pub async fn serve_stdio(cfg: ProjectConfig) -> anyhow::Result<()> {
    let state = Arc::new(AppState::build(cfg)?);
    let server = DevaiServer::new(state);
    let service = server.serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
}

/// Blocking entry point: build a Tokio runtime and serve over stdio.
pub fn run_stdio(cfg: ProjectConfig) -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(serve_stdio(cfg))
}
