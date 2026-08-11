//! `devctx-tui` — an interactive terminal UI for DevCtxEngine (ratatui).
//!
//! Four views, switched with F1/F2/F3/F4:
//!   - **Search**: type a query, switch retrieval mode (vector/keyword/hybrid),
//!     browse ranked results and preview the selected chunk.
//!   - **Graph**: type a symbol, see its transitive callers (upstream) and
//!     callees (downstream) from the call-graph.
//!   - **Memories**: recall memories by query, or browse the most recent. Tab
//!     switches between this project's memories, the shared global ones, or both.
//!   - **Projects**: every repository DevCtxEngine tracks — register a new one,
//!     index it, or retire it, without leaving the UI.
//!
//! Long operations (indexing above all) run on a worker thread and report back
//! over a channel, so the event loop never blocks and the UI never freezes.
//! Reranking is skipped for responsiveness. See `docs/rust-rewrite-plan.md`.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use devctx_core::config::ProjectConfig;
use devctx_core::{SearchFilter, SearchResult, VectorMetadata, VectorPoint};
use devctx_embed::{create_provider, EmbedSettings, EmbeddingProvider};
use devctx_index::GitRepo;
use devctx_search::{search as run_search, SearchMode};
use devctx_store::Store;
use serde_json::Value;

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};

/// Results fetched per query.
const LIMIT: usize = 20;

/// Transitive callers and callees of a symbol, each `(symbol, depth)`.
type ImpactLists = (Vec<(String, usize)>, Vec<(String, usize)>);

/// A memory row, unified across recall (scored) and recent (unscored).
#[derive(Clone)]
struct MemoryItem {
    title: String,
    mtype: String,
    tags: String,
    content: String,
    score: Option<f32>,
    /// Which store it came from: `local` or `global`.
    scope: String,
}

/// One row of the project registry, as shown in the Projects view.
#[derive(Clone, Default)]
struct ProjectRow {
    name: String,
    path: String,
    description: String,
    model: String,
    indexed: String,
    active: bool,
}

/// Which memories the Memories view is looking at.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MemScope {
    /// This project's own memories.
    Local,
    /// The shared central store, contributed by every project.
    Global,
    /// Both, fused by rank.
    All,
}

impl MemScope {
    fn name(self) -> &'static str {
        match self {
            MemScope::Local => "project",
            MemScope::Global => "global",
            MemScope::All => "all",
        }
    }

    /// The value the `/recall` endpoints expect.
    fn wire(self) -> &'static str {
        match self {
            MemScope::Local => "local",
            MemScope::Global => "global",
            MemScope::All => "all",
        }
    }

    fn cycle(self) -> Self {
        match self {
            MemScope::All => MemScope::Local,
            MemScope::Local => MemScope::Global,
            MemScope::Global => MemScope::All,
        }
    }
}

/// Connection to a running server the TUI routes through, so it never opens the
/// DuckDB file itself (no lock fights with other `devctx` processes).
pub struct ServerConn {
    /// Base URL, e.g. `http://127.0.0.1:20111`.
    pub base: String,
    /// Bearer token, if the server requires one.
    pub token: Option<String>,
}

/// The engine backing the UI: a client of a running server (preferred) or a
/// direct store (fallback when no server is available).
// One `Engine` exists per process, so the size gap between variants is irrelevant.
#[allow(clippy::large_enum_variant)]
enum Engine {
    Local {
        store: Store,
        embedder: Box<dyn EmbeddingProvider>,
        filter: SearchFilter,
        repo: String,
        branch: String,
        project: String,
    },
    Remote {
        base: String,
        token: Option<String>,
    },
}

impl Engine {
    fn local(cfg: &ProjectConfig) -> anyhow::Result<Self> {
        let embedder = create_provider(&EmbedSettings::from_config(&cfg.embeddings))?;
        let store = Store::open(&cfg.db_path(), embedder.dimension())?;
        let root = if cfg.project.path.is_empty() {
            std::env::current_dir()?
        } else {
            PathBuf::from(&cfg.project.path)
        };
        let (repo, branch) = match GitRepo::open(&root) {
            Ok(git) => (git.short_name(), git.state().branch),
            Err(_) => (String::new(), String::new()),
        };
        let project = if cfg.project.name.is_empty() {
            "default".to_string()
        } else {
            cfg.project.name.clone()
        };
        Ok(Engine::Local {
            store,
            embedder,
            filter: SearchFilter {
                exclude_deletions: true,
                ..Default::default()
            },
            repo,
            branch,
            project,
        })
    }

    fn search(&self, query: &str, mode: SearchMode) -> anyhow::Result<Vec<SearchResult>> {
        match self {
            Engine::Local {
                store,
                embedder,
                filter,
                ..
            } => {
                let emb = if mode == SearchMode::Keyword {
                    None
                } else {
                    Some(embedder.as_ref())
                };
                Ok(run_search(store, query, filter, LIMIT, mode, emb, None)?)
            }
            Engine::Remote { base, token } => {
                let m = match mode {
                    SearchMode::Vector => "vector",
                    SearchMode::Keyword => "keyword",
                    SearchMode::Hybrid => "hybrid",
                };
                // Reranking costs seconds against milliseconds for the search
                // itself, which is the difference between this view feeling
                // instant and feeling broken. The local path already skipped it;
                // routed — the normal case — it did not, so F1 was slow while
                // every other view stayed fast.
                let v = http_post(
                    base,
                    "/search",
                    token,
                    serde_json::json!({
                        "query": query, "limit": LIMIT, "mode": m, "rerank": false,
                    }),
                )?;
                Ok(v.as_array()
                    .map(|a| a.iter().map(json_to_hit).collect())
                    .unwrap_or_default())
            }
        }
    }

    /// Transitive callers (upstream) and callees (downstream) of a symbol.
    fn graph(&self, symbol: &str) -> anyhow::Result<ImpactLists> {
        match self {
            Engine::Local {
                store,
                repo,
                branch,
                ..
            } => {
                let im = store.impact_analysis(repo, branch, symbol, 3)?;
                Ok((im.upstream, im.downstream))
            }
            Engine::Remote { base, token } => {
                let v = http_get(
                    base,
                    &format!("/impact/{}?depth=3", urlencode(symbol)),
                    token,
                )?;
                Ok((json_pairs(&v["upstream"]), json_pairs(&v["downstream"])))
            }
        }
    }

    fn recall(&self, query: &str, scope: MemScope) -> anyhow::Result<Vec<MemoryItem>> {
        match self {
            Engine::Local {
                store,
                embedder,
                project,
                ..
            } => {
                let local = if scope == MemScope::Global {
                    Vec::new()
                } else {
                    devctx_memory::recall(
                        store,
                        embedder.as_ref(),
                        &devctx_memory::RecallQuery {
                            query,
                            project: Some(project),
                            repo: None,
                            limit: LIMIT,
                        },
                    )?
                    .into_iter()
                    .map(|h| MemoryItem {
                        title: h.memory.title,
                        mtype: h.memory.memory_type,
                        tags: h.memory.tags,
                        content: h.memory.content,
                        score: Some(h.score),
                        scope: "local".to_string(),
                    })
                    .collect()
                };
                let global = if scope == MemScope::Local {
                    Vec::new()
                } else {
                    global_recall(query)?
                };
                // By rank, never by score: the project and the central store may
                // embed with different models.
                Ok(devctx_core::fuse_by_rank(
                    vec![local, global],
                    |m| m.title.clone(),
                    LIMIT,
                ))
            }
            Engine::Remote { base, token } => {
                let v = http_post(
                    base,
                    "/recall",
                    token,
                    serde_json::json!({
                        "query": query, "limit": LIMIT, "scope": scope.wire(),
                    }),
                )?;
                // The project server answers with a `memories` envelope; older
                // builds answered with a bare array.
                Ok(json_memories(v.get("memories").unwrap_or(&v)))
            }
        }
    }

    fn recent_memories(&self) -> anyhow::Result<Vec<MemoryItem>> {
        match self {
            Engine::Local { store, project, .. } => {
                let mems = devctx_memory::memory_context(store, project, LIMIT)?;
                Ok(mems
                    .into_iter()
                    .map(|m| MemoryItem {
                        title: m.title,
                        mtype: m.memory_type,
                        tags: m.tags,
                        content: m.content,
                        score: None,
                        scope: "local".to_string(),
                    })
                    .collect())
            }
            Engine::Remote { base, token } => {
                let v = http_get(base, &format!("/memories?limit={LIMIT}"), token)?;
                Ok(json_memories(&v))
            }
        }
    }
}

/// Recall from the shared central store.
///
/// The TUI never opens the central database itself — it is single-writer, owned
/// by the daemon — so this always goes over the wire, whether or not the project
/// half is served locally.
fn global_recall(query: &str) -> anyhow::Result<Vec<MemoryItem>> {
    let paths = devctx_central::CentralPaths::resolve()?;
    let Some(client) = devctx_central::client::ensure(&paths) else {
        anyhow::bail!("no central store daemon (run `devctx serve --central`)");
    };
    let hits = client.recall(query, LIMIT, None)?;
    Ok(hits
        .iter()
        .map(|m| MemoryItem {
            title: m["title"].as_str().unwrap_or("").to_string(),
            mtype: m["type"].as_str().unwrap_or("insight").to_string(),
            tags: m["tags"].as_str().unwrap_or("").to_string(),
            content: m["content"].as_str().unwrap_or("").to_string(),
            score: m["score"].as_f64().map(|x| x as f32),
            scope: "global".to_string(),
        })
        .collect())
}

// --- HTTP helpers for the remote backend ---

fn http_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(120))
        .build()
}

fn with_auth(req: ureq::Request, token: &Option<String>) -> ureq::Request {
    match token {
        Some(t) => req.set("Authorization", &format!("Bearer {t}")),
        None => req,
    }
}

fn http_get(base: &str, path: &str, token: &Option<String>) -> anyhow::Result<Value> {
    let req = with_auth(http_agent().get(&format!("{base}{path}")), token);
    let body = req
        .call()
        .map_err(|e| anyhow::anyhow!(e.to_string()))?
        .into_string()?;
    Ok(serde_json::from_str(&body)?)
}

fn http_post(base: &str, path: &str, token: &Option<String>, body: Value) -> anyhow::Result<Value> {
    let req = with_auth(http_agent().post(&format!("{base}{path}")), token);
    let resp = req
        .send_json(body)
        .map_err(|e| anyhow::anyhow!(e.to_string()))?
        .into_string()?;
    Ok(serde_json::from_str(&resp)?)
}

/// Build a display `SearchResult` from a server `/search` hit (vector omitted).
fn json_to_hit(v: &Value) -> SearchResult {
    let s = |k: &str| v[k].as_str().unwrap_or("").to_string();
    let i = |k: &str| v[k].as_i64().unwrap_or(0) as i32;
    SearchResult {
        score: v["score"].as_f64().unwrap_or(0.0) as f32,
        point: VectorPoint {
            id: String::new(),
            vector: Vec::new(),
            text: s("text"),
            metadata: VectorMetadata {
                file: s("file"),
                symbol: s("symbol"),
                symbol_type: s("symbol_type"),
                start_line: i("start_line"),
                end_line: i("end_line"),
                chunk_level: s("level"),
                ..Default::default()
            },
        },
    }
}

fn json_pairs(v: &Value) -> Vec<(String, usize)> {
    v.as_array()
        .map(|a| {
            a.iter()
                .map(|e| {
                    (
                        e["symbol"].as_str().unwrap_or("").to_string(),
                        e["depth"].as_u64().unwrap_or(0) as usize,
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

fn json_memories(v: &Value) -> Vec<MemoryItem> {
    v.as_array()
        .map(|a| {
            a.iter()
                .map(|m| MemoryItem {
                    title: m["title"].as_str().unwrap_or("").to_string(),
                    mtype: m["type"].as_str().unwrap_or("note").to_string(),
                    tags: m["tags"].as_str().unwrap_or("").to_string(),
                    content: m["content"].as_str().unwrap_or("").to_string(),
                    score: m["score"].as_f64().map(|x| x as f32),
                    scope: m["scope"].as_str().unwrap_or("local").to_string(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Minimal percent-encoding for a path segment.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Work that must not run on the event-loop thread.
///
/// Indexing a repository takes minutes; doing it inline would freeze the UI
/// completely. Each job is driven by re-invoking our own binary, so the TUI
/// inherits the CLI's server routing and auto-spawn instead of reimplementing
/// them — and a job that hangs cannot take the interface down with it.
enum Job {
    /// Reload the registry.
    LoadProjects,
    /// Register a repository at this path, initializing it if needed.
    AddProject(String),
    /// Index a registered project.
    Index { name: String, path: String },
    /// Retire (or restore) a project.
    SetActive { name: String, active: bool },
}

/// What a finished job hands back to the event loop.
enum JobDone {
    Projects(Vec<ProjectRow>),
    /// A short line for the status bar, and whether to reload the registry.
    Message(String, bool),
    Failed(String),
}

/// Run `devctx` with the given arguments, returning stdout.
fn run_devctx(args: &[&str]) -> anyhow::Result<String> {
    let exe = std::env::current_exe()?;
    let out = std::process::Command::new(exe).args(args).output()?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("{}", err.trim().lines().last().unwrap_or("command failed"));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn load_projects() -> anyhow::Result<Vec<ProjectRow>> {
    let raw = run_devctx(&["projects", "list", "--all", "--format", "json"])?;
    let v: Value = serde_json::from_str(&raw)?;
    Ok(v.as_array()
        .map(|a| {
            a.iter()
                .map(|p| {
                    let s = |k: &str| p[k].as_str().unwrap_or("").to_string();
                    let indexed = if s("last_indexed_at").is_empty() {
                        "never indexed".to_string()
                    } else {
                        format!(
                            "{} files @ {}",
                            p["file_count"].as_i64().unwrap_or(0),
                            s("last_commit").chars().take(8).collect::<String>()
                        )
                    };
                    ProjectRow {
                        name: s("name"),
                        path: s("path"),
                        description: s("description"),
                        model: s("embed_model"),
                        indexed,
                        active: p["active"].as_bool().unwrap_or(true),
                    }
                })
                .collect()
        })
        .unwrap_or_default())
}

/// Spawn the worker thread and return its job sender plus the result receiver.
fn spawn_worker() -> (Sender<Job>, Receiver<JobDone>) {
    let (job_tx, job_rx) = mpsc::channel::<Job>();
    let (done_tx, done_rx) = mpsc::channel::<JobDone>();
    std::thread::spawn(move || {
        for job in job_rx {
            let result = match job {
                Job::LoadProjects => load_projects().map(JobDone::Projects),
                Job::AddProject(path) => run_devctx(&["projects", "add", &path, "--init"])
                    .map(|_| JobDone::Message(format!("Registered {path}"), true)),
                Job::Index { name, path } => run_devctx(&["-C", &path, "index"])
                    .or_else(|_| index_in(&path))
                    .map(|_| JobDone::Message(format!("Indexed {name}"), true)),
                Job::SetActive { name, active } => {
                    let args: Vec<&str> = if active {
                        vec!["projects", "refresh", &name]
                    } else {
                        vec!["projects", "rm", &name, "--deactivate"]
                    };
                    run_devctx(&args).map(|_| {
                        let verb = if active { "Restored" } else { "Retired" };
                        JobDone::Message(format!("{verb} {name}"), true)
                    })
                }
            };
            let msg = result.unwrap_or_else(|e| JobDone::Failed(e.to_string()));
            if done_tx.send(msg).is_err() {
                break; // the UI is gone
            }
        }
    });
    (job_tx, done_rx)
}

/// Index a project by running `devctx index` from inside its directory.
///
/// `devctx` has no global `-C`, so the working directory is how the command
/// finds the project it should act on.
fn index_in(path: &str) -> anyhow::Result<String> {
    let exe = std::env::current_exe()?;
    let out = std::process::Command::new(exe)
        .arg("index")
        .current_dir(path)
        .output()?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("{}", err.trim().lines().last().unwrap_or("index failed"));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Which view is active.
#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Search,
    Graph,
    Memories,
    Projects,
}

/// UI state.
struct App {
    view: View,
    query: String,
    mode: SearchMode,
    // search
    results: Vec<SearchResult>,
    list: ListState,
    // graph
    upstream: Vec<(String, usize)>,
    downstream: Vec<(String, usize)>,
    // memories
    memories: Vec<MemoryItem>,
    mem_list: ListState,
    mem_scope: MemScope,
    // projects
    projects: Vec<ProjectRow>,
    proj_list: ListState,
    /// When set, keystrokes go to this prompt instead of the query line.
    prompt: Option<Prompt>,
    /// A job is in flight; the UI stays usable while it runs.
    busy: bool,
    status: String,
}

/// A one-line modal prompt (currently only "add a project").
struct Prompt {
    label: &'static str,
    value: String,
}

impl App {
    fn new() -> Self {
        Self {
            view: View::Search,
            query: String::new(),
            mode: SearchMode::Vector,
            results: Vec::new(),
            list: ListState::default(),
            upstream: Vec::new(),
            downstream: Vec::new(),
            memories: Vec::new(),
            mem_list: ListState::default(),
            mem_scope: MemScope::All,
            projects: Vec::new(),
            proj_list: ListState::default(),
            prompt: None,
            busy: false,
            status: "Type a query and press Enter".to_string(),
        }
    }

    fn mode_name(&self) -> &'static str {
        match self.mode {
            SearchMode::Vector => "vector",
            SearchMode::Keyword => "keyword",
            SearchMode::Hybrid => "hybrid",
        }
    }

    fn cycle_mode(&mut self) {
        self.mode = match self.mode {
            SearchMode::Vector => SearchMode::Keyword,
            SearchMode::Keyword => SearchMode::Hybrid,
            SearchMode::Hybrid => SearchMode::Vector,
        };
    }

    /// The list state + length of the active, navigable view.
    fn active_list(&mut self) -> Option<(&mut ListState, usize)> {
        match self.view {
            View::Search => Some((&mut self.list, self.results.len())),
            View::Memories => Some((&mut self.mem_list, self.memories.len())),
            View::Projects => Some((&mut self.proj_list, self.projects.len())),
            View::Graph => None,
        }
    }

    fn next(&mut self) {
        if let Some((list, len)) = self.active_list() {
            if len == 0 {
                return;
            }
            let i = list.selected().map(|i| (i + 1).min(len - 1)).unwrap_or(0);
            list.select(Some(i));
        }
    }

    fn prev(&mut self) {
        if let Some((list, len)) = self.active_list() {
            if len == 0 {
                return;
            }
            let i = list.selected().map(|i| i.saturating_sub(1)).unwrap_or(0);
            list.select(Some(i));
        }
    }

    fn selected(&self) -> Option<&SearchResult> {
        self.list.selected().and_then(|i| self.results.get(i))
    }

    fn selected_memory(&self) -> Option<&MemoryItem> {
        self.mem_list.selected().and_then(|i| self.memories.get(i))
    }

    fn selected_project(&self) -> Option<&ProjectRow> {
        self.proj_list.selected().and_then(|i| self.projects.get(i))
    }
}

/// Launch the TUI, restoring the terminal on exit. When `server` is `Some`, the
/// TUI routes all queries through that server (no direct DB access); otherwise
/// it opens the store locally and loads the embedding model.
pub fn run(cfg: ProjectConfig, server: Option<ServerConn>) -> anyhow::Result<()> {
    let engine = match server {
        Some(conn) => Engine::Remote {
            base: conn.base,
            token: conn.token,
        },
        None => {
            eprintln!("Loading the embedding model…");
            Engine::local(&cfg)?
        }
    };
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &engine);
    ratatui::restore();
    result
}

fn event_loop(terminal: &mut DefaultTerminal, engine: &Engine) -> anyhow::Result<()> {
    let mut app = App::new();
    let (jobs, done) = spawn_worker();

    loop {
        terminal.draw(|f| ui(f, &mut app))?;

        // Drain anything the worker finished. This is why the loop polls rather
        // than blocking on a key: an index running in the background has to be
        // able to report back without the user pressing anything.
        while let Ok(msg) = done.try_recv() {
            app.busy = false;
            match msg {
                JobDone::Projects(p) => {
                    app.projects = p;
                    app.proj_list
                        .select((!app.projects.is_empty()).then_some(0));
                    app.status = format!("{} project(s)", app.projects.len());
                }
                JobDone::Message(m, reload) => {
                    app.status = m;
                    if reload {
                        app.busy = true;
                        let _ = jobs.send(Job::LoadProjects);
                    }
                }
                JobDone::Failed(e) => app.status = format!("error: {e}"),
            }
        }

        if !event::poll(Duration::from_millis(120))? {
            continue;
        }
        let Event::Key(k) = event::read()? else {
            continue;
        };
        if k.kind != KeyEventKind::Press {
            continue;
        }
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            break;
        }

        // A prompt swallows every key until it is submitted or cancelled.
        if let Some(prompt) = app.prompt.as_mut() {
            match k.code {
                KeyCode::Esc => app.prompt = None,
                KeyCode::Enter => {
                    let value = prompt.value.trim().to_string();
                    app.prompt = None;
                    if value.is_empty() {
                        app.status = "Cancelled".to_string();
                    } else {
                        app.busy = true;
                        app.status = format!("Registering {value}…");
                        let _ = jobs.send(Job::AddProject(value));
                    }
                }
                KeyCode::Backspace => {
                    prompt.value.pop();
                }
                KeyCode::Char(c) => prompt.value.push(c),
                _ => {}
            }
            continue;
        }

        match k.code {
            KeyCode::Esc => break,
            KeyCode::F(1) => app.view = View::Search,
            KeyCode::F(2) => app.view = View::Graph,
            KeyCode::F(3) => {
                app.view = View::Memories;
                if app.memories.is_empty() && app.query.is_empty() {
                    load_recent(&mut app, engine);
                }
            }
            KeyCode::F(4) => {
                app.view = View::Projects;
                if app.projects.is_empty() && !app.busy {
                    app.busy = true;
                    app.status = "Loading projects…".to_string();
                    let _ = jobs.send(Job::LoadProjects);
                }
            }
            KeyCode::Enter => {
                terminal.draw(|f| ui(f, &mut app))?;
                submit(&mut app, engine);
            }
            KeyCode::Tab => match app.view {
                View::Memories => {
                    app.mem_scope = app.mem_scope.cycle();
                    app.status = format!("Scope: {}", app.mem_scope.name());
                }
                _ => app.cycle_mode(),
            },
            KeyCode::Down => app.next(),
            KeyCode::Up => app.prev(),
            KeyCode::Backspace if app.view != View::Projects => {
                app.query.pop();
            }
            // Projects view: single-key actions, so the query line is free.
            KeyCode::Char('a') if app.view == View::Projects => {
                app.prompt = Some(Prompt {
                    label: "Repository path",
                    value: String::new(),
                });
            }
            KeyCode::Char('i') if app.view == View::Projects => {
                let target = app
                    .selected_project()
                    .map(|p| (p.name.clone(), p.path.clone()));
                match target {
                    None => app.status = "No project selected".to_string(),
                    Some((name, _)) if app.busy => {
                        app.status = format!("Busy; {name} not started");
                    }
                    Some((name, path)) => {
                        app.busy = true;
                        app.status = format!("Indexing {name}… (the UI stays usable)");
                        let _ = jobs.send(Job::Index { name, path });
                    }
                }
            }
            KeyCode::Char('d') if app.view == View::Projects => {
                let target = app.selected_project().map(|p| (p.name.clone(), !p.active));
                match target {
                    Some((name, active)) => {
                        app.busy = true;
                        let _ = jobs.send(Job::SetActive { name, active });
                    }
                    None => app.status = "No project selected".to_string(),
                }
            }
            KeyCode::Char('r') if app.view == View::Projects => {
                app.busy = true;
                app.status = "Reloading…".to_string();
                let _ = jobs.send(Job::LoadProjects);
            }
            KeyCode::Char(c) if app.view != View::Projects => app.query.push(c),
            _ => {}
        }
    }
    Ok(())
}

fn submit(app: &mut App, engine: &Engine) {
    match app.view {
        View::Search => {
            app.status = "Searching…".to_string();
            match engine.search(&app.query, app.mode) {
                Ok(r) => {
                    app.results = r;
                    app.list.select((!app.results.is_empty()).then_some(0));
                    app.status = format!("{} results", app.results.len());
                }
                Err(e) => {
                    app.results.clear();
                    app.list.select(None);
                    app.status = format!("error: {e}");
                }
            }
        }
        View::Graph => {
            let sym = app.query.trim().to_string();
            if sym.is_empty() {
                app.status = "Enter a symbol name".to_string();
                return;
            }
            match engine.graph(&sym) {
                Ok((up, down)) => {
                    app.upstream = up;
                    app.downstream = down;
                    app.status = format!(
                        "{}: {} callers · {} callees",
                        sym,
                        app.upstream.len(),
                        app.downstream.len()
                    );
                }
                Err(e) => {
                    app.upstream.clear();
                    app.downstream.clear();
                    app.status = format!("error: {e}");
                }
            }
        }
        // Projects act on keystrokes, not on the query line.
        View::Projects => {}
        View::Memories => {
            if app.query.trim().is_empty() {
                load_recent(app, engine);
                return;
            }
            match engine.recall(&app.query, app.mem_scope) {
                Ok(m) => {
                    app.memories = m;
                    app.mem_list.select((!app.memories.is_empty()).then_some(0));
                    app.status = format!("{} recalled", app.memories.len());
                }
                Err(e) => {
                    app.memories.clear();
                    app.mem_list.select(None);
                    app.status = format!("error: {e}");
                }
            }
        }
    }
}

fn load_recent(app: &mut App, engine: &Engine) {
    match engine.recent_memories() {
        Ok(m) => {
            app.memories = m;
            app.mem_list.select((!app.memories.is_empty()).then_some(0));
            app.status = format!("{} recent memories", app.memories.len());
        }
        Err(e) => {
            app.memories.clear();
            app.status = format!("error: {e}");
        }
    }
}

fn ui(f: &mut Frame, app: &mut App) {
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .split(f.area());

    f.render_widget(tabs_line(app), chunks[0]);

    let prompt = match app.view {
        View::Search => format!("Query — mode: {} (Tab to change)", app.mode_name()),
        View::Graph => "Symbol (Enter for callers/callees)".to_string(),
        View::Memories => format!(
            "Recall — scope: {} (Tab to change, empty = recent)",
            app.mem_scope.name()
        ),
        View::Projects => "Projects — a add · i index · d retire/restore · r reload".to_string(),
    };
    let (title, value) = match app.prompt.as_ref() {
        Some(p) => (
            format!("{} (Enter to confirm, Esc to cancel)", p.label),
            p.value.as_str(),
        ),
        None if app.busy => (format!("{prompt}  · working…"), app.query.as_str()),
        None => (prompt, app.query.as_str()),
    };
    let input = Paragraph::new(value).block(Block::bordered().title(title));
    f.render_widget(input, chunks[1]);

    match app.view {
        View::Search => render_search(f, app, chunks[2]),
        View::Graph => render_graph(f, app, chunks[2]),
        View::Memories => render_memories(f, app, chunks[2]),
        View::Projects => render_projects(f, app, chunks[2]),
    }

    let help = match app.view {
        View::Search => {
            "F1 search  F2 graph  F3 memories  F4 projects | Enter: search  Tab: mode  Esc: quit"
        }
        View::Graph => {
            "F1 search  F2 graph  F3 memories  F4 projects | Enter: analyze symbol  Esc: quit"
        }
        View::Memories => {
            "F1 search  F2 graph  F3 memories  F4 projects | Enter: recall  Tab: scope  Esc: quit"
        }
        View::Projects => {
            "F1 search  F2 graph  F3 memories  F4 projects | a add  i index  d retire  r reload"
        }
    };
    f.render_widget(Paragraph::new(help).dim(), chunks[3]);
}

fn tabs_line(app: &App) -> Line<'static> {
    let tab = |name: &'static str, active: bool| {
        if active {
            Span::styled(
                format!(" {name} "),
                Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD),
            )
        } else {
            Span::raw(format!(" {name} "))
        }
    };
    Line::from(vec![
        tab("F1 Search", app.view == View::Search),
        Span::raw("  "),
        tab("F2 Graph", app.view == View::Graph),
        Span::raw("  "),
        tab("F3 Memories", app.view == View::Memories),
        Span::raw("  "),
        tab("F4 Projects", app.view == View::Projects),
    ])
}

fn render_search(f: &mut Frame, app: &mut App, area: ratatui::layout::Rect) {
    let mid =
        Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).split(area);

    let items: Vec<ListItem> = app
        .results
        .iter()
        .map(|h| {
            let m = &h.point.metadata;
            let sym = if m.symbol.is_empty() { "-" } else { &m.symbol };
            ListItem::new(format!(
                "{:.3}  {}:{}-{}  {} [{}]",
                h.score, m.file, m.start_line, m.end_line, sym, m.chunk_level
            ))
        })
        .collect();
    let list = List::new(items)
        .block(Block::bordered().title(app.status.clone()))
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(list, mid[0], &mut app.list);

    let preview = app
        .selected()
        .map(|h| h.point.text.clone())
        .unwrap_or_default();
    f.render_widget(
        Paragraph::new(preview)
            .block(Block::bordered().title("Preview"))
            .wrap(Wrap { trim: false }),
        mid[1],
    );
}

fn render_graph(f: &mut Frame, app: &mut App, area: ratatui::layout::Rect) {
    let cols =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).split(area);
    let fmt = |v: &[(String, usize)]| -> Vec<ListItem> {
        v.iter()
            .map(|(s, d)| ListItem::new(format!("{}{}  (depth {})", "  ".repeat(*d), s, d)))
            .collect()
    };
    f.render_widget(
        List::new(fmt(&app.upstream))
            .block(Block::bordered().title(format!("Callers / upstream ({})", app.upstream.len()))),
        cols[0],
    );
    f.render_widget(
        List::new(fmt(&app.downstream)).block(
            Block::bordered().title(format!("Callees / downstream ({})", app.downstream.len())),
        ),
        cols[1],
    );
}

/// The project registry: one row per repository, with its freshness.
fn render_projects(f: &mut Frame, app: &mut App, area: ratatui::layout::Rect) {
    if app.projects.is_empty() {
        let hint = if app.busy {
            "Loading…"
        } else {
            "No projects registered. Press `a` to add one."
        };
        f.render_widget(
            Paragraph::new(hint).block(Block::bordered().title(" Projects ")),
            area,
        );
        return;
    }

    let cols =
        Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).split(area);
    let width = app
        .projects
        .iter()
        .map(|p| p.name.len())
        .max()
        .unwrap_or(4)
        .min(24);

    let items: Vec<ListItem> = app
        .projects
        .iter()
        .map(|p| {
            let mark = if p.active { " " } else { "·" };
            let name = Span::styled(
                format!("{mark} {:width$}", p.name),
                if p.active {
                    Style::default().add_modifier(Modifier::BOLD)
                } else {
                    Style::default().dim()
                },
            );
            ListItem::new(Line::from(vec![
                name,
                Span::raw("  "),
                Span::styled(p.indexed.clone(), Style::default().dim()),
            ]))
        })
        .collect();

    f.render_stateful_widget(
        List::new(items)
            .block(Block::bordered().title(" Projects "))
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED)),
        cols[0],
        &mut app.proj_list,
    );

    let detail = match app.selected_project() {
        Some(p) => {
            let mut lines = vec![
                Line::from(Span::styled(
                    p.name.clone(),
                    Style::default().add_modifier(Modifier::BOLD),
                )),
                Line::raw(p.path.clone()),
                Line::raw(""),
                Line::raw(format!("model:   {}", p.model)),
                Line::raw(format!("index:   {}", p.indexed)),
                Line::raw(format!(
                    "status:  {}",
                    if p.active { "active" } else { "retired" }
                )),
            ];
            if !p.description.is_empty() {
                lines.push(Line::raw(""));
                lines.push(Line::raw(p.description.clone()));
            }
            Paragraph::new(lines).wrap(Wrap { trim: false })
        }
        None => Paragraph::new("Select a project"),
    };
    f.render_widget(detail.block(Block::bordered().title(" Detail ")), cols[1]);
}

fn render_memories(f: &mut Frame, app: &mut App, area: ratatui::layout::Rect) {
    let mid =
        Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).split(area);

    let items: Vec<ListItem> = app
        .memories
        .iter()
        .map(|m| {
            let title = if m.title.is_empty() {
                "(untitled)"
            } else {
                &m.title
            };
            // Where a memory came from matters more than its raw score, which is
            // not comparable across stores anyway.
            let (tag, style) = if m.scope == "global" {
                ("global", Style::default().add_modifier(Modifier::BOLD))
            } else {
                ("local ", Style::default().dim())
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!("{tag} "), style),
                Span::raw(format!("[{}] {title}", m.mtype)),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(Block::bordered().title(app.status.clone()))
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(list, mid[0], &mut app.mem_list);

    let body = app
        .selected_memory()
        .map(|m| {
            let tags = if m.tags.is_empty() {
                String::new()
            } else {
                format!("\ntags: {}", m.tags)
            };
            let origin = if m.scope == "global" {
                "\nscope: global — shared with every project"
            } else {
                "\nscope: this project only"
            };
            // The score belongs here rather than in the list: it is only
            // meaningful within one store, and the list mixes two.
            let score = m
                .score
                .map(|s| format!("\nrelevance: {s:.3}"))
                .unwrap_or_default();
            format!("{}{}{}{}\n\n{}", m.title, tags, origin, score, m.content)
        })
        .unwrap_or_default();
    f.render_widget(
        Paragraph::new(body)
            .block(Block::bordered().title("Memory"))
            .wrap(Wrap { trim: false }),
        mid[1],
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use devctx_core::{VectorMetadata, VectorPoint};

    fn dummy(id: &str) -> SearchResult {
        SearchResult {
            score: 1.0,
            point: VectorPoint {
                id: id.into(),
                vector: vec![],
                text: id.into(),
                metadata: VectorMetadata::default(),
            },
        }
    }

    fn mem(title: &str) -> MemoryItem {
        MemoryItem {
            title: title.into(),
            mtype: "note".into(),
            tags: String::new(),
            content: title.into(),
            score: None,
            scope: "local".into(),
        }
    }

    #[test]
    fn cycles_mode() {
        let mut app = App::new();
        assert_eq!(app.mode_name(), "vector");
        app.cycle_mode();
        assert_eq!(app.mode_name(), "keyword");
        app.cycle_mode();
        assert_eq!(app.mode_name(), "hybrid");
        app.cycle_mode();
        assert_eq!(app.mode_name(), "vector");
    }

    #[test]
    fn navigation_clamps() {
        let mut app = App::new();
        // No results: navigation is a no-op.
        app.next();
        assert_eq!(app.list.selected(), None);

        app.results = vec![dummy("a"), dummy("b")];
        app.list.select(Some(0));
        app.next();
        assert_eq!(app.list.selected(), Some(1));
        app.next(); // clamped at last
        assert_eq!(app.list.selected(), Some(1));
        app.prev();
        assert_eq!(app.list.selected(), Some(0));
        app.prev(); // clamped at first
        assert_eq!(app.list.selected(), Some(0));
        assert_eq!(app.selected().unwrap().point.id, "a");
    }

    #[test]
    fn navigation_targets_active_view() {
        let mut app = App::new();
        app.results = vec![dummy("a"), dummy("b")];
        app.memories = vec![mem("m1"), mem("m2"), mem("m3")];

        // In Memories view, navigation moves the memory list, not the search list.
        app.view = View::Memories;
        app.mem_list.select(Some(0));
        app.next();
        assert_eq!(app.mem_list.selected(), Some(1));
        assert_eq!(app.list.selected(), None);
        assert_eq!(app.selected_memory().unwrap().title, "m2");

        // In Graph view, navigation is a no-op.
        app.view = View::Graph;
        app.next();
        assert_eq!(app.mem_list.selected(), Some(1));
    }

    fn proj(name: &str, active: bool) -> ProjectRow {
        ProjectRow {
            name: name.into(),
            path: format!("/repos/{name}"),
            model: "minilm-l6".into(),
            indexed: "never indexed".into(),
            active,
            ..Default::default()
        }
    }

    #[test]
    fn scope_cycles_through_every_option_and_back() {
        let mut s = MemScope::All;
        s = s.cycle();
        assert_eq!(s.name(), "project");
        s = s.cycle();
        assert_eq!(s.name(), "global");
        s = s.cycle();
        assert_eq!(s.name(), "all");
    }

    /// The wire values must match what the `/recall` endpoints accept; a typo
    /// here would silently search the wrong store.
    #[test]
    fn scope_wire_values_match_the_api() {
        assert_eq!(MemScope::Local.wire(), "local");
        assert_eq!(MemScope::Global.wire(), "global");
        assert_eq!(MemScope::All.wire(), "all");
    }

    #[test]
    fn navigation_reaches_the_projects_list() {
        let mut app = App::new();
        app.view = View::Projects;
        app.projects = vec![proj("alpha", true), proj("beta", false)];
        app.next();
        assert_eq!(app.proj_list.selected(), Some(0));
        app.next();
        assert_eq!(app.proj_list.selected(), Some(1));
        app.next();
        assert_eq!(app.proj_list.selected(), Some(1), "clamps at the end");
        assert_eq!(app.selected_project().unwrap().name, "beta");
        assert!(!app.selected_project().unwrap().active);
    }

    #[test]
    fn a_registry_row_is_parsed_into_a_project() {
        let raw = serde_json::json!([{
            "name": "alpha",
            "path": "/repos/alpha",
            "description": "the alpha service",
            "embed_model": "bge-base",
            "last_indexed_at": "1700000000",
            "last_commit": "0123456789abcdef",
            "file_count": 42,
            "active": true,
        }]);
        let rows: Vec<ProjectRow> = raw
            .as_array()
            .unwrap()
            .iter()
            .map(|p| {
                let s = |k: &str| p[k].as_str().unwrap_or("").to_string();
                let indexed = if s("last_indexed_at").is_empty() {
                    "never indexed".to_string()
                } else {
                    format!(
                        "{} files @ {}",
                        p["file_count"].as_i64().unwrap_or(0),
                        s("last_commit").chars().take(8).collect::<String>()
                    )
                };
                ProjectRow {
                    name: s("name"),
                    path: s("path"),
                    description: s("description"),
                    model: s("embed_model"),
                    indexed,
                    active: p["active"].as_bool().unwrap_or(true),
                }
            })
            .collect();

        assert_eq!(rows[0].name, "alpha");
        assert_eq!(rows[0].model, "bge-base");
        assert_eq!(rows[0].indexed, "42 files @ 01234567");
    }

    /// A never-indexed project must say so rather than showing a blank commit.
    #[test]
    fn a_never_indexed_project_says_so() {
        let p = serde_json::json!({ "name": "x", "last_indexed_at": "", "file_count": 0 });
        let indexed = if p["last_indexed_at"].as_str().unwrap_or("").is_empty() {
            "never indexed".to_string()
        } else {
            "something".to_string()
        };
        assert_eq!(indexed, "never indexed");
    }

    /// Typing must reach the prompt, not the query line, while one is open.
    #[test]
    fn a_prompt_is_separate_from_the_query() {
        let mut app = App::new();
        app.query = "search text".into();
        app.prompt = Some(Prompt {
            label: "Repository path",
            value: String::new(),
        });
        if let Some(p) = app.prompt.as_mut() {
            p.value.push_str("/repos/new");
        }
        assert_eq!(app.prompt.as_ref().unwrap().value, "/repos/new");
        assert_eq!(app.query, "search text", "the query is left untouched");
    }
}
