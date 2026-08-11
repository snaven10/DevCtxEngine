//! `devctx` — command-line entry point for the DevCtxEngine Rust rewrite.
//!
//! F5 wires the real pipeline: `init`, `status`, `index`, `search`. Building the
//! embedder pulls in fastembed/ort (the `local` provider) and downloads the
//! model on first use.

mod central_remote;
mod mcp_configure;
mod remote;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use devctx_central::{project_json, Central, CentralPaths, RegisterRequest};
use devctx_core::config::{find_config_file, Project, ProjectConfig};
use devctx_core::{SearchFilter, SearchResult};
use devctx_embed::{create_provider, EmbedSettings, EmbeddingProvider};
use devctx_index::{run as index_run, IndexRequest, ProgressSink};
use devctx_memory::{memory_stats, recall, remember, RememberRequest};
use devctx_rerank::{create_reranker, RerankSettings};
use devctx_search::SearchMode;
use devctx_store::Store;
use devctx_summarize::{create_summarizer, SummarizeSettings};
use mcp_configure::{McpClient, Options, Scope};
use serde::Serialize;

/// Default bind address for the project server (and the `--addr` default that
/// `serve --central` overrides with a per-home port).
const DEFAULT_ADDR: &str = "127.0.0.1:8080";

/// Git-aware AI code intelligence tool.
#[derive(Debug, Parser)]
#[command(name = "devctx", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Initialize DevCtxEngine tracking for a repository.
    Init {
        /// Repository path (defaults to the current directory).
        path: Option<PathBuf>,
        /// Project name (defaults to the directory name).
        #[arg(long)]
        name: Option<String>,
    },
    /// Show repository and index status.
    Status,
    /// Index the current repository (git diff → parse → chunk → embed → store).
    Index {
        /// Force a full reindex instead of incremental.
        #[arg(long)]
        full: bool,
    },
    /// Semantic search across the indexed code.
    Search {
        /// The search query.
        query: String,
        /// Maximum results.
        #[arg(long, default_value_t = 10)]
        limit: usize,
        /// Restrict to a language (store `language` value).
        #[arg(long)]
        language: Option<String>,
        /// Output format.
        #[arg(long, value_enum, default_value_t = OutputFormat::Table)]
        format: OutputFormat,
        /// Disable cross-encoder reranking (return raw vector-search order).
        #[arg(long)]
        no_rerank: bool,
        /// Keyword (BM25) search instead of semantic vector search.
        #[arg(long)]
        keyword: bool,
        /// Hybrid search: fuse vector + keyword (BM25) via reciprocal rank fusion.
        #[arg(long)]
        hybrid: bool,
    },
    /// Run the MCP server over stdio, or `mcp configure` a client.
    Mcp {
        /// Project root for the server (defaults to discovery from the cwd).
        #[arg(long)]
        project: Option<PathBuf>,
        #[command(subcommand)]
        action: Option<McpAction>,
    },
    /// Save a memory (deduplicated by topic key or content).
    Remember {
        /// The memory content.
        content: String,
        /// Short title.
        #[arg(long)]
        title: Option<String>,
        /// Memory type (decision/note/bug/insight/architecture/…).
        #[arg(long = "type", default_value = "note")]
        memory_type: String,
        /// Topic key (upsert-by-topic).
        #[arg(long)]
        topic: Option<String>,
        /// Comma-separated tags.
        #[arg(long)]
        tags: Option<String>,
    },
    /// Recall memories relevant to a query.
    Recall {
        /// The query.
        query: String,
        /// Maximum results.
        #[arg(long, default_value_t = 5)]
        limit: usize,
    },
    /// Show memory counts for the project.
    MemoryStats,
    /// Import memories from an old DevAI SQLite DB into DuckDB (re-embedded).
    Migrate {
        /// Old SQLite DB path (default: this project's `.devai`, then the global one).
        #[arg(long)]
        from: Option<PathBuf>,
        /// List what would be imported without writing anything.
        #[arg(long)]
        dry_run: bool,
        /// Keep each memory's original project name instead of the current one.
        #[arg(long)]
        keep_project: bool,
    },
    /// Show the blast radius (transitive callers/callees) of a symbol.
    Impact {
        /// The symbol to analyze.
        symbol: String,
        /// Traversal depth.
        #[arg(long, default_value_t = 3)]
        depth: usize,
    },
    /// List framework-aware HTTP routes.
    Routes {
        /// Filter by HTTP method.
        #[arg(long)]
        method: Option<String>,
        /// Filter by path substring.
        #[arg(long)]
        path: Option<String>,
    },
    /// Summarize a file (extractive by default; query-focusable).
    Summarize {
        /// File to summarize.
        path: PathBuf,
        /// Focus the summary on a query.
        #[arg(long)]
        query: Option<String>,
        /// Target length in tokens (overrides config).
        #[arg(long)]
        tokens: Option<usize>,
    },
    /// Serve the HTTP REST API.
    Api {
        /// Address to bind (host:port).
        #[arg(long, default_value = DEFAULT_ADDR)]
        addr: String,
        /// Bearer token required on all routes except /health (or DEVCTX_API_TOKEN).
        #[arg(long)]
        token: Option<String>,
    },
    /// Open the interactive terminal UI (search, graph & memories).
    Tui,
    /// Serve the web dashboard (call-graph + memories) in a browser.
    Web {
        /// Address to bind (host:port).
        #[arg(long, default_value = DEFAULT_ADDR)]
        addr: String,
        /// Don't try to open the dashboard in a browser.
        #[arg(long)]
        no_open: bool,
    },
    /// Run the long-lived server that owns the DB; other commands route to it.
    Serve {
        /// Address to bind (host:port).
        #[arg(long, default_value = DEFAULT_ADDR)]
        addr: String,
        /// Bearer token required on all routes except /health (or DEVCTX_API_TOKEN).
        #[arg(long)]
        token: Option<String>,
        /// Exit after this many seconds with no request (0 = never; used by auto-spawn).
        #[arg(long, default_value_t = 0)]
        idle: u64,
        /// Stop a running server for this project instead of starting one.
        #[arg(long)]
        stop: bool,
        /// Own the central store (registry + global memories) instead of a project.
        #[arg(long)]
        central: bool,
    },
    /// Manage the central registry of projects DevCtxEngine knows about.
    Projects {
        #[command(subcommand)]
        action: ProjectsAction,
    },
}

/// Actions under `devctx projects`.
#[derive(Debug, Subcommand)]
enum ProjectsAction {
    /// Register a repository (or update its entry).
    Add {
        /// Repository path (defaults to the current directory).
        path: Option<PathBuf>,
        /// Project name (defaults to the config's name, then the directory).
        #[arg(long)]
        name: Option<String>,
        /// Description, so an agent can pick a project without opening it.
        #[arg(long, default_value = "")]
        description: String,
        /// Comma-separated tags.
        #[arg(long, default_value = "")]
        tags: String,
        /// Create `.devctx/config.yaml` from the central defaults when missing.
        #[arg(long)]
        init: bool,
    },
    /// List registered projects.
    List {
        /// Include deactivated projects.
        #[arg(long)]
        all: bool,
        /// Print JSON instead of a table.
        #[arg(long, value_enum, default_value_t = OutputFormat::Table)]
        format: OutputFormat,
    },
    /// Show one project in full.
    Show {
        /// Project name.
        name: String,
    },
    /// Re-read a project's `.devctx/config.yaml` into the registry.
    Refresh {
        /// Project name.
        name: String,
    },
    /// Remove a project from the registry (the repository is left untouched).
    Rm {
        /// Project name.
        name: String,
        /// Deactivate instead of deleting, keeping the row and its history.
        #[arg(long)]
        deactivate: bool,
    },
}

/// Search output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum OutputFormat {
    /// Human-readable table.
    Table,
    /// JSON array.
    Json,
}

/// Actions under `devctx mcp`.
#[derive(Debug, Subcommand)]
enum McpAction {
    /// Register DevCtxEngine as an MCP server in an AI client.
    Configure {
        /// Target client.
        #[arg(long, value_enum, default_value_t = McpClient::ClaudeCode)]
        client: McpClient,
        /// Config scope.
        #[arg(long, value_enum, default_value_t = Scope::Project)]
        scope: Scope,
        /// Server name under `mcpServers`.
        #[arg(long, default_value = "devctx")]
        name: String,
        /// Remove the entry instead of adding it.
        #[arg(long)]
        remove: bool,
        /// Print the resulting config without writing it.
        #[arg(long)]
        show: bool,
        /// Extra env entries (`KEY=VALUE`), repeatable.
        #[arg(long = "env")]
        envs: Vec<String>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Init { path, name } => cmd_init(path, name),
        Command::Status => cmd_status(),
        Command::Index { full } => cmd_index(full),
        Command::Search {
            query,
            limit,
            language,
            format,
            no_rerank,
            keyword,
            hybrid,
        } => cmd_search(query, limit, language, format, no_rerank, keyword, hybrid),
        Command::Mcp { project, action } => match action {
            None => cmd_mcp(project),
            Some(McpAction::Configure {
                client,
                scope,
                name,
                remove,
                show,
                envs,
            }) => cmd_mcp_configure(client, scope, name, remove, show, envs),
        },
        Command::Remember {
            content,
            title,
            memory_type,
            topic,
            tags,
        } => cmd_remember(content, title, memory_type, topic, tags),
        Command::Recall { query, limit } => cmd_recall(query, limit),
        Command::MemoryStats => cmd_memory_stats(),
        Command::Migrate {
            from,
            dry_run,
            keep_project,
        } => cmd_migrate(from, dry_run, keep_project),
        Command::Impact { symbol, depth } => cmd_impact(symbol, depth),
        Command::Routes { method, path } => cmd_routes(method, path),
        Command::Summarize {
            path,
            query,
            tokens,
        } => cmd_summarize(path, query, tokens),
        Command::Api { addr, token } => cmd_api(addr, token),
        Command::Tui => cmd_tui(),
        Command::Web { addr, no_open } => cmd_web(addr, no_open),
        Command::Serve {
            addr,
            token,
            idle,
            stop,
            central,
        } => {
            if central {
                cmd_serve_central(addr, token, idle, stop)
            } else {
                cmd_serve(addr, token, idle, stop)
            }
        }
        Command::Projects { action } => cmd_projects(action),
    }
}

/// `devctx projects` — the central registry.
fn cmd_projects(action: ProjectsAction) -> Result<()> {
    let paths = CentralPaths::resolve().context("resolving the central store location")?;
    // Route through the daemon so concurrent commands don't fight over the
    // single-writer DuckDB file. Falling back to a direct open keeps a lone
    // command working when no daemon can be spawned.
    let remote = central_remote::ensure(&paths);
    let direct = || Central::open().context("opening the central store");

    match action {
        ProjectsAction::Add {
            path,
            name,
            description,
            tags,
            init,
        } => {
            let root = match path {
                Some(p) => p,
                None => std::env::current_dir().context("resolving current directory")?,
            };
            let rec = match &remote {
                Some(r) => r.add(&root, name.as_deref(), &description, &tags, init)?,
                None => project_json(&direct()?.register(&RegisterRequest {
                    root,
                    name,
                    description,
                    tags,
                    create_config: init,
                    now: devctx_central::now_stamp(),
                })?),
            };
            println!(
                "Registered `{}` at {}",
                field(&rec, "name"),
                field(&rec, "path")
            );
            println!(
                "  Model:  {} ({}, {}d)",
                field(&rec, "embed_model"),
                field(&rec, "embed_provider"),
                num(&rec, "embed_dim")
            );
            println!("  Store:  {}", field(&rec, "db_path"));
            if field(&rec, "last_indexed_at").is_empty() {
                println!("  Index:  not indexed yet — run `devctx index` in the repo");
            }
            Ok(())
        }

        ProjectsAction::List { all, format } => {
            let projects = match &remote {
                Some(r) => r.list(all)?,
                None => direct()?.list(all)?.iter().map(project_json).collect(),
            };
            if format == OutputFormat::Json {
                println!("{}", serde_json::to_string_pretty(&projects)?);
                return Ok(());
            }
            if projects.is_empty() {
                println!("No projects registered (run `devctx projects add`).");
                return Ok(());
            }
            let width = projects
                .iter()
                .map(|p| field(p, "name").len())
                .max()
                .unwrap_or(4);
            for p in &projects {
                let indexed = if field(p, "last_indexed_at").is_empty() {
                    "never indexed".to_string()
                } else {
                    format!(
                        "{} files @ {}",
                        num(p, "file_count"),
                        short_commit(&field(p, "last_commit"))
                    )
                };
                let flag = if p.get("active").and_then(|v| v.as_bool()).unwrap_or(true) {
                    " "
                } else {
                    "-"
                };
                println!(
                    "{flag} {:width$}  {:<12} {:<22} {}",
                    field(p, "name"),
                    field(p, "embed_model"),
                    indexed,
                    field(p, "path")
                );
            }
            Ok(())
        }

        ProjectsAction::Show { name } => {
            let p = match &remote {
                Some(r) => r.show(&name)?,
                None => match direct()?.get(&name)? {
                    Some(rec) => project_json(&rec),
                    None => bail!("no registered project named `{name}`"),
                },
            };
            println!("{}", field(&p, "name"));
            println!("  Path:        {}", field(&p, "path"));
            println!("  Config:      {}", field(&p, "config_path"));
            println!("  Store:       {}", field(&p, "db_path"));
            println!(
                "  Model:       {} ({}, {}d)",
                field(&p, "embed_model"),
                field(&p, "embed_provider"),
                num(&p, "embed_dim")
            );
            if !field(&p, "description").is_empty() {
                println!("  Description: {}", field(&p, "description"));
            }
            if !field(&p, "tags").is_empty() {
                println!("  Tags:        {}", field(&p, "tags"));
            }
            if field(&p, "last_indexed_at").is_empty() {
                println!("  Index:       never indexed");
            } else {
                println!(
                    "  Index:       {} files, {} symbols, {} chunks",
                    num(&p, "file_count"),
                    num(&p, "symbol_count"),
                    num(&p, "chunk_count")
                );
                println!(
                    "  Last:        {} on {}",
                    short_commit(&field(&p, "last_commit")),
                    field(&p, "last_branch")
                );
            }
            println!(
                "  Active:      {}",
                p.get("active").and_then(|v| v.as_bool()).unwrap_or(true)
            );
            Ok(())
        }

        ProjectsAction::Refresh { name } => {
            let rec = match &remote {
                Some(r) => r.refresh(&name)?,
                None => project_json(&direct()?.refresh(&name, &devctx_central::now_stamp())?),
            };
            println!(
                "Refreshed `{}` from {} — model {} ({}d)",
                field(&rec, "name"),
                field(&rec, "config_path"),
                field(&rec, "embed_model"),
                num(&rec, "embed_dim")
            );
            Ok(())
        }

        ProjectsAction::Rm { name, deactivate } => {
            match &remote {
                Some(r) => {
                    r.remove(&name, deactivate)?;
                }
                None => {
                    let c = direct()?;
                    let done = if deactivate {
                        c.deactivate(&name, &devctx_central::now_stamp())?
                    } else {
                        c.remove(&name)?
                    };
                    if !done {
                        bail!("no registered project named `{name}`");
                    }
                }
            }
            let verb = if deactivate { "Deactivated" } else { "Removed" };
            println!("{verb} `{name}` (the repository itself was not touched)");
            Ok(())
        }
    }
}

/// A string field of a registry row, empty when absent.
fn field(v: &serde_json::Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_string()
}

/// A numeric field of a registry row.
fn num(v: &serde_json::Value, key: &str) -> i64 {
    v.get(key).and_then(|x| x.as_i64()).unwrap_or(0)
}

/// `devctx tui` — open the interactive terminal UI.
fn cmd_tui() -> Result<()> {
    let cfg = load_project()?;
    // Route through the server (auto-spawned if needed) so the TUI never opens
    // the DB itself and coexists with other processes. Fall back to local only
    // when no server is available.
    let server = match remote::ensure(&cfg) {
        Some(r) => {
            let (base, token) = r.into_parts();
            Some(devctx_tui::ServerConn { base, token })
        }
        None => {
            remote::reclaim_db(&cfg);
            None
        }
    };
    devctx_tui::run(cfg, server)
}

/// `devctx api` — serve the HTTP REST API.
fn cmd_api(addr: String, token: Option<String>) -> Result<()> {
    let cfg = load_project()?;
    let socket = addr
        .parse()
        .with_context(|| format!("invalid --addr `{addr}`"))?;
    let token = token.or_else(|| std::env::var("DEVCTX_API_TOKEN").ok());
    remote::reclaim_db(&cfg); // take over the DB from any auto-spawned daemon
    remote::write_serve_file(&cfg, socket, token.as_deref())?;
    let result = devctx_api::run_blocking(cfg.clone(), socket, token, None);
    remote::remove_serve_file(&cfg);
    result
}

/// `devctx serve` — the long-lived owner of the DB. Advertises itself in a
/// discovery file so other `devctx` commands route through it (no lock fights).
fn cmd_serve(addr: String, token: Option<String>, idle: u64, stop: bool) -> Result<()> {
    let cfg = load_project()?;
    if stop {
        return remote::stop_server(&cfg);
    }
    remote::reclaim_db(&cfg); // replace any auto-spawned daemon
    let socket: SocketAddr = addr
        .parse()
        .with_context(|| format!("invalid --addr `{addr}`"))?;
    let token = token.or_else(|| std::env::var("DEVCTX_API_TOKEN").ok());

    remote::write_serve_file(&cfg, socket, token.as_deref())?;
    println!("DevCtxEngine server (owns the DB) → http://{addr}");
    println!("Other `devctx` commands will route through it while it runs. Ctrl-C to stop.");

    let idle = (idle > 0).then(|| std::time::Duration::from_secs(idle));
    let result = devctx_api::run_blocking(cfg.clone(), socket, token, idle);
    remote::remove_serve_file(&cfg);
    result
}

/// `devctx serve --central` — the single owner of the central store.
///
/// Unlike a project server this is a singleton: the central database is shared
/// by every project, and DuckDB permits one writing process per file. Binding a
/// second one is therefore refused rather than raced.
fn cmd_serve_central(addr: String, token: Option<String>, idle: u64, stop: bool) -> Result<()> {
    let paths = CentralPaths::resolve().context("resolving the central store location")?;
    if stop {
        return central_remote::stop(&paths);
    }
    if central_remote::discover(&paths).is_some() {
        bail!(
            "a central daemon is already running for {} (stop it with \
             `devctx serve --central --stop`)",
            paths.dir.display()
        );
    }

    // `--addr` defaults to the project-server port, which would be wrong here;
    // when it is left at that default, use the per-home address instead.
    let addr = if addr == DEFAULT_ADDR {
        central_remote::default_addr(&paths)
    } else {
        addr
    };
    let socket: SocketAddr = addr
        .parse()
        .with_context(|| format!("invalid --addr `{addr}`"))?;
    let token = token.or_else(|| std::env::var("DEVCTX_API_TOKEN").ok());

    let central = Central::open().context("opening the central store")?;
    central_remote::write_serve_file(&paths, socket, token.as_deref())?;
    println!("DevCtxEngine central store → http://{addr}");
    println!("  Database: {}", paths.db.display());
    println!("  Config:   {}", paths.config.display());
    println!("Registry commands will route through it while it runs. Ctrl-C to stop.");

    let idle = (idle > 0).then(|| std::time::Duration::from_secs(idle));
    let result = devctx_api::central::run_blocking(central, socket, token, idle);
    central_remote::remove_serve_file(&paths);
    result
}

/// `devctx web` — serve the web dashboard (call-graph + memories) locally.
fn cmd_web(addr: String, no_open: bool) -> Result<()> {
    let cfg = load_project()?;
    // A running server (incl. an auto-spawned one) already serves the dashboard
    // and owns the DB — reuse it instead of trying to bind a second owner.
    if let Some(url) = remote::running_server_url(&cfg) {
        println!("Reusing the running server → {url}");
        if !no_open {
            open_browser(&url);
        }
        return Ok(());
    }
    let socket = addr
        .parse()
        .with_context(|| format!("invalid --addr `{addr}`"))?;
    let url = format!("http://{addr}");
    println!("DevCtxEngine dashboard → {url}");
    if !no_open {
        // Best-effort: open the default browser; ignore failures (headless/CI).
        open_browser(&url);
    }
    // Advertise so other commands (TUI/CLI) discover and route to this server.
    remote::reclaim_db(&cfg);
    remote::write_serve_file(&cfg, socket, None)?;
    // No token: the dashboard runs locally and needs unauthenticated access.
    let result = devctx_api::run_blocking(cfg.clone(), socket, None, None);
    remote::remove_serve_file(&cfg);
    result
}

/// Best-effort open of a URL in the platform browser.
fn open_browser(url: &str) {
    #[cfg(target_os = "linux")]
    let cmd = "xdg-open";
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(target_os = "windows")]
    let cmd = "explorer";
    let _ = std::process::Command::new(cmd)
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// `devctx mcp` — run the MCP server over stdio.
fn cmd_mcp(project: Option<PathBuf>) -> Result<()> {
    let cfg = match project {
        Some(root) => ProjectConfig::load(&root.join(devctx_core::CONFIG_FILE_NAME))
            .with_context(|| format!("loading project at {}", root.display()))?,
        None => load_project()?,
    };
    // Route through a shared server (auto-spawned if needed) so many MCP
    // sessions + web/TUI/CLI of the same project coexist without lock fights.
    // Fall back to owning the DB locally only when no server is available.
    let server = match remote::ensure(&cfg) {
        Some(r) => {
            let (base, token) = r.into_parts();
            eprintln!("Starting DevCtxEngine MCP server (stdio) → routing to {base}");
            Some(devctx_mcp::ServerConn { base, token })
        }
        None => {
            remote::reclaim_db(&cfg);
            eprintln!("Starting DevCtxEngine MCP server (stdio, local DB)…");
            None
        }
    };
    devctx_mcp::run_stdio(cfg, server)
}

/// `devctx mcp configure` — register DevCtxEngine as an MCP server in an AI client.
fn cmd_mcp_configure(
    client: McpClient,
    scope: Scope,
    name: String,
    remove: bool,
    show: bool,
    envs: Vec<String>,
) -> Result<()> {
    let cfg = load_project()?;
    let project_root = project_root(&cfg)?;
    let exe = std::env::current_exe().context("resolving the devctx binary path")?;
    mcp_configure::run(&Options {
        client,
        scope,
        name,
        project_root,
        exe,
        envs,
        remove,
        show,
    })
}

/// `devctx remember` — save a memory.
fn cmd_remember(
    content: String,
    title: Option<String>,
    memory_type: String,
    topic: Option<String>,
    tags: Option<String>,
) -> Result<()> {
    let cfg = load_project()?;
    if let Some(r) = remote::ensure(&cfg) {
        println!(
            "{}",
            r.remember(
                &content,
                title.as_deref().unwrap_or(""),
                &memory_type,
                topic.as_deref().unwrap_or(""),
                tags.as_deref().unwrap_or(""),
            )?
        );
        return Ok(());
    }
    let embedder = build_embedder(&cfg)?;
    let store = open_store(&cfg, embedder.dimension())?;

    let req = RememberRequest {
        title: title.unwrap_or_default(),
        content,
        memory_type,
        project: project_name(&cfg),
        topic_key: topic.unwrap_or_default(),
        tags: tags.unwrap_or_default(),
        now: now_epoch(),
        ..Default::default()
    };
    let res = remember(&store, embedder.as_ref(), &req)?;
    let title = if res.memory.title.is_empty() {
        "(untitled)"
    } else {
        &res.memory.title
    };
    println!("{:?}: {} — {}", res.status, res.memory.id, title);
    Ok(())
}

/// `devctx recall` — recall memories relevant to a query.
fn cmd_recall(query: String, limit: usize) -> Result<()> {
    let cfg = load_project()?;
    if let Some(r) = remote::ensure(&cfg) {
        println!("{}", r.recall(&query, limit)?);
        return Ok(());
    }
    let embedder = build_embedder(&cfg)?;
    let store = open_store(&cfg, embedder.dimension())?;

    let hits = recall(
        &store,
        embedder.as_ref(),
        &query,
        Some(&project_name(&cfg)),
        limit,
    )?;
    if hits.is_empty() {
        println!("No memories.");
        return Ok(());
    }
    for h in &hits {
        let m = &h.memory;
        let title = if m.title.is_empty() {
            "(untitled)"
        } else {
            &m.title
        };
        println!("{:.3}  [{}] {}", h.score, m.memory_type, title);
        println!("        {}", snippet(&m.content, 100));
    }
    Ok(())
}

/// `devctx impact` — show the blast radius of a symbol.
fn cmd_impact(symbol: String, depth: usize) -> Result<()> {
    let cfg = load_project()?;
    if let Some(r) = remote::ensure(&cfg) {
        let json: serde_json::Value = serde_json::from_str(&r.impact(&symbol, depth)?)?;
        println!("Impact of `{symbol}` (depth {depth}):");
        print_impact("callers (upstream)", &json_depth_pairs(&json["upstream"]));
        print_impact(
            "callees (downstream)",
            &json_depth_pairs(&json["downstream"]),
        );
        return Ok(());
    }
    let store = open_store(&cfg, configured_dimension(&cfg))?;
    let git = devctx_index::GitRepo::open(&project_root(&cfg)?)?;
    let branch = git.state().branch;
    let impact = store.impact_analysis(&git.short_name(), &branch, &symbol, depth)?;

    println!("Impact of `{symbol}` (depth {depth}):");
    print_impact("callers (upstream)", &impact.upstream);
    print_impact("callees (downstream)", &impact.downstream);
    Ok(())
}

/// Extract `[{symbol, depth}]` from a server JSON array into `(symbol, depth)`.
fn json_depth_pairs(v: &serde_json::Value) -> Vec<(String, usize)> {
    v.as_array()
        .map(|arr| {
            arr.iter()
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

fn print_impact(label: &str, items: &[(String, usize)]) {
    println!("  {label}:");
    if items.is_empty() {
        println!("    (none)");
        return;
    }
    let mut sorted = items.to_vec();
    sorted.sort_by_key(|(_, d)| *d);
    for (sym, d) in &sorted {
        println!("    {sym} (depth {d})");
    }
}

/// `devctx summarize` — summarize a file.
fn cmd_summarize(path: PathBuf, query: Option<String>, tokens: Option<usize>) -> Result<()> {
    let cfg = load_project()?;
    let content =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    if let Some(r) = remote::ensure(&cfg) {
        println!(
            "{}",
            r.summarize(&content, query.as_deref(), tokens.unwrap_or(200))?
        );
        return Ok(());
    }
    let target = tokens.unwrap_or(cfg.summarization.target_tokens);

    // The extractive summarizer needs an embedder; other providers don't.
    let extractive =
        cfg.summarization.provider.is_empty() || cfg.summarization.provider == "extractive";
    let embedder: Option<Arc<dyn EmbeddingProvider>> = if extractive {
        Some(Arc::from(build_embedder(&cfg)?))
    } else {
        None
    };

    let summarizer = create_summarizer(
        &SummarizeSettings {
            provider: cfg.summarization.provider.clone(),
            require_local: cfg.summarization.require_local,
            target_tokens: target,
            model: cfg.summarization.model.clone(),
            api_key: None,
        },
        embedder,
    )?;
    println!(
        "{}",
        summarizer.summarize(&content, query.as_deref(), target)?
    );
    Ok(())
}

/// `devctx routes` — list framework-aware HTTP routes.
fn cmd_routes(method: Option<String>, path: Option<String>) -> Result<()> {
    let cfg = load_project()?;
    if let Some(r) = remote::ensure(&cfg) {
        println!("{}", r.routes(method.as_deref(), path.as_deref())?);
        return Ok(());
    }
    let store = open_store(&cfg, configured_dimension(&cfg))?;
    let git = devctx_index::GitRepo::open(&project_root(&cfg)?)?;
    let routes = store.search_routes(
        &git.short_name(),
        &git.state().branch,
        method.as_deref(),
        path.as_deref(),
    )?;
    if routes.is_empty() {
        println!("No routes.");
        return Ok(());
    }
    for r in &routes {
        let handler = if r.handler_symbol.is_empty() {
            "-"
        } else {
            &r.handler_symbol
        };
        println!(
            "{:6} {}  [{}] {} ({}:{})",
            r.http_method, r.path, r.framework, handler, r.file, r.line
        );
    }
    Ok(())
}

/// A CLI progress bar for indexing (shows elapsed time, ETA and the current file).
struct IndexBar {
    bar: indicatif::ProgressBar,
}

impl IndexBar {
    fn new() -> Self {
        let bar = indicatif::ProgressBar::new(0);
        bar.set_style(
            indicatif::ProgressStyle::with_template(
                "  {spinner:.green} [{elapsed_precise}] [{bar:32.cyan/blue}] {pos}/{len} \
                 (eta {eta}) {msg}",
            )
            .unwrap()
            .progress_chars("=> "),
        );
        Self { bar }
    }

    fn finish(&self) {
        self.bar.finish_and_clear();
    }
}

impl ProgressSink for IndexBar {
    fn start(&self, total: usize) {
        self.bar.set_length(total as u64);
    }
    fn file(&self, path: &str) {
        self.bar.set_message(path.to_string());
        self.bar.inc(1);
    }
}

/// `devctx memory-stats` — show memory counts for the project.
fn cmd_memory_stats() -> Result<()> {
    let cfg = load_project()?;
    if let Some(r) = remote::ensure(&cfg) {
        println!("{}", r.memory_stats()?);
        return Ok(());
    }
    let store = open_store(&cfg, configured_dimension(&cfg))?;
    let stats = memory_stats(&store, &project_name(&cfg))?;
    println!("{} memories in {}", stats.total, cfg.project.name);
    for (ty, n) in &stats.by_type {
        println!("  {ty}: {n}");
    }
    Ok(())
}

/// A memory row from an old DevAI SQLite database.
struct OldMemory {
    title: String,
    content: String,
    memory_type: String,
    scope: String,
    project: String,
    topic_key: String,
    tags: String,
    author: String,
    repo: String,
    branch: String,
    files: String,
    created_at: String,
}

/// `devctx migrate` — import memories from an old DevAI SQLite DB, re-embedding
/// them with the currently-configured model (so it works for Granite too).
/// Code vectors/graph are regenerated by `devctx index`, not migrated.
fn cmd_migrate(from: Option<PathBuf>, dry_run: bool, keep_project: bool) -> Result<()> {
    let cfg = load_project()?;
    let src = from
        .or_else(default_old_db)
        .context("no old DevAI SQLite DB found; pass --from <path>")?;
    if !src.exists() {
        bail!("SQLite DB not found: {}", src.display());
    }
    println!("Reading memories from {}", src.display());

    let conn = rusqlite::Connection::open_with_flags(
        &src,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .with_context(|| format!("opening {}", src.display()))?;
    let mut stmt = conn.prepare(
        "SELECT title, content, memory_type, scope, project, COALESCE(topic_key,''), \
                tags, author, repo, branch, files, created_at \
         FROM memories WHERE deleted_at IS NULL OR deleted_at = '' ORDER BY id",
    )?;
    let mems: Vec<OldMemory> = stmt
        .query_map([], |r| {
            Ok(OldMemory {
                title: r.get(0)?,
                content: r.get(1)?,
                memory_type: r.get(2)?,
                scope: r.get(3)?,
                project: r.get(4)?,
                topic_key: r.get(5)?,
                tags: r.get(6)?,
                author: r.get(7)?,
                repo: r.get(8)?,
                branch: r.get(9)?,
                files: r.get(10)?,
                created_at: r.get(11)?,
            })
        })?
        .collect::<std::result::Result<_, _>>()?;
    println!("Found {} memories.", mems.len());

    if dry_run {
        let target = project_name(&cfg);
        for m in &mems {
            let proj = if keep_project && !m.project.is_empty() {
                &m.project
            } else {
                &target
            };
            println!("  · [{}] {}  → project `{proj}`", m.memory_type, m.title);
        }
        println!("(dry run — nothing written)");
        return Ok(());
    }
    if mems.is_empty() {
        return Ok(());
    }

    // Direct import so the project field is set exactly as chosen. If a server
    // owns the DB, the open fails — the error tells the user to stop it.
    let embedder = build_embedder(&cfg)?;
    let store = open_store(&cfg, embedder.dimension()).context(
        "opening the store (if a `devctx serve` is running, stop it first: `devctx serve --stop`)",
    )?;

    // `--keep-project` preserves each memory's original project; otherwise they
    // are imported under the current project so they're immediately findable.
    let target_project = project_name(&cfg);
    let (mut created, mut dup) = (0usize, 0usize);
    for m in &mems {
        let project = if keep_project && !m.project.is_empty() {
            m.project.clone()
        } else {
            target_project.clone()
        };
        let req = RememberRequest {
            title: m.title.clone(),
            content: m.content.clone(),
            memory_type: m.memory_type.clone(),
            project,
            topic_key: m.topic_key.clone(),
            tags: m.tags.clone(),
            scope: m.scope.clone(),
            author: m.author.clone(),
            repo: m.repo.clone(),
            branch: m.branch.clone(),
            files: m.files.clone(),
            session_id: String::new(),
            now: m.created_at.clone(),
        };
        match remember(&store, embedder.as_ref(), &req)?.status {
            devctx_memory::RememberStatus::Duplicate => dup += 1,
            _ => created += 1,
        }
    }
    println!(
        "Imported {created} memories ({dup} duplicates skipped), embedded with `{}`.",
        cfg.embeddings.model
    );
    if keep_project {
        println!("Kept original project names; query them with the matching project.");
    }
    println!("For code search, run `devctx index` to (re)build the index in DuckDB.");
    Ok(())
}

/// Locate an old DevAI SQLite DB: this project's `.devai`, then the global one.
fn default_old_db() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join(".devai/state/index.db"));
    }
    if let Ok(home) = std::env::var("HOME") {
        candidates.push(PathBuf::from(home).join(".local/share/devai/state/index.db"));
    }
    candidates.into_iter().find(|p| p.exists())
}

/// `devctx init` — write a `.devctx/config.yaml` for the target repo.
fn cmd_init(path: Option<PathBuf>, name: Option<String>) -> Result<()> {
    let root = match path {
        Some(p) => p,
        None => std::env::current_dir().context("resolving current directory")?,
    };
    let root = std::fs::canonicalize(&root)
        .with_context(|| format!("resolving path {}", root.display()))?;

    let cfg_path = root.join(devctx_core::CONFIG_FILE_NAME);
    if cfg_path.exists() {
        println!("Already initialized: {}", cfg_path.display());
        return Ok(());
    }

    let name = name.unwrap_or_else(|| {
        root.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "project".to_string())
    });

    let cfg = ProjectConfig {
        project: Project {
            name,
            path: root.to_string_lossy().into_owned(),
        },
        ..Default::default()
    };

    let yaml = serde_yaml::to_string(&cfg).context("serializing config")?;
    if let Some(parent) = cfg_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(&cfg_path, yaml).with_context(|| format!("writing {}", cfg_path.display()))?;

    println!("Initialized DevCtxEngine project at {}", cfg_path.display());
    register_centrally(&root);
    Ok(())
}

/// Add a freshly initialized repo to the central registry so it is discoverable
/// from other projects.
///
/// Best-effort on purpose: a central store that cannot be opened (unwritable
/// home, or a daemon holding the file) must not make `devctx init` fail — the
/// project config, which is what actually matters here, is already written.
fn register_centrally(root: &std::path::Path) {
    let result = Central::open().and_then(|c| {
        c.register(&RegisterRequest {
            root: root.to_path_buf(),
            now: devctx_central::now_stamp(),
            ..Default::default()
        })
    });
    match result {
        Ok(rec) => println!("Registered in the central store as `{}`", rec.name),
        Err(e) => eprintln!("· not registered centrally ({e}); run `devctx projects add` later"),
    }
}

/// `devctx status` — discover and summarize the active project config.
fn cmd_status() -> Result<()> {
    let cwd = std::env::current_dir().context("resolving current directory")?;
    let Some(cfg_path) = find_config_file(&cwd) else {
        println!("No DevCtxEngine project found (run `devctx init` first).");
        return Ok(());
    };
    let cfg = ProjectConfig::load(&cfg_path)?;
    if let Some(r) = remote::ensure(&cfg) {
        println!("DevCtxEngine {} (server mode)", devctx_core::VERSION);
        println!("  config:   {}", cfg_path.display());
        println!("{}", r.status()?);
        return Ok(());
    }

    println!("DevCtxEngine {}", devctx_core::VERSION);
    println!("  config:   {}", cfg_path.display());
    println!("  project:  {}", cfg.project.name);
    println!("  language: {:?}", cfg.language);
    println!(
        "  model:    {} ({})",
        cfg.embeddings.model, cfg.embeddings.provider
    );
    println!("  database: {}", cfg.db_path().display());
    Ok(())
}

/// `devctx index` — run the indexing pipeline.
fn cmd_index(full: bool) -> Result<()> {
    let cfg = load_project()?;
    if let Some(r) = remote::ensure(&cfg) {
        println!("{}", r.index(full)?);
        return Ok(());
    }
    let root = project_root(&cfg)?;
    eprintln!(
        "Loading embedder ({} / {})…",
        cfg.embeddings.model, cfg.embeddings.provider
    );
    let embedder = build_embedder(&cfg)?;
    let store = open_store(&cfg, embedder.dimension())?;

    // Golden rule for bulk loads: don't maintain the HNSW index row-by-row.
    // For a full reindex, drop it up front and rebuild once after the load.
    if full && cfg.storage.hnsw {
        store.drop_hnsw()?;
    }

    let progress = IndexBar::new();
    let res = index_run(IndexRequest {
        store: &store,
        embedder: embedder.as_ref(),
        repo_root: &root,
        incremental: !full,
        model_name: &cfg.embeddings.model,
        progress: Some(&progress),
    })?;
    progress.finish();

    println!(
        "Indexed {} ({}) @ {}",
        cfg.project.name,
        res.branch,
        short_commit(&res.commit)
    );
    println!(
        "  {} ({} files, {} skipped, {} deleted, {} pruned, {} renamed)",
        if res.full_reindex {
            "full reindex"
        } else {
            "incremental"
        },
        res.files_indexed,
        res.files_skipped,
        res.files_deleted,
        res.files_pruned,
        res.files_renamed,
    );
    println!("  {} symbols, {} chunks stored", res.symbols, res.chunks);

    if cfg.storage.hnsw {
        if store.enable_hnsw()? {
            println!("  HNSW index ready (VSS)");
        } else {
            eprintln!("  HNSW requested but the VSS extension is unavailable; using brute-force");
        }
    }
    if cfg.storage.fts {
        if store.rebuild_fts()? {
            println!("  FTS index ready (BM25)");
        } else {
            eprintln!("  FTS requested but the FTS extension is unavailable");
        }
    }
    Ok(())
}

/// `devctx search` — vector / keyword / hybrid search, then optional rerank.
fn cmd_search(
    query: String,
    limit: usize,
    language: Option<String>,
    format: OutputFormat,
    no_rerank: bool,
    keyword: bool,
    hybrid: bool,
) -> Result<()> {
    let cfg = load_project()?;
    if let Some(r) = remote::ensure(&cfg) {
        let mode = if hybrid {
            "hybrid"
        } else if keyword {
            "keyword"
        } else {
            "vector"
        };
        let json = r.search(&query, limit, language.as_deref(), mode)?;
        print_remote_search(&json, format)?;
        return Ok(());
    }
    let filter = SearchFilter {
        languages: language.into_iter().collect(),
        exclude_deletions: true,
        ..Default::default()
    };

    let mode = if hybrid {
        SearchMode::Hybrid
    } else if keyword {
        SearchMode::Keyword
    } else {
        SearchMode::Vector
    };

    // Keyword-only search needs no embedding model; vector/hybrid do.
    let embedder = if mode == SearchMode::Keyword {
        None
    } else {
        Some(build_embedder(&cfg)?)
    };
    let dim = embedder
        .as_ref()
        .map(|e| e.dimension())
        .unwrap_or_else(|| configured_dimension(&cfg));
    let store = open_store(&cfg, dim)?;

    // Rerank vector/hybrid results when enabled; keep keyword lightweight.
    let reranker = if !no_rerank && cfg.reranking.enabled && mode != SearchMode::Keyword {
        eprintln!("Loading reranker ({})…", cfg.reranking.model);
        Some(create_reranker(&RerankSettings {
            enabled: true,
            model: cfg.reranking.model.clone(),
        })?)
    } else {
        None
    };

    let hits = devctx_search::search(
        &store,
        &query,
        &filter,
        limit,
        mode,
        embedder.as_deref(),
        reranker.as_deref(),
    )?;

    let out = match format {
        OutputFormat::Table => render_table(&hits),
        OutputFormat::Json => render_json(&hits)?,
    };
    println!("{out}");
    Ok(())
}

// --- helpers ---

fn load_project() -> Result<ProjectConfig> {
    let cwd = std::env::current_dir().context("resolving current directory")?;
    let Some(cfg_path) = find_config_file(&cwd) else {
        bail!("No DevCtxEngine project found (run `devctx init` first).");
    };
    Ok(ProjectConfig::load(&cfg_path)?)
}

fn project_root(cfg: &ProjectConfig) -> Result<PathBuf> {
    if cfg.project.path.is_empty() {
        Ok(std::env::current_dir()?)
    } else {
        Ok(PathBuf::from(&cfg.project.path))
    }
}

fn build_embedder(cfg: &ProjectConfig) -> Result<Box<dyn EmbeddingProvider>> {
    let settings = EmbedSettings::from_config(&cfg.embeddings);
    Ok(create_provider(&settings)?)
}

fn open_store(cfg: &ProjectConfig, dim: usize) -> Result<Store> {
    let path = cfg.db_path();
    Ok(Store::open(&path, dim)?)
}

/// Project name for memory scoping (config name, else db-derived fallback).
fn project_name(cfg: &ProjectConfig) -> String {
    if !cfg.project.name.is_empty() {
        cfg.project.name.clone()
    } else {
        "default".to_string()
    }
}

/// Current epoch seconds as a string (memory timestamp).
fn now_epoch() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_default()
}

/// The store's vector dimension for the configured model, without loading it.
fn configured_dimension(cfg: &ProjectConfig) -> usize {
    devctx_embed::dimension_for(&cfg.embeddings.provider, &cfg.embeddings.model)
}

/// A one-line snippet of `text`, truncated to `max` chars.
fn snippet(text: &str, max: usize) -> String {
    let line = text.split('\n').next().unwrap_or("").trim();
    if line.chars().count() > max {
        let s: String = line.chars().take(max).collect();
        format!("{s}…")
    } else {
        line.to_string()
    }
}

fn short_commit(commit: &str) -> &str {
    if commit.len() >= 8 {
        &commit[..8]
    } else if commit.is_empty() {
        "(no commit)"
    } else {
        commit
    }
}

/// Compact JSON view of a search hit (excludes the raw vector).
#[derive(Serialize)]
struct SearchHitOut<'a> {
    score: f32,
    file: &'a str,
    start_line: i32,
    end_line: i32,
    symbol: &'a str,
    symbol_type: &'a str,
    level: &'a str,
    text: &'a str,
}

fn hit_out(h: &SearchResult) -> SearchHitOut<'_> {
    let m = &h.point.metadata;
    SearchHitOut {
        score: h.score,
        file: &m.file,
        start_line: m.start_line,
        end_line: m.end_line,
        symbol: &m.symbol,
        symbol_type: &m.symbol_type,
        level: &m.chunk_level,
        text: &h.point.text,
    }
}

fn render_json(hits: &[SearchResult]) -> Result<String> {
    let out: Vec<SearchHitOut> = hits.iter().map(hit_out).collect();
    Ok(serde_json::to_string_pretty(&out)?)
}

/// Render a server's `/search` JSON response in the requested output format,
/// matching the local table layout so routing is transparent.
fn print_remote_search(json: &str, format: OutputFormat) -> Result<()> {
    match format {
        OutputFormat::Json => println!("{json}"),
        OutputFormat::Table => {
            let hits: serde_json::Value = serde_json::from_str(json)?;
            let arr = hits.as_array().cloned().unwrap_or_default();
            if arr.is_empty() {
                println!("No results.");
                return Ok(());
            }
            for h in arr {
                let s = |k| h.get(k).and_then(|v| v.as_str()).unwrap_or("");
                let i = |k| h.get(k).and_then(|v| v.as_i64()).unwrap_or(0);
                let sym = if s("symbol").is_empty() {
                    "-"
                } else {
                    s("symbol")
                };
                println!(
                    "{:.3}  {}:{}-{}  {} [{}]",
                    h.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0),
                    s("file"),
                    i("start_line"),
                    i("end_line"),
                    sym,
                    s("level"),
                );
            }
        }
    }
    Ok(())
}

fn render_table(hits: &[SearchResult]) -> String {
    if hits.is_empty() {
        return "No results.".to_string();
    }
    let mut s = String::new();
    for h in hits {
        let m = &h.point.metadata;
        let symbol = if m.symbol.is_empty() { "-" } else { &m.symbol };
        s.push_str(&format!(
            "{:.3}  {}:{}-{}  {} [{}]\n",
            h.score, m.file, m.start_line, m.end_line, symbol, m.chunk_level
        ));
    }
    s.pop();
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use devctx_core::{VectorMetadata, VectorPoint};

    fn hit(file: &str, symbol: &str, score: f32) -> SearchResult {
        SearchResult {
            score,
            point: VectorPoint {
                id: "id".into(),
                vector: vec![],
                text: "fn foo() {}".into(),
                metadata: VectorMetadata {
                    file: file.into(),
                    symbol: symbol.into(),
                    symbol_type: "function".into(),
                    chunk_level: "function".into(),
                    start_line: 3,
                    end_line: 7,
                    ..Default::default()
                },
            },
        }
    }

    #[test]
    fn table_renders_rows() {
        let out = render_table(&[hit("src/a.rs", "foo", 0.9421)]);
        assert!(out.contains("0.942"));
        assert!(out.contains("src/a.rs:3-7"));
        assert!(out.contains("foo [function]"));
    }

    #[test]
    fn empty_table_message() {
        assert_eq!(render_table(&[]), "No results.");
    }

    #[test]
    fn json_excludes_vector_and_has_fields() {
        let out = render_json(&[hit("src/a.rs", "foo", 0.5)]).unwrap();
        assert!(out.contains("\"file\": \"src/a.rs\""));
        assert!(out.contains("\"symbol\": \"foo\""));
        assert!(out.contains("\"start_line\": 3"));
        assert!(!out.contains("vector"));
    }

    #[test]
    fn short_commit_truncates() {
        assert_eq!(short_commit("0123456789abcdef"), "01234567");
        assert_eq!(short_commit(""), "(no commit)");
    }
}
