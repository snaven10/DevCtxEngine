//! `devctx-mcp` — Model Context Protocol server (stdio) exposing DevCtxEngine to agents.
//!
//! 23 tools over the indexing pipeline: code (`search`, `read_file`,
//! `read_symbol`, `get_references`, `impact_analysis`, `summarize`), routes,
//! memory, and project/index management. Built on the official `rmcp` SDK.
//! See `docs/architecture-spec.md` §8 for the process model.

pub mod backend;
pub mod lifecycle;
pub mod state;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use devctx_core::config::ProjectConfig;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    GetPromptRequestParams, GetPromptResponse, GetPromptResult, Implementation, ListPromptsResult,
    ListResourcesResult, PaginatedRequestParams, Prompt, PromptMessage, ReadResourceRequestParams,
    ReadResourceResponse, ReadResourceResult, Resource, ResourceContents, Role, ServerCapabilities,
    ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::{tool, tool_handler, tool_router, ErrorData, RoleServer, ServerHandler, ServiceExt};

pub use backend::{Backend, Connector, ServerConn};

/// Parameters for the `search` tool.
#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct SearchReq {
    /// Narrow a group-wide search to these registered project names. Without it
    /// a group-bound session searches every member; naming them is how you pay
    /// for only the repositories you care about.
    #[serde(default)]
    projects: Option<Vec<String>>,
    /// Answer this call from a different project than the one bound (this call only).
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
    /// Keep only one kind of file: "code", "test", "doc" or "config". Without
    /// it tests, docs, READMEs and SQL/YAML/JSON still appear but rank below
    /// code (a ranking penalty, not an exclusion).
    #[serde(default)]
    kind: Option<String>,
    /// `false` drops test files from the results (default true: they are only
    /// demoted). An explicit `kind` wins over this.
    #[serde(default)]
    include_tests: Option<bool>,
}

/// Parameters for the `read_file` tool.
#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct ReadFileReq {
    /// Answer this call from a different project than the one bound (this call only).
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
    /// Keep only one kind of file: "code", "test", "doc" or "config".
    #[serde(default)]
    kind: Option<String>,
    /// `false` drops test files (default true: they are only demoted).
    #[serde(default)]
    include_tests: Option<bool>,
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
    /// Answer this call from a different project than the one bound (this call only).
    #[serde(default)]
    project: Option<String>,
    /// The symbol to analyze.
    symbol: String,
    /// Traversal depth (default 3).
    #[serde(default)]
    depth: Option<usize>,
    /// Lowest confidence of the call that reaches a symbol: `"high"`,
    /// `"medium"` (default) or `"low"`. The symbols it leaves out are counted
    /// in `below_confidence` and not walked; the calls to the name the index
    /// could not decide are counted apart, in calls, in `undecided_calls`
    /// (none from tests or from callers already listed); `low` lists the
    /// ambiguous calls and the callers of the undecided ones, marked
    /// `undecided: true`.
    #[serde(default)]
    min_confidence: Option<String>,
    /// List symbols of test files (default false: counted in
    /// `excluded.tests`, not walked). Listed ones carry `test: true`.
    #[serde(default)]
    include_tests: Option<bool>,
    /// List external (library) callees (default false: counted in
    /// `excluded.external`). Listed ones carry `external: true`.
    #[serde(default)]
    include_external: Option<bool>,
    /// Symbols per direction (default 200; 0 = no cap). The ones of the depth
    /// that overflows are counted in `omitted`/`omitted_by_limit`, and no
    /// deeper level is read.
    #[serde(default)]
    max_nodes: Option<usize>,
    /// Follow dispatch through interfaces, abstract classes and traits
    /// (default true): the callers of a method the symbol overrides, and the
    /// implementations of an interface or abstract method, as `via:
    /// "dispatch"` with `through` (the method it went through), never surer
    /// than `medium`. `false` follows direct calls only; `min_confidence:
    /// "high"` also leaves them out (counted in `below_confidence`).
    #[serde(default)]
    dispatch: Option<bool>,
}

/// Parameters for the `plan_status` tool.
#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct PlanStatusReq {
    /// Answer this call from a different project than the one bound (this call only).
    #[serde(default)]
    project: Option<String>,
    /// Plan id (PLAN-005 or 5). Omit to list plans.
    #[serde(default)]
    plan: Option<String>,
    /// List only plans with unresolved tasks (default false).
    #[serde(default)]
    active_only: Option<bool>,
    /// Plans per page of the list, newest first (default 25).
    #[serde(default)]
    limit: Option<usize>,
    /// Skip this many plans (the `next_offset` of the previous page).
    #[serde(default)]
    offset: Option<usize>,
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
    /// Answer this call from a different project than the one bound (this call only).
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
    /// Build the brief from a different project than the one bound (this call
    /// only): a registered name or a path inside it — in a group session, a
    /// member of the group. In a group session without it, the members whose
    /// servers are running are compared and the best match answers, named with
    /// how many members were scored; a close call adds an "ambiguous" warning
    /// naming the others. Pass it to choose.
    #[serde(default)]
    project: Option<String>,
    /// What context is needed, in natural language.
    query: String,
    /// Token budget for the whole brief (default 4096). A hard stop: whatever
    /// does not fit is counted and named, never silently dropped.
    #[serde(default)]
    max_tokens: Option<usize>,
    /// Include recalled and linked memories (default true).
    #[serde(default)]
    include_memories: Option<bool>,
    /// `code` | `test` | `doc` | `config`: keep only that kind of file.
    #[serde(default)]
    kind: Option<String>,
    /// `false` drops test files from the code (default true: tests rank lower).
    #[serde(default)]
    include_tests: Option<bool>,
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
    /// Answer this call from a different project than the one bound (this call only).
    #[serde(default)]
    project: Option<String>,
    /// The symbol name. Bare (`charge`) or qualified (`src/pay.rs::charge`).
    symbol: String,
    /// Maximum memories to return (default 5).
    #[serde(default)]
    limit: Option<usize>,
    /// Skip this many memories (the `next_offset` of the previous page).
    #[serde(default)]
    offset: Option<usize>,
    /// Return each memory's whole content (default: cut to 600 characters).
    #[serde(default)]
    full: Option<bool>,
}

/// Parameters for the `memories_by_file` tool.
#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct MemoriesByFileReq {
    /// Answer this call from a different project than the one bound (this call only).
    #[serde(default)]
    project: Option<String>,
    /// Repository-relative path, as reported by `search` or `read_symbol`.
    file: String,
    /// Maximum memories to return (default 5).
    #[serde(default)]
    limit: Option<usize>,
    /// Skip this many memories (the `next_offset` of the previous page).
    #[serde(default)]
    offset: Option<usize>,
    /// Return each memory's whole content (default: cut to 600 characters).
    #[serde(default)]
    full: Option<bool>,
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
    /// Answer this call from a different project than the one bound (this call only).
    #[serde(default)]
    project: Option<String>,
    /// The symbol whose references to list: a bare name (`charge`), a
    /// qualified one (`Card.charge`, `Card::charge`) or `file::name`.
    symbol: String,
    /// Lowest confidence listed: `"high"`, `"medium"` (default) or `"low"`.
    /// `low` adds the ambiguous references and the calls the index could not
    /// decide, each marked `undecided: true`; whatever the default leaves out
    /// is counted in `below_confidence`.
    #[serde(default)]
    min_confidence: Option<String>,
    /// Relations to list besides the default calls and instantiations
    /// (`new Foo()`): any of `"references"` (type uses), `"imports"`,
    /// `"inherits"`, `"implements"`, or `"all"`. Each reference says which in
    /// `via`.
    #[serde(default)]
    kinds: Option<Vec<String>>,
}

/// Parameters for the `search_routes` tool.
#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct SearchRoutesReq {
    /// Answer this call from a different project than the one bound (this call only).
    #[serde(default)]
    project: Option<String>,
    /// Restrict to an HTTP method (GET/POST/…), optional.
    #[serde(default)]
    method: Option<String>,
    /// Restrict to routes whose path contains this substring, optional.
    #[serde(default)]
    path: Option<String>,
    /// Routes per page (default 20).
    #[serde(default)]
    limit: Option<usize>,
    /// Skip this many routes (the `next_offset` of the previous page).
    #[serde(default)]
    offset: Option<usize>,
}

/// Parameters for the `routes_for_handler` tool.
#[derive(serde::Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct RoutesForHandlerReq {
    /// Answer this call from a different project than the one bound (this call only).
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
    /// `build_context`'s group member choices, per [`pick_cache_key`], for
    /// [`PICK_CACHE_TTL`]: the same question asked again in a session is not
    /// worth another fan-out.
    picks: Arc<Mutex<PickCache>>,
    tool_router: ToolRouter<Self>,
}

/// How long a group member choice is reused for the same question.
const PICK_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(600);

/// The cache key of a group member choice: the query lowercased with its
/// whitespace collapsed, and the kind selection (it changes what is compared).
fn pick_cache_key(query: &str, sel: &devctx_search::KindSel) -> String {
    let q = query
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    format!(
        "{q}\u{1f}{}\u{1f}{:?}",
        sel.kind.as_deref().unwrap_or("").trim().to_lowercase(),
        sel.include_tests
    )
}

/// Group member choices worth reusing, by [`pick_cache_key`], for
/// [`PICK_CACHE_TTL`]. Only a [`state::GroupPick::cacheable`] choice goes in:
/// one made with every comparable member compared and a clear lead. A choice
/// among the members that happened to be warm is decided again next time, when
/// the member that answers may have warmed up (fixup H, I-1).
#[derive(Default)]
struct PickCache {
    picks: HashMap<String, (std::time::Instant, state::GroupPick)>,
}

impl PickCache {
    /// The cached choice for `key` as of `now`, with its age; `None` when
    /// absent or older than [`PICK_CACHE_TTL`].
    fn get(
        &self,
        key: &str,
        now: std::time::Instant,
    ) -> Option<(state::GroupPick, std::time::Duration)> {
        let (at, pick) = self.picks.get(key)?;
        let age = now.saturating_duration_since(*at);
        (age < PICK_CACHE_TTL).then(|| (pick.clone(), age))
    }

    /// Remember `pick` for `key` if it is worth reusing; expired entries go.
    fn put(&mut self, key: String, pick: &state::GroupPick, now: std::time::Instant) {
        self.picks
            .retain(|_, (at, _)| now.saturating_duration_since(*at) < PICK_CACHE_TTL);
        if pick.cacheable() {
            self.picks.insert(key, (now, pick.clone()));
        }
    }
}

/// How many hint-resolved backends to keep open at once.
const HINT_CACHE_CAP: usize = 8;

/// The memory protocol (`MEMORY-PROTOCOL.md` at the repo root), embedded so it
/// travels with the binary. Exposed over MCP as both a prompt and a resource,
/// so an agent — or whatever assembled its system prompt — can pull it once
/// instead of every project pasting a copy into its own CLAUDE.md, which is
/// how it drifts out of date.
const MEMORY_PROTOCOL: &str = include_str!("../../../MEMORY-PROTOCOL.md");

/// Name the protocol is served under as a prompt.
const MEMORY_PROTOCOL_NAME: &str = "memory-protocol";

/// URI the protocol is served under as a resource.
const MEMORY_PROTOCOL_URI: &str = "devctx://memory-protocol";

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
            picks: Arc::new(Mutex::new(PickCache::default())),
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
    fn backend_for(&self, hint: Option<&str>) -> Result<(Arc<Backend>, Option<String>), ErrorData> {
        if let Some(row) = hint.and_then(state::resolve_hint) {
            let backend = self.open_path(&row.path)?;
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

    /// The backend for a project root, from the hint cache or newly connected.
    fn open_path(&self, path: &std::path::Path) -> Result<Arc<Backend>, ErrorData> {
        if let Ok(cache) = self.hinted.lock() {
            if let Some(b) = cache.get(path) {
                return Ok(b.clone());
            }
        }
        let backend =
            Arc::new((self.connect)(path).map_err(|e| ErrorData::invalid_request(e, None))?);
        if let Ok(mut cache) = self.hinted.lock() {
            // A session that walks a large workspace would otherwise hold a
            // database handle per repository for its whole life.
            if cache.len() >= HINT_CACHE_CAP {
                cache.clear();
            }
            cache.insert(path.to_path_buf(), backend.clone());
        }
        Ok(backend)
    }

    /// The backend of the group member `name`, opened by the path the binding
    /// already holds (fixup H, M-1). Resolving the name again against the
    /// registry could fail (central down or slow) and fall back to the
    /// default member, while the header named another.
    fn member_backend(
        &self,
        members: &[state::ProjectRow],
        name: &str,
    ) -> Result<Arc<Backend>, ErrorData> {
        let m = members.iter().find(|m| m.name == name).ok_or_else(|| {
            ErrorData::internal_error(format!("{name} is not a member of this group"), None)
        })?;
        self.open_path(&m.path)
    }

    /// The backend `build_context` answers from, and the line that names it.
    ///
    /// Outside a group this is `backend_for` (a `project` hint names the
    /// project it resolved to; no hint, no line). In a group a `project` must
    /// be a registered *member* — an unresolvable one or a project of another
    /// group is an error, never a quiet fall back to the default member.
    /// Without `project` the members whose servers are running are compared
    /// and the best match answers, named, with how many were scored; see
    /// [`state::pick_group_member`]. Only a choice that compared every
    /// comparable member with a clear lead is cached, per (query, kind,
    /// include_tests) for [`PICK_CACHE_TTL`], and a reused one says so with
    /// its age instead of the old call's coverage. The member is opened by
    /// the path the binding holds, never re-resolved by name.
    async fn context_backend(
        &self,
        project: Option<&str>,
        query: &str,
        sel: &devctx_search::KindSel,
    ) -> Result<(Arc<Backend>, Option<String>), ErrorData> {
        let Binding::Group {
            members,
            default_name,
            ..
        } = self.binding()
        else {
            let (b, resolved) = self.backend_for(project)?;
            return Ok((b, resolved));
        };
        if let Some(name) = state::group_context_target(project, &members, state::resolve_hint)
            .map_err(|e| ErrorData::invalid_request(e, None))?
        {
            let b = self.member_backend(&members, &name)?;
            return Ok((b, Some(name)));
        }
        let key = pick_cache_key(query, sel);
        let cached = self
            .picks
            .lock()
            .ok()
            .and_then(|c| c.get(&key, std::time::Instant::now()));
        let (pick, header) = match cached {
            Some((p, age)) => {
                let header = p.cached_header(age);
                (p, header)
            }
            None => {
                let (q, s, d) = (query.to_string(), sel.clone(), default_name.clone());
                let ms = members.clone();
                let p = tokio::task::spawn_blocking(move || {
                    state::pick_group_member(&ms, &q, &s, Some(&d))
                })
                .await
                .map_err(|e| ErrorData::internal_error(format!("task failed: {e}"), None))?
                .map_err(|e| ErrorData::invalid_request(e, None))?;
                if let Ok(mut c) = self.picks.lock() {
                    c.put(key, &p, std::time::Instant::now());
                }
                let header = p.header();
                (p, header)
            }
        };
        let b = self.member_backend(&members, &pick.member)?;
        Ok((b, Some(header)))
    }

    /// The plans root to answer `plan_status` from in this process, when the binding is a
    /// group or nothing and the working directory holds the plans. `None` means "ask the
    /// bound project": a single project, or a cwd without `plans/` (a group member's own
    /// resolution, or the unbound error, then apply).
    fn workspace_plans_root(&self) -> Option<devctx_core::plans::PlansRoot> {
        if matches!(self.binding(), Binding::Project(_)) {
            return None;
        }
        let root = devctx_core::plans::resolve_plans_root(
            None,
            Some(&self.cwd),
            false,
            state::home_dir().as_deref(),
        );
        (root.source == devctx_core::plans::PlansRootSource::Workspace).then_some(root)
    }

    /// This process's version and whether its binary was replaced (PLAN-008 D2).
    /// Off the async threads: it may run `<exe> --version`, capped at 2 s.
    async fn mcp_info(&self) -> serde_json::Value {
        tokio::task::spawn_blocking(lifecycle::mcp_json)
            .await
            .unwrap_or(serde_json::Value::Null)
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
                let sel = devctx_search::KindSel {
                    kind: req.kind.clone(),
                    include_tests: req.include_tests,
                };
                return run_blocking(move || {
                    state::do_search_group(
                        &members,
                        &query,
                        limit,
                        language,
                        mode.as_deref().unwrap_or("vector"),
                        only.as_deref(),
                        &sel,
                    )
                })
                .await;
            }
        }
        let (backend, resolved) = self.backend_for(req.project.as_deref())?;
        let sel = devctx_search::KindSel {
            kind: req.kind.clone(),
            include_tests: req.include_tests,
        };
        run_blocking(move || {
            backend.search(
                &req.query,
                req.limit.unwrap_or(10),
                req.language,
                req.mode,
                req.rerank.unwrap_or(true),
                &sel,
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
        let hint = req
            .project
            .clone()
            .or_else(|| Some(req.path.as_str().to_string()));
        let (backend, resolved) = self.backend_for(hint.as_deref())?;
        run_blocking(move || backend.read_file(&req.path, req.start_line, req.end_line))
            .await
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
        let out = run_blocking(move || backend.index_status()).await?;
        Ok(Self::note(out, "mcp", self.mcp_info().await))
    }

    /// Save a memory (decision, insight, note) for later recall.
    #[tool(
        description = "Save a memory (decision/insight/note/bug/…) for later recall, \
        deduplicated by topic or content. Scope: \"local\" (default, this repo \
        only), \"group\" (this product's repos, see list_projects), or \"global\" \
        (every project — rare; prefer \"group\" when the lesson is product-specific)."
    )]
    async fn remember(
        &self,
        Parameters(req): Parameters<RememberReq>,
    ) -> Result<String, ErrorData> {
        // Unbound, and nothing named a repository to hint at one: there is no
        // "local" store to write into. The old code let this fall straight
        // through to `backend_for`, which is exactly where an unbound session
        // always errors out — so `remember` discarded the content on every
        // scope, global included, before this function ever looked at what was
        // asked for. Handled here, first, so a caller who said `scope: global`
        // (or said nothing) is never punished for a session nobody bound.
        if matches!(self.binding(), Binding::None) && req.project.is_none() {
            let title = req.title.clone().unwrap_or_default();
            let content = req.content.clone();
            if req.scope.as_deref() == Some("local") {
                // `local` truly has nowhere to go here — there is no project at
                // all, not even a group to redirect to `group` scope. Refuse,
                // but hand the content back so the caller can retry rather than
                // having to remember what it just tried to save.
                return Err(ErrorData::invalid_request(
                    format!(
                        "{}\n\nNothing was saved: `scope: local` has no repository to write \
                         into while unbound. Retry with `scope: global` (or omit `scope`), or \
                         bind a project first. So the content is not lost:\n\nTitle: {title}\n\
                         Content: {content}",
                        state::unbound_help(&self.cwd),
                    ),
                    None,
                ));
            }
            let memory_type = req.memory_type.unwrap_or_else(|| "note".to_string());
            let topic = req.topic.unwrap_or_default();
            let tags = req.tags.unwrap_or_default();
            let files = req.files.unwrap_or_default();
            let title_for_err = title.clone();
            let content_for_err = content.clone();
            return run_blocking(move || {
                state::do_remember_unbound(&content, &title, &memory_type, &topic, &tags, &files)
            })
            .await
            .map_err(|e| {
                // The central store failed too — the only way left to not lose
                // the memory is to hand it back whole, so the caller can retry.
                ErrorData::internal_error(
                    format!(
                        "{}\n\nThe memory was NOT saved anywhere. Retry once a store is \
                         reachable, with this exact content:\n\nTitle: {title_for_err}\n\
                         Content: {content_for_err}",
                        e.message,
                    ),
                    None,
                )
            });
        }

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
        //
        // The condition is what the CALLER asked for, not what `backend_for`
        // resolved. In a group binding `backend_for(None)` still names the
        // default member — deliberately, because a code tool must say which
        // repository answered — so keying off its return value meant the group
        // override never fired and every product-wide memory was attributed to
        // whichever member happened to be the default.
        let named_a_project = req.project.is_some();
        let (backend, resolved) = self.backend_for(req.project.as_deref())?;
        let provenance = match (&group_binding, named_a_project) {
            (Some(group), false) => Some(group.clone()),
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
    #[tool(description = "Recall saved memories for a query, across local/group/\
        global tiers by default (tagged per hit). Results dropped for budget are \
        listed under omitted_for_budget — retry narrower. Returns JSON.")]
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
        let sel = devctx_search::KindSel {
            kind: req.kind.clone(),
            include_tests: req.include_tests,
        };
        let Some(backend) = self.maybe_bound() else {
            return run_blocking(move || {
                state::do_search_project(
                    &req.project,
                    &req.query,
                    req.limit.unwrap_or(10),
                    req.language,
                    req.mode.as_deref().unwrap_or("vector"),
                    &sel,
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
                &sel,
            )
        })
        .await
    }

    /// Every project DevCtxEngine knows about.
    #[tool(
        description = "List every repository DevCtxEngine tracks: name, path, \
        description, embedding model, and index freshness. Use to discover other \
        projects before recalling from or reading their code. Returns JSON."
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
        let out = Self::note(out, "mcp", self.mcp_info().await);
        // `bound: null` used to mean "no project". With group bindings it would
        // also mean "a whole product", and an agent must be able to tell those
        // apart before deciding whether it needs to call `use_project` at all.
        Ok(Self::note(out, "binding", binding))
    }

    /// Bind this session to a registered project.
    #[tool(
        description = "Bind this session to a different project (name or path, see \
        list_projects). Rarely needed. To answer ONE call from another repo \
        without moving the session, use `project` on that tool instead."
    )]
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
        callees of a symbol, by depth. Each symbol carries `confidence` and `via`; by \
        default only high/medium calls are followed, test files and library callees are \
        left out, and 200 symbols per direction are listed — whatever that leaves out is \
        counted (`below_confidence` in symbols, `undecided_calls` in calls to the name the \
        index could not decide, `excluded`, `omitted`), never dropped silently. A call \
        through an interface, abstract class or trait is followed by dispatch: \
        `via: \"dispatch\"` with `through`, never `high` (`dispatch: false` turns it off; \
        what a node's cap cut is in `omitted_by_limit.dispatch`). \
        `min_confidence`, `include_tests`, `include_external`, `max_nodes` and `dispatch` \
        change that. Returns JSON."
    )]
    async fn impact_analysis(
        &self,
        Parameters(req): Parameters<ImpactReq>,
    ) -> Result<String, ErrorData> {
        let (backend, resolved) = self.backend_for(req.project.as_deref())?;
        let q = state::ImpactQuery {
            min_confidence: req.min_confidence.clone(),
            include_tests: req.include_tests,
            include_external: req.include_external,
            max_nodes: req.max_nodes,
            dispatch: req.dispatch,
        };
        run_blocking(move || backend.impact(&req.symbol, req.depth.unwrap_or(3), &q))
            .await
            .map(|out| Self::annotate(out, resolved))
    }

    /// Progress on the plans under `plans/` (markdown, source of truth).
    #[tool(
        description = "Plans in plans/ (markdown, source of truth): no arg lists progress and \
        the active plan; with `plan`, ready/in-progress/blocked tasks. JSON."
    )]
    async fn plan_status(
        &self,
        Parameters(req): Parameters<PlanStatusReq>,
    ) -> Result<String, ErrorData> {
        // Plans are markdown, not store data, so in a group (or with nothing bound) the answer
        // is computed here from the workspace root, not by a member's daemon: that daemon may be
        // an older build still reading its own root (PLAN-007 DD-4). A `project` hint is
        // specific and goes to that project as before.
        if req.project.is_none() {
            if let Some(root) = self.workspace_plans_root() {
                let plan = req.plan.clone();
                let opts = plan_opts(&req);
                return run_blocking(move || {
                    state::plan_status_budgeted(&root, plan.as_deref(), opts)
                })
                .await;
            }
        }
        let (backend, resolved) = self.backend_for(req.project.as_deref())?;
        let opts = plan_opts(&req);
        run_blocking(move || backend.plan_status(req.plan.as_deref(), opts))
            .await
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
        run_blocking(move || backend.read_symbol(&req.name, req.limit.unwrap_or(5)))
            .await
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
        let sel = devctx_search::KindSel {
            kind: req.kind.clone(),
            include_tests: req.include_tests,
        };
        let (backend, label) = self
            .context_backend(req.project.as_deref(), &req.query, &sel)
            .await?;
        let out = run_blocking(move || {
            backend.build_context(
                &req.query,
                req.max_tokens.unwrap_or(4096),
                req.include_memories.unwrap_or(true),
                &sel,
            )
        })
        .await?;
        // Which repository answered is part of the answer: a brief from the
        // wrong one is otherwise indistinguishable from a right one.
        Ok(match label {
            Some(l) => format!("[devctx] context from {l}\n\n{out}"),
            None => out,
        })
    }

    /// Memories recorded about a symbol.
    #[tool(description = "Decisions, bugs and insights recorded about a symbol. \
        Searches project and shared memories. `link_sources` says how: files-\
        field/content-mention (recorded) or inference (text match only).")]
    async fn memories_by_symbol(
        &self,
        Parameters(req): Parameters<MemoriesBySymbolReq>,
    ) -> Result<String, ErrorData> {
        let (backend, resolved) = self.backend_for(req.project.as_deref())?;
        let opts = memories_opts(req.limit, req.offset, req.full);
        run_blocking(move || backend.memories_by_symbol(&req.symbol, opts))
            .await
            .map(|out| Self::annotate(out, resolved))
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
        let (backend, resolved) = self.backend_for(req.project.as_deref())?;
        let opts = memories_opts(req.limit, req.offset, req.full);
        run_blocking(move || backend.memories_by_file(&req.file, opts))
            .await
            .map(|out| Self::annotate(out, resolved))
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
    #[tool(description = "Move a memory to another tier (local/group/global) or \
        project by name. Use when it's invisible where needed or noise where \
        not. The id changes (project+content derived); returned in the result.")]
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
        description = "Find all call sites of a symbol across the indexed code \
        (calls and instantiations), one per occurrence. Each carries `confidence` \
        (high/medium/low) and `via` (the relation); by default high and medium are \
        listed and anything lower is only counted in `below_confidence` — pass \
        `min_confidence: \"low\"` to list it. `kinds` adds type uses, imports and \
        inherits/implements (or `all`). Returns JSON."
    )]
    async fn get_references(
        &self,
        Parameters(req): Parameters<ReferencesReq>,
    ) -> Result<String, ErrorData> {
        let (backend, resolved) = self.backend_for(req.project.as_deref())?;
        run_blocking(move || {
            let kinds = req.kinds.as_ref().map(|k| k.join(","));
            backend.references(&req.symbol, req.min_confidence.as_deref(), kinds.as_deref())
        })
        .await
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
        let page = state::Page::new(req.limit, req.offset);
        run_blocking(move || backend.search_routes(req.method, req.path, page))
            .await
            .map(|out| Self::annotate(out, resolved))
    }

    /// Reverse lookup: which routes a handler serves.
    #[tool(description = "Find the HTTP routes served by a handler symbol. Returns JSON.")]
    async fn routes_for_handler(
        &self,
        Parameters(req): Parameters<RoutesForHandlerReq>,
    ) -> Result<String, ErrorData> {
        let (backend, resolved) = self.backend_for(req.project.as_deref())?;
        run_blocking(move || backend.routes_for_handler(&req.handler))
            .await
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
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_prompts()
                .enable_resources()
                .build(),
        )
        .with_server_info(
            Implementation::new("devctx", env!("CARGO_PKG_VERSION")).with_title("DevCtxEngine"),
        )
        .with_instructions(
            "DevCtxEngine: semantic code search, file reading, and incremental indexing \
             over your git repository. If a tool reports that no project is bound, call \
             list_projects to see what is registered and use_project to bind one — a \
             globally-registered server starts in whatever directory the client was \
             launched from, which is often none of them.\n\n\
             Several tools take an optional `project`: a registered project name, or any \
             path inside it. It resolves THAT ONE CALL only and never changes what the \
             session is bound to — use it when the work you are doing right now is in a \
             different repository than the one this session is bound to, without giving up \
             the binding for every call after it.\n\n\
             The memory-recording protocol (when to remember, how to scope it, how recall \
             works) is available as the `memory-protocol` prompt and as the \
             `devctx://memory-protocol` resource, rather than needing to be pasted into a \
             project's own instructions.\n\n\
             After a compaction, call `plan_status` before resuming work: it reads the \
             plans under plans/ and answers what is ready, in progress, or blocked.",
        )
    }

    /// The one prompt this server offers: the memory protocol, so a client can
    /// fetch it once instead of every project pasting a copy into its own
    /// instructions file.
    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        Ok(ListPromptsResult::with_all_items(vec![Prompt::new(
            MEMORY_PROTOCOL_NAME,
            Some("How and when to use DevCtxEngine's remember/recall memory tools"),
            None,
        )]))
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, ErrorData> {
        if request.name != MEMORY_PROTOCOL_NAME {
            return Err(ErrorData::invalid_params(
                format!("no such prompt: {}", request.name),
                None,
            ));
        }
        Ok(GetPromptResult::new(vec![PromptMessage::new_text(Role::User, MEMORY_PROTOCOL)]).into())
    }

    /// Same document, exposed as a resource for a client that reads those
    /// rather than fetching prompts.
    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Ok(ListResourcesResult::with_all_items(vec![Resource::new(
            MEMORY_PROTOCOL_URI,
            MEMORY_PROTOCOL_NAME,
        )
        .with_description("How and when to use DevCtxEngine's remember/recall memory tools")
        .with_mime_type("text/markdown")]))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        if request.uri != MEMORY_PROTOCOL_URI {
            return Err(ErrorData::resource_not_found(
                format!("no such resource: {}", request.uri),
                None,
            ));
        }
        Ok(ReadResourceResult::new(vec![ResourceContents::text(
            MEMORY_PROTOCOL,
            MEMORY_PROTOCOL_URI,
        )
        .with_mime_type("text/markdown")])
        .into())
    }
}

/// Run a blocking tool body on the blocking pool, mapping errors to MCP errors.
fn plan_opts(req: &PlanStatusReq) -> state::PlanListOpts {
    state::PlanListOpts {
        active_only: req.active_only.unwrap_or(false),
        page: state::Page::new(req.limit, req.offset),
    }
}

fn memories_opts(
    limit: Option<usize>,
    offset: Option<usize>,
    full: Option<bool>,
) -> state::MemoriesOpts {
    state::MemoriesOpts {
        page: state::Page::new(limit, offset),
        full: full.unwrap_or(false),
    }
}

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

/// Build the backend for a project already resolved to a config.
///
/// Always remote: the MCP never opens the project's database. `connect` finds
/// (or starts) the project's server the first time a tool needs it, so a
/// missing server is an error on that call — with its cause — rather than a
/// process that quietly took the DuckDB lock for as long as the session lives.
pub fn backend_for(cfg: &ProjectConfig, connect: backend::Connector) -> Backend {
    let root = if cfg.project.path.is_empty() {
        std::env::current_dir().ok()
    } else {
        Some(std::path::PathBuf::from(&cfg.project.path))
    };
    let plans_root = root.map(|r| {
        devctx_core::plans::resolve_plans_root(
            Some(&r),
            None,
            !cfg.project.group.trim().is_empty(),
            state::home_dir().as_deref(),
        )
    });
    Backend::lazy(
        connect,
        crate::backend::ProjectIdentity {
            name: if cfg.project.name.is_empty() {
                "default".to_string()
            } else {
                cfg.project.name.clone()
            },
            group: cfg.project.group.clone(),
            plans_root,
        },
    )
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
    lifecycle::init();
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
    let served = rt.block_on(serve_stdio_bound(binding, connect));
    // The client closing stdin ends the session. rmcp first gives in-flight
    // requests up to 5 s to answer (so `echo '{...}' | devctx mcp` still gets its
    // reply); after that, dropping the runtime would wait for every
    // `spawn_blocking` still running (an `index_repo`, a federated `recall`) and
    // keep an orphaned MCP alive for as long as it takes. Cap that wait too:
    // the worst case from EOF to exit is ~5 s + `SHUTDOWN_GRACE`.
    rt.shutdown_timeout(SHUTDOWN_GRACE);
    served
}

/// How long in-flight blocking tool calls may delay the exit once the service loop ended.
const SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(1);

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn pick(by_relevance: bool, warning: Option<&str>) -> state::GroupPick {
        state::GroupPick {
            member: "front".into(),
            label: "front (best match 0.61; 2 of 3 members scored; not scored: x (cold))".into(),
            warning: warning.map(str::to_string),
            by_relevance,
            score: 0.61,
            compared: 2,
            not_comparable: vec!["x".into()],
            selection: None,
        }
    }

    /// Fixup H (M-6): the key folds case and whitespace, and the kind
    /// selection is part of it.
    #[test]
    fn pick_cache_key_folds_the_query_and_keeps_the_selection() {
        let none = devctx_search::KindSel::default();
        let code = devctx_search::KindSel {
            kind: Some(" Code ".into()),
            include_tests: None,
        };
        let no_tests = devctx_search::KindSel {
            kind: None,
            include_tests: Some(false),
        };
        assert_eq!(
            pick_cache_key("How  is a\tPayment charged", &none),
            pick_cache_key("how is a payment charged", &none)
        );
        assert_ne!(pick_cache_key("q", &none), pick_cache_key("q", &code));
        assert_ne!(pick_cache_key("q", &none), pick_cache_key("q", &no_tests));
        assert_eq!(
            pick_cache_key("q", &code),
            pick_cache_key(
                "q",
                &devctx_search::KindSel {
                    kind: Some("code".into()),
                    include_tests: None,
                }
            )
        );
    }

    /// Fixup H (I-1, M-6): only a choice that compared every comparable member
    /// with a clear lead is cached; it expires after the TTL; and reused, it is
    /// said to be cached without the old call's coverage or warnings.
    #[test]
    fn pick_cache_keeps_only_full_choices_for_the_ttl() {
        let t0 = Instant::now();
        let mut c = PickCache::default();
        c.put(
            "partial".into(),
            &pick(false, Some("compared only 2 of 3")),
            t0,
        );
        c.put("close".into(), &pick(true, Some("ambiguous: also x")), t0);
        c.put("full".into(), &pick(true, None), t0);
        assert!(
            c.get("partial", t0).is_none(),
            "a partial pick is not reused"
        );
        assert!(c.get("close", t0).is_none(), "a close call is not reused");
        let (p, age) = c.get("full", t0 + Duration::from_secs(30)).expect("cached");
        assert_eq!(age, Duration::from_secs(30));
        let h = p.cached_header(age);
        assert!(h.starts_with("front (cached choice from 30s ago"), "{h}");
        assert!(!h.contains("not scored") && !h.contains("ambiguous"), "{h}");
        assert!(
            c.get("full", t0 + PICK_CACHE_TTL).is_none(),
            "expired at the TTL"
        );
        // An insert sweeps what expired.
        c.put("other".into(), &pick(true, None), t0 + PICK_CACHE_TTL);
        assert_eq!(c.picks.len(), 1);
    }

    /// Fixup H (M-1): the chosen member is opened by the path the binding
    /// holds, never re-resolved by name (which could fall back to the default
    /// while the header names another).
    #[test]
    fn a_group_member_is_opened_by_its_path() {
        let connect: Connect =
            Arc::new(|p: &std::path::Path| Err(format!("connect:{}", p.display())));
        let server = DevctxServer::with_binding(Binding::None, connect);
        let members = vec![
            state::ProjectRow {
                name: "a".into(),
                path: "/nonexistent/fixup-h/a".into(),
                ..Default::default()
            },
            state::ProjectRow {
                name: "b".into(),
                path: "/nonexistent/fixup-h/b".into(),
                ..Default::default()
            },
        ];
        let err = server
            .member_backend(&members, "b")
            .err()
            .expect("connect fails");
        assert!(
            err.message.contains("connect:/nonexistent/fixup-h/b"),
            "{}",
            err.message
        );
        let err = server
            .member_backend(&members, "zz")
            .err()
            .expect("unknown");
        assert!(err.message.contains("not a member"), "{}", err.message);
    }
}
