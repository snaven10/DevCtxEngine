//! `devctx-mcp` — Model Context Protocol server (stdio) exposing DevCtxEngine to agents.
//!
//! 23 tools over the indexing pipeline: code (`search`, `read_file`,
//! `read_symbol`, `get_references`, `impact_analysis`, `summarize`), routes,
//! memory, and project/index management. Built on the official `rmcp` SDK.
//! See `docs/architecture-spec.md` §8 for the process model.

pub mod backend;
pub mod state;

use std::collections::HashMap;
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
    /// Narrow a group-wide search to these registered project names. Without it
    /// a group-bound session searches every member; naming them is how you pay
    /// for only the repositories you care about.
    #[serde(default)]
    projects: Option<Vec<String>>,
    /// Which project to answer from: a registered project name, or any path
    /// inside it. Resolves THIS call only and never changes what the session is
    /// bound to — use it when the work is in a different repository than the
    /// one bound.
    #[serde(default)]
    project: Option<String>,
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
    /// Which project to answer from: a registered project name, or any path
    /// inside it. Resolves THIS call only and never changes what the session is
    /// bound to — use it when the work is in a different repository than the
    /// one bound.
    #[serde(default)]
    project: Option<String>,
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
    /// Which project this memory is about: a registered project name, or any
    /// path inside it. In a group-bound session this is what makes `scope:
    /// local` possible, and it becomes the memory's provenance.
    #[serde(default)]
    project: Option<String>,
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
    /// Which project to answer from: a registered project name, or any path
    /// inside it. Resolves THIS call only and never changes what the session is
    /// bound to — use it when the work is in a different repository than the
    /// one bound.
    #[serde(default)]
    project: Option<String>,
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
    /// Which project to answer from: a registered project name, or any path
    /// inside it. Resolves THIS call only and never changes what the session is
    /// bound to — use it when the work is in a different repository than the
    /// one bound.
    #[serde(default)]
    project: Option<String>,
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
    /// Which project to answer from: a registered project name, or any path
    /// inside it. Resolves THIS call only and never changes what the session is
    /// bound to — use it when the work is in a different repository than the
    /// one bound.
    #[serde(default)]
    project: Option<String>,
    /// The symbol whose call sites to list.
    symbol: String,
}

/// Parameters for the `search_routes` tool.
#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct SearchRoutesReq {
    /// Which project to answer from: a registered project name, or any path
    /// inside it. Resolves THIS call only and never changes what the session is
    /// bound to — use it when the work is in a different repository than the
    /// one bound.
    #[serde(default)]
    project: Option<String>,
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
    /// Which project to answer from: a registered project name, or any path
    /// inside it. Resolves THIS call only and never changes what the session is
    /// bound to — use it when the work is in a different repository than the
    /// one bound.
    #[serde(default)]
    project: Option<String>,
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
/// What this session is attached to.
///
/// A plain `Option<Backend>` could say "one project" or "nothing", and those
/// were the only two answers while the server only ever looked *upwards* from
/// its working directory. Once it also looks down into a workspace, a third
/// answer exists and has to be representable: *this directory is a product made
/// of several repositories*. Collapsing that into one of its members would make
/// every memory land in a repository the user never chose.
#[derive(Clone)]
pub enum Binding {
    /// Nothing resolved — tools that need a project explain how to get one.
    None,
    /// One project, whether found by the walk upwards, the descent, or
    /// `use_project`.
    Project(Arc<Backend>),
    /// Every registered project under the working directory shares a group.
    /// `default` is the member the code tools fall back to when a call carries
    /// no path hint: the most recently indexed one, on the reasoning that it is
    /// the repository actually being worked on.
    Group {
        name: String,
        members: Vec<state::ProjectRow>,
        default: Arc<Backend>,
        /// Which member `default` is. Needed to tell a caller which repository
        /// answered when the choice was inferred rather than asked for.
        default_name: String,
    },
}

impl Binding {
    /// The backend to use when nothing more specific was asked for.
    fn backend(&self) -> Option<Arc<Backend>> {
        match self {
            Binding::None => None,
            Binding::Project(b) => Some(b.clone()),
            Binding::Group { default, .. } => Some(default.clone()),
        }
    }
}

#[derive(Clone)]
pub struct DevctxServer {
    binding: Arc<Mutex<Binding>>,
    connect: Connect,
    cwd: std::path::PathBuf,
    /// Backends opened for per-call path hints, so hopping between repositories
    /// does not reopen a store on every call. Capped: a long session that walks
    /// a large workspace would otherwise hold a handle per repository forever.
    hinted: Arc<Mutex<HashMap<std::path::PathBuf, Arc<Backend>>>>,
    tool_router: ToolRouter<Self>,
}

/// How many hint-resolved backends to keep open at once.
const HINT_CACHE_CAP: usize = 8;

impl DevctxServer {
    /// Create a server, bound to `backend` when one could be resolved at start.
    pub fn new(backend: Option<Arc<Backend>>, connect: Connect) -> Self {
        Self::with_binding(
            match backend {
                Some(b) => Binding::Project(b),
                None => Binding::None,
            },
            connect,
        )
    }

    /// Create a server with a binding the caller already resolved — the descent
    /// in the CLI produces group bindings that `new` cannot express.
    pub fn with_binding(binding: Binding, connect: Connect) -> Self {
        Self {
            binding: Arc::new(Mutex::new(binding)),
            connect,
            cwd: std::env::current_dir().unwrap_or_default(),
            hinted: Arc::new(Mutex::new(HashMap::new())),
            tool_router: Self::tool_router(),
        }
    }

    /// A snapshot of the current binding.
    fn binding(&self) -> Binding {
        self.binding
            .lock()
            .ok()
            .map(|b| b.clone())
            .unwrap_or(Binding::None)
    }

    /// The bound backend, or an explanation of how to bind one.
    fn bound(&self) -> Result<Arc<Backend>, ErrorData> {
        match self.binding().backend() {
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
        self.binding().backend()
    }

    /// The backend for one call, and the project it landed on when that was
    /// inferred rather than stated.
    ///
    /// Precedence is deliberate and one-directional: a hint decides the call and
    /// never touches the session binding, and the binding never overrides a hint
    /// that resolved. They answer different questions — "where is this work" and
    /// "where is this session" — and a call may legitimately disagree with its
    /// session.
    ///
    /// A hint that resolves to nothing is not an error: paths reach us as text an
    /// agent assembled, and refusing the call would turn a slightly wrong guess
    /// into a dead end. It falls back to the binding and says so.
    fn backend_for(
        &self,
        hint: Option<&str>,
    ) -> Result<(Arc<Backend>, Option<String>), ErrorData> {
        if let Some(row) = hint.and_then(state::resolve_hint) {
            if let Ok(cache) = self.hinted.lock() {
                if let Some(b) = cache.get(&row.path) {
                    return Ok((b.clone(), Some(row.name)));
                }
            }
            let backend = (self.connect)(&row.path)
                .map_err(|e| ErrorData::invalid_request(e, None))?;
            let backend = Arc::new(backend);
            if let Ok(mut cache) = self.hinted.lock() {
                // A session that walks a large workspace would otherwise hold a
                // database handle per repository for its whole life.
                if cache.len() >= HINT_CACHE_CAP {
                    cache.clear();
                }
                cache.insert(row.path.clone(), backend.clone());
            }
            return Ok((backend, Some(row.name)));
        }
        match self.binding() {
            Binding::Project(b) => Ok((b, None)),
            // In a group nobody chose this member, so name it: an answer from
            // the wrong repository is otherwise indistinguishable from a right one.
            Binding::Group {
                default,
                default_name,
                ..
            } => Ok((default, Some(default_name))),
            Binding::None => Err(ErrorData::invalid_request(
                state::unbound_help(&self.cwd),
                None,
            )),
        }
    }

    /// Add one field to a JSON object result, leaving other shapes untouched.
    fn note(out: String, key: &str, value: serde_json::Value) -> String {
        match serde_json::from_str::<serde_json::Value>(&out) {
            Ok(serde_json::Value::Object(mut map)) => {
                map.insert(key.into(), value);
                serde_json::to_string(&map).unwrap_or(out)
            }
            _ => out,
        }
    }

    /// Record which project answered, when it was not the one the caller stated.
    fn annotate(out: String, project: Option<String>) -> String {
        let Some(project) = project else { return out };
        match serde_json::from_str::<serde_json::Value>(&out) {
            Ok(serde_json::Value::Object(mut map)) => {
                map.insert("resolved_project".into(), serde_json::json!(project));
                serde_json::to_string(&map).unwrap_or(out)
            }
            // Arrays and bare strings are returned as they are: wrapping them
            // would change a shape callers already parse.
            _ => out,
        }
    }

    /// How the binding describes itself to `list_projects`.
    ///
    /// `null` used to mean both "unbound" and, after the descent, would have
    /// meant "in a group" — two states an agent must be able to tell apart.
    fn binding_json(&self) -> serde_json::Value {
        match self.binding() {
            Binding::None => serde_json::Value::Null,
            Binding::Project(_) => serde_json::json!({ "kind": "project" }),
            Binding::Group { name, members, .. } => serde_json::json!({
                "kind": "group",
                "group": name,
                "members": members.iter().map(|m| m.name.clone()).collect::<Vec<_>>(),
            }),
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
        // Bound to a group and asked nothing more specific, "search" means the
        // product — every member — not whichever member happens to be open.
        // A `project` names one and takes precedence: the caller was specific.
        if req.project.is_none() {
            if let Binding::Group { members, .. } = self.binding() {
                let (query, limit) = (req.query.clone(), req.limit.unwrap_or(10));
                let (language, mode) = (req.language.clone(), req.mode.clone());
                let only = req.projects.clone();
                return run_blocking(move || {
                    state::do_search_group(
                        &members,
                        &query,
                        limit,
                        language,
                        mode.as_deref().unwrap_or("vector"),
                        only.as_deref(),
                    )
                })
                .await;
            }
        }
        let (backend, resolved) = self.backend_for(req.project.as_deref())?;
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
        .map(|out| Self::annotate(out, resolved))
    }

    /// Read a file (optionally a line range) from the repository.
    #[tool(description = "Read a file from the repository, optionally a 1-based \
        inclusive line range.")]
    async fn read_file(
        &self,
        Parameters(req): Parameters<ReadFileReq>,
    ) -> Result<String, ErrorData> {
        let hint = req.project.clone().or_else(|| Some(req.path.as_str().to_string()));
        let (backend, resolved) = self.backend_for(hint.as_deref())?;
        run_blocking(move || backend.read_file(&req.path, req.start_line, req.end_line)).await
            .map(|out| Self::annotate(out, resolved))
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
        // Where a memory is written, and who it is attributed to, are two
        // different questions. A group binding answers the first with "the
        // product" and the second with "the group" — never with a member.
        let group_binding = match self.binding() {
            Binding::Group { name, .. } => Some(name),
            _ => None,
        };
        let implied_group = group_binding.is_some() && req.scope.is_none();
        let scope = req
            .scope
            .clone()
            .unwrap_or_else(|| if implied_group { "group" } else { "local" }.to_string());

        // `local` means "this repository's own store", and in a group binding
        // there is no such repository. Picking one would file the memory
        // somewhere nobody chose and nobody will look. Ask instead.
        if !devctx_memory::is_group(&scope) && !devctx_memory::is_global(&scope) {
            if let Some(group) = &group_binding {
                if req.project.is_none() {
                    return Err(ErrorData::invalid_request(
                        format!(
                            "This session is bound to group `{group}`, not to one repository, so \
                             `scope: local` has no store to write to. Either name the project with \
                             `project` (a registered name, or a path inside it), or use \
                             `scope: group` to record this for the whole product."
                        ),
                        None,
                    ));
                }
            }
        }

        // A `project` hint names the repository, and that repository is then the
        // real provenance — so the group override only applies without one.
        let (backend, resolved) = self.backend_for(req.project.as_deref())?;
        let provenance = match (&group_binding, &resolved) {
            (Some(group), None) => Some(group.clone()),
            _ => None,
        };
        let attributed = provenance.clone().or_else(|| resolved.clone());

        run_blocking(move || {
            backend.remember(
                req.content,
                req.title.unwrap_or_default(),
                req.memory_type.unwrap_or_else(|| "note".to_string()),
                req.topic.unwrap_or_default(),
                req.tags.unwrap_or_default(),
                scope,
                req.files.unwrap_or_default(),
                provenance,
            )
        })
        .await
        .map(|out| {
            // A default the caller did not state is one the caller cannot audit.
            let out = if implied_group {
                Self::note(out, "scope_from_binding", serde_json::json!("group"))
            } else {
                out
            };
            Self::annotate(out, attributed)
        })
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
        // Bound to a group, "what do we know" spans the product: the shared
        // tier plus every member's own memories. Answering from one member's
        // store would be a partial answer wearing the shape of a complete one.
        if let Binding::Group { members, .. } = self.binding() {
            let (query, limit) = (req.query.clone(), req.limit.unwrap_or(5));
            let scope = req.scope.clone().unwrap_or_else(|| "all".to_string());
            let repo = req.repo.clone();
            return run_blocking(move || {
                state::do_recall_group(&members, &query, limit, &scope, repo.as_deref())
            })
            .await;
        }
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
        let binding = self.binding_json();
        let out = match self.maybe_bound() {
            Some(backend) => run_blocking(move || backend.list_projects(all)).await,
            None => run_blocking(move || state::do_list_projects(None, "", all)).await,
        }?;
        // `bound: null` used to mean "no project". With group bindings it would
        // also mean "a whole product", and an agent must be able to tell those
        // apart before deciding whether it needs to call `use_project` at all.
        Ok(Self::note(out, "binding", binding))
    }

    /// Bind this session to a registered project.
    #[tool(description = "Move this session to a different project, by name (see \
        list_projects) or by path. Rarely needed: the server resolves a project \
        from its working directory at startup, descending into the registry when \
        that directory is a workspace root holding several. To answer ONE call \
        from another repository, pass `project` to that tool instead — it does \
        not move the session. Use this when a long stretch of work moves.")]
    async fn use_project(
        &self,
        Parameters(req): Parameters<UseProjectReq>,
    ) -> Result<String, ErrorData> {
        let connect = self.connect.clone();
        let slot = self.binding.clone();
        let target = req.project.clone();
        run_blocking(move || {
            let root = state::resolve_project_root(&target)?;
            let backend = connect(&root)?;
            // An explicit `use_project` overrides a group binding: the user
            // naming one repository is a stronger signal than any inference.
            *slot.lock().map_err(|e| e.to_string())? = Binding::Project(Arc::new(backend));
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
        let (backend, resolved) = self.backend_for(req.project.as_deref())?;
        run_blocking(move || backend.impact(&req.symbol, req.depth.unwrap_or(3))).await
            .map(|out| Self::annotate(out, resolved))
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
        let (backend, resolved) = self.backend_for(req.project.as_deref())?;
        run_blocking(move || backend.read_symbol(&req.name, req.limit.unwrap_or(5))).await
            .map(|out| Self::annotate(out, resolved))
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
        let (backend, resolved) = self.backend_for(req.project.as_deref())?;
        run_blocking(move || backend.references(&req.symbol)).await
            .map(|out| Self::annotate(out, resolved))
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
        let (backend, resolved) = self.backend_for(req.project.as_deref())?;
        run_blocking(move || backend.search_routes(req.method, req.path)).await
            .map(|out| Self::annotate(out, resolved))
    }

    /// Reverse lookup: which routes a handler serves.
    #[tool(description = "Find the HTTP routes served by a handler symbol. Returns JSON.")]
    async fn routes_for_handler(
        &self,
        Parameters(req): Parameters<RoutesForHandlerReq>,
    ) -> Result<String, ErrorData> {
        let (backend, resolved) = self.backend_for(req.project.as_deref())?;
        run_blocking(move || backend.routes_for_handler(&req.handler)).await
            .map(|out| Self::annotate(out, resolved))
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
    let binding = match backend {
        Some(b) => Binding::Project(Arc::new(b)),
        None => Binding::None,
    };
    serve_stdio_bound(binding, connect).await
}

/// Serve with a binding the caller resolved, which may be a whole group.
pub async fn serve_stdio_bound(binding: Binding, connect: Connect) -> anyhow::Result<()> {
    let service = DevctxServer::with_binding(binding, connect)
        .serve(rmcp::transport::stdio())
        .await?;
    service.waiting().await?;
    Ok(())
}

/// Blocking entry point: build a Tokio runtime and serve over stdio.
pub fn run_stdio(backend: Option<Backend>, connect: Connect) -> anyhow::Result<()> {
    let binding = match backend {
        Some(b) => Binding::Project(Arc::new(b)),
        None => Binding::None,
    };
    run_stdio_bound(binding, connect)
}

/// Blocking entry point taking a resolved binding.
pub fn run_stdio_bound(binding: Binding, connect: Connect) -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(serve_stdio_bound(binding, connect))
}
