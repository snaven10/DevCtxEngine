//! `devctx` — command-line entry point for the DevCtxEngine Rust rewrite.
//!
//! F5 wires the real pipeline: `init`, `status`, `index`, `search`. Building the
//! embedder pulls in fastembed/ort (the `local` provider) and downloads the
//! model on first use.

mod help_map;
mod hooks;
mod init_wizard;
mod mcp_configure;
mod models;
mod prompt_ui;
mod remote;
mod transfer;
mod transfer_apply;
mod update_check;
mod watch;
mod wizard_text;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use devctx_central::{project_json, Central, CentralPaths, RegisterRequest};
use devctx_core::config::{find_config_file, Project, ProjectConfig};
use devctx_core::{SearchFilter, SearchResult};
use devctx_embed::{create_provider, EmbedSettings, EmbeddingProvider};
use devctx_index::{run as index_run, IndexRequest, ProgressSink};
use devctx_memory::{memory_stats, recall, remember, RecallQuery, RememberRequest};
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
        /// Group this repository belongs to: a product spanning several repos,
        /// which then share a memory tier between them.
        #[arg(long)]
        group: Option<String>,
        /// Embedding model (see `devctx models`). On the first project it also
        /// becomes the machine's default and the vector space of shared
        /// memories; afterwards it applies to this project only.
        #[arg(long)]
        model: Option<String>,
        /// Directory for this project's index. Default: inside the repository.
        #[arg(long)]
        state_dir: Option<String>,
        /// Take the defaults without asking, and without confirming.
        #[arg(long)]
        yes: bool,
    },
    /// List the embedding models available, or download one that needs files.
    Models {
        /// Download a user-defined ONNX model (e.g. `ml-granite`) into the
        /// shared model cache. Without it, the models are listed.
        #[arg(long)]
        download: Option<String>,
    },
    /// Replace this binary with the latest published release.
    Update,
    /// Show repository and index status.
    Status,
    /// Index the current repository (git diff → parse → chunk → embed → store).
    Index {
        /// Force a full reindex instead of incremental.
        #[arg(long)]
        full: bool,
        /// The branch to index, which need not be the one checked out — a
        /// repository with worktrees has several live at once and only one on
        /// disk. Defaults to the first entry of `indexing.branches`, then to
        /// whatever is checked out. The branch must exist in git.
        #[arg(long)]
        branch: Option<String>,
    },
    /// Repair an index left inconsistent by an unclean shutdown.
    Repair,
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
        /// Where the memory belongs: this project, or every project.
        #[arg(long, value_enum, default_value_t = MemoryScope::Local)]
        scope: MemoryScope,
        /// Comma-separated files this memory is about. Fill it in and the
        /// memory becomes findable from the symbols in those files, via
        /// `memories_by_symbol`; leave it out and it is findable only by text.
        #[arg(long)]
        files: Option<String>,
    },
    /// Recall memories relevant to a query.
    Recall {
        /// The query.
        query: String,
        /// Maximum results.
        #[arg(long, default_value_t = 5)]
        limit: usize,
        /// Which memories to search.
        #[arg(long, value_enum, default_value_t = MemoryScope::All)]
        scope: MemoryScope,
        /// Only global memories contributed by this repository.
        #[arg(long)]
        repo: Option<String>,
    },
    /// Show memory counts for the project.
    MemoryStats,
    /// Permanently delete one memory by id, wherever it lives.
    MemoryForget {
        /// The memory id, as reported by `recall` or `remember`.
        id: String,
    },
    /// Export or import memories as JSONL.
    Memories {
        #[command(subcommand)]
        action: MemoriesAction,
    },
    /// Permanently delete every memory stored under one `project` key, and the
    /// vectors that belong to them.
    MemoryPurge {
        /// The key to purge, e.g. `@global` for rows an import left in the
        /// wrong store. Not a project *name* — the reserved key as stored.
        #[arg(long)]
        project: String,
        /// Report what would go without deleting anything.
        #[arg(long)]
        dry_run: bool,
    },
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
    /// Read a symbol's definition and code.
    Symbol {
        /// The symbol name. Bare (`charge`) or qualified (`Card.charge`).
        name: String,
        /// Maximum definitions to show.
        #[arg(long, default_value_t = 5)]
        limit: usize,
    },
    /// Assemble one budgeted brief for a question: what is already known, the
    /// code that ranks highest, and the memories recorded against those files.
    Context {
        /// What context is needed, in natural language.
        query: String,
        /// Token budget for the whole brief.
        #[arg(long, default_value_t = 4096)]
        max_tokens: usize,
        /// Leave memories out and return code only.
        #[arg(long)]
        no_memories: bool,
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
    Tui {
        /// Project to open, by registered name or path. Without it: the project
        /// in the current directory, else the only registered one.
        #[arg(long)]
        project: Option<String>,
    },
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
    /// Watch the work tree and re-index files as they are saved.
    Watch {
        /// Seconds to wait after the last change before indexing.
        #[arg(long, default_value_t = 3)]
        debounce: u64,
    },
    /// Install or remove the git hook that re-indexes after each commit.
    Hooks {
        #[command(subcommand)]
        action: HooksAction,
    },
    /// Re-index registered projects (incremental by default).
    Reindex {
        /// Every active project in the registry.
        #[arg(long)]
        all: bool,
        /// Named project(s); repeatable. Defaults to the current project.
        #[arg(long = "project")]
        projects: Vec<String>,
        /// Force a full reindex instead of incremental.
        #[arg(long)]
        full: bool,
    },
    /// Manage the central registry of projects DevCtxEngine knows about.
    Projects {
        #[command(subcommand)]
        action: ProjectsAction,
    },
}

/// Actions under `devctx hooks`.
#[derive(Debug, Subcommand)]
enum HooksAction {
    /// Install the `post-commit` hook (safe to re-run).
    Install,
    /// Remove the hook, leaving any of your own hook code in place.
    Uninstall,
    /// Report whether the hook is installed.
    Status,
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

/// Which memories a command applies to.
/// What `devctx memories` can do.
#[derive(Debug, Subcommand)]
enum MemoriesAction {
    /// Write memories to stdout, one JSON object per line.
    Export {
        /// Which memories: `local`, `group`, or `global`.
        #[arg(long, default_value = "local")]
        scope: String,
        /// Within a shared scope, only what this repository contributed.
        #[arg(long)]
        repo: Option<String>,
    },
    /// Link memories to the code they name, for memories saved before the
    /// junction existed — migrated, imported, or written by an older build.
    ///
    /// Run it once per repository: it links only the files this one has
    /// indexed, so a shared memory's links land wherever its files actually
    /// live. Safe to repeat.
    BackfillLinks {
        /// Report what would happen without writing anything.
        #[arg(long)]
        dry_run: bool,
        /// Also recover file paths from a memory's own text, for memories that
        /// name no files at all — about half of a corpus written before the
        /// field existed. Every recovered path is checked against the index
        /// before it is linked, so a library name that looks like a file is
        /// dropped rather than linked to nothing. Off by default: the
        /// derivation is weaker than a stated `files` field.
        #[arg(long)]
        from_text: bool,
    },
    /// Read memories from a JSONL file. Only ever adds; never overwrites.
    Import {
        /// The file to read.
        file: PathBuf,
        /// Put every memory in this scope, whatever the file says.
        #[arg(long)]
        scope: Option<String>,
        /// Report what would happen without writing.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum MemoryScope {
    /// This project only.
    Local,
    /// The repositories of this project's group (see `project.group`).
    Group,
    /// The shared central store, visible from every project.
    Global,
    /// Every tier that applies, fused by rank.
    All,
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
    // Parsed through the builder rather than `Cli::parse()` so the command map
    // can be chosen at run time: it is the one part of the help that is
    // translated, and the language lives in the project's config.
    let cli = {
        use clap::{CommandFactory as _, FromArgMatches as _};
        let cmd = Cli::command().after_help(help_map::text(help_map::language()));
        Cli::from_arg_matches(&cmd.get_matches())?
    };
    match cli.command {
        Command::Init {
            path,
            name,
            group,
            model,
            state_dir,
            yes,
        } => cmd_init(path, name, group, model, state_dir, yes),
        Command::Models { download } => cmd_models(download),
        Command::Update => models::self_update("snaven10/DevCtxEngine", env!("CARGO_PKG_VERSION")),
        Command::Status => cmd_status(),
        Command::Index { full, branch } => cmd_index(full, branch),
        Command::Repair => cmd_repair(),
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
            scope,
            files,
        } => cmd_remember(content, title, memory_type, topic, tags, scope, files),
        Command::Recall {
            query,
            limit,
            scope,
            repo,
        } => cmd_recall(query, limit, scope, repo),
        Command::MemoryStats => cmd_memory_stats(),
        Command::Memories { action } => match action {
            MemoriesAction::Export { scope, repo } => cmd_memories_export(&scope, repo.as_deref()),
            MemoriesAction::Import {
                file,
                scope,
                dry_run,
            } => cmd_memories_import(&file, scope.as_deref(), dry_run),
            MemoriesAction::BackfillLinks { dry_run, from_text } => {
                cmd_backfill_links(dry_run, from_text)
            }
        },
        Command::MemoryForget { id } => cmd_memory_forget(id),
        Command::MemoryPurge { project, dry_run } => cmd_memory_purge(project, dry_run),
        Command::Migrate {
            from,
            dry_run,
            keep_project,
        } => cmd_migrate(from, dry_run, keep_project),
        Command::Symbol { name, limit } => cmd_symbol(name, limit),
        Command::Context {
            query,
            max_tokens,
            no_memories,
        } => cmd_context(query, max_tokens, !no_memories),
        Command::Impact { symbol, depth } => cmd_impact(symbol, depth),
        Command::Routes { method, path } => cmd_routes(method, path),
        Command::Summarize {
            path,
            query,
            tokens,
        } => cmd_summarize(path, query, tokens),
        Command::Api { addr, token } => cmd_api(addr, token),
        Command::Tui { project } => cmd_tui(project),
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
        Command::Watch { debounce } => cmd_watch(debounce),
        Command::Hooks { action } => cmd_hooks(action),
        Command::Reindex {
            all,
            projects,
            full,
        } => cmd_reindex(all, projects, full),
        Command::Projects { action } => cmd_projects(action),
    }
}

/// `devctx watch` — re-index saved files until interrupted.
///
/// Complements the post-commit hook rather than replacing it: the hook covers
/// committed work, this covers what you have written but not committed yet.
fn cmd_watch(debounce: u64) -> Result<()> {
    let cfg = load_project()?;
    let root = project_root(&cfg)?;
    watch::run(&cfg, &root, std::time::Duration::from_secs(debounce.max(1)))
}

/// `devctx hooks` — manage the post-commit indexing hook.
fn cmd_hooks(action: HooksAction) -> Result<()> {
    let cfg = load_project()?;
    let root = project_root(&cfg)?;
    match action {
        HooksAction::Install => {
            let path = hooks::install(&root)?;
            println!("Installed the post-commit hook at {}", path.display());
            println!("This repository now re-indexes itself after each commit.");
        }
        HooksAction::Uninstall => {
            if hooks::uninstall(&root)? {
                println!("Removed the post-commit hook.");
            } else {
                println!("No DevCtxEngine hook was installed.");
            }
        }
        HooksAction::Status => {
            if hooks::installed(&root)? {
                println!("Installed: this repository re-indexes after each commit.");
            } else {
                println!("Not installed (run `devctx hooks install`).");
            }
        }
    }
    Ok(())
}

/// `devctx reindex` — re-index projects from the registry.
///
/// Each project is indexed through its own server, so this never takes a second
/// lock on a database another process already owns. One project failing does not
/// stop the rest: the failures are collected and reported at the end.
fn cmd_reindex(all: bool, names: Vec<String>, full: bool) -> Result<()> {
    let targets = reindex_targets(all, names)?;
    if targets.is_empty() {
        println!("Nothing to re-index.");
        return Ok(());
    }

    let mut failed = Vec::new();
    for (name, path) in &targets {
        println!("→ {name} ({})", path.display());
        match reindex_one(path, full) {
            Ok(summary) => println!("  {summary}"),
            Err(e) => {
                eprintln!("  failed: {e}");
                failed.push(name.clone());
            }
        }
    }

    println!(
        "\n{} of {} project(s) re-indexed.",
        targets.len() - failed.len(),
        targets.len()
    );
    if !failed.is_empty() {
        bail!("failed: {}", failed.join(", "));
    }
    Ok(())
}

/// Resolve which projects to re-index: the registry, named ones, or just this one.
fn reindex_targets(all: bool, names: Vec<String>) -> Result<Vec<(String, PathBuf)>> {
    if !all && names.is_empty() {
        let cfg = load_project()?;
        return Ok(vec![(project_name(&cfg), project_root(&cfg)?)]);
    }

    let registered = with_central(|c| c.list(false))?;
    let pick = |v: &serde_json::Value| (field(v, "name"), PathBuf::from(field(v, "path")));
    if all {
        return Ok(registered.iter().map(pick).collect());
    }

    let mut out = Vec::new();
    for name in names {
        let found = registered
            .iter()
            .find(|p| field(p, "name") == name)
            .ok_or_else(|| anyhow::anyhow!("no registered project named `{name}`"))?;
        out.push(pick(found));
    }
    Ok(out)
}

/// Index one project through its server, returning a one-line summary.
fn reindex_one(root: &std::path::Path, full: bool) -> Result<String> {
    let cfg_path = root.join(devctx_core::CONFIG_FILE_NAME);
    let cfg = ProjectConfig::load(&cfg_path)
        .with_context(|| format!("reading {}", cfg_path.display()))?;

    let Some(r) = remote::ensure(&cfg) else {
        bail!("could not reach or start a server for this project");
    };
    let raw = r.index(full)?;
    let v: serde_json::Value = serde_json::from_str(&raw).context("parsing the index result")?;
    Ok(format!(
        "{} @ {} — {} files, {} symbols, {} chunks ({} skipped)",
        if v["full_reindex"].as_bool().unwrap_or(false) {
            "full reindex"
        } else {
            "incremental"
        },
        short_commit(&field(&v, "commit")),
        num(&v, "files_indexed"),
        num(&v, "symbols"),
        num(&v, "chunks"),
        num(&v, "files_skipped"),
    ))
}

/// `devctx projects` — the central registry.
fn cmd_projects(action: ProjectsAction) -> Result<()> {
    let paths = CentralPaths::resolve().context("resolving the central store location")?;
    // Route through the daemon so concurrent commands don't fight over the
    // single-writer DuckDB file. Falling back to a direct open keeps a lone
    // command working when no daemon can be spawned.
    let remote = devctx_central::client::ensure(&paths);
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
            // Resolve against *our* working directory before the path leaves this
            // process: the daemon has its own cwd, so a relative path would
            // otherwise silently register whatever repository it happens to sit in.
            let root = std::fs::canonicalize(&root)
                .with_context(|| format!("resolving {}", root.display()))?;
            // Checked after registering, so the warning is the last thing seen.
            let is_repo = is_git_repo(&root);
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
            if !is_repo {
                warn_not_a_repo(&field(&rec, "path"));
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
fn cmd_tui(project: Option<String>) -> Result<()> {
    note_update();
    let cfg = tui_project(project)?;
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

/// Resolve `--model` into an embeddings config, refusing what cannot work.
///
/// A user-defined ONNX model needs its files on disk, and the failure without
/// them arrives later, at the first index. Checking here turns it into a
/// sentence about running `models download`.
fn choose_model(
    key: &str,
    base: &devctx_core::config::Embeddings,
) -> Result<devctx_core::config::Embeddings> {
    let spec = devctx_embed::registry::find_local(key).ok_or_else(|| {
        anyhow!("unknown model `{key}`; run `devctx models` to see what there is")
    })?;
    let mut out = base.clone();
    out.provider = "local".to_string();
    out.model = key.to_string();
    out.model_dir = if spec.builtin.is_some() {
        String::new()
    } else {
        let dir = models::local_dir(key).ok_or_else(|| {
            anyhow!(
                "`{key}` is a user-defined ONNX model and its files are not on this \
                 machine yet. Run `devctx models download {key}` first."
            )
        })?;
        dir.to_string_lossy().into_owned()
    };
    Ok(out)
}

/// Make this project's model the machine's default, and the vector space that
/// shared memories live in.
///
/// The first project on a machine is where the decision is actually made, and
/// leaving the central store on its own built-in default is how a machine ends
/// up with repositories indexed one way and their shared memories embedded
/// another — with no error, because the widths agree.
///
/// Only when the central store holds no memories yet: after that, `memory.model`
/// is not a preference but the space existing vectors live in, and changing it
/// would silently strand every one of them.
fn adopt_as_machine_default(embeddings: &devctx_core::config::Embeddings) {
    let Ok(paths) = CentralPaths::resolve() else {
        return;
    };
    let Ok(mut cfg) = devctx_central::CentralConfig::load_or_default(&paths.config) else {
        return;
    };
    let already = cfg.memory.model == embeddings.model;
    // "Are there memories?" decides whether the shared vector space may be
    // repointed, so an unknown answer must count as yes. Reading it can easily
    // fail for a reason that says nothing about the store — the daemon holds
    // the file, most of the time — and treating that as "no memories" repoints
    // the space every existing vector lives in, which is silent and total: the
    // store keeps its 384-wide rows and the next open refuses them.
    let has_memories = match Central::open() {
        Ok(c) => c.memory_stats().map(|s| s.total > 0).unwrap_or(true),
        // Most often: a daemon owns the file. That says nothing about whether
        // the store is empty, so it cannot license repointing it.
        Err(_) => true,
    };

    cfg.defaults.embeddings = embeddings.clone();
    if !already && has_memories {
        eprintln!(
            "note: shared memories are already embedded with `{}`, so that stays the \
             central memory model; this project uses `{}` for its own index. New \
             projects will inherit `{}`.",
            cfg.memory.model, embeddings.model, embeddings.model
        );
    } else {
        cfg.memory.provider = embeddings.provider.clone();
        cfg.memory.model = embeddings.model.clone();
        cfg.memory.model_dir = embeddings.model_dir.clone();
        if !already {
            println!(
                "  set the machine default and shared-memory model to `{}`",
                embeddings.model
            );
        }
    }
    if let Err(e) = cfg.save(&paths.config) {
        eprintln!("warning: could not write {}: {e}", paths.config.display());
    }
}

/// The repository releases are published from.
const RELEASE_REPO: &str = "snaven10/DevCtxEngine";

/// Print a one-line notice when a newer release exists. Silent otherwise, and
/// silent on failure — see `update_check`.
fn note_update() {
    if let Some(v) = update_check::available(RELEASE_REPO, env!("CARGO_PKG_VERSION")) {
        eprintln!(
            "A newer devctx is available ({v}; this is {}). Update with `devctx update`.",
            env!("CARGO_PKG_VERSION")
        );
    }
}

/// `devctx models` — list what can be embedded with, or fetch one.
fn cmd_models(download: Option<String>) -> Result<()> {
    match download {
        Some(key) => {
            models::download(&key)?;
            Ok(())
        }
        None => {
            let configured = central_defaults().embeddings.model;
            models::list((!configured.is_empty()).then_some(configured.as_str()))
        }
    }
}

/// How many registered projects use each embedding model, commonest first.
///
/// Read from the registry rather than by opening each project: the registry
/// already caches every project's model, and a wizard that opened four
/// databases to print one line would be slow for no reason. It can be stale if
/// someone edited a config by hand — `projects refresh` is the cure.
fn models_in_use() -> Vec<(String, usize)> {
    let mut counts: std::collections::BTreeMap<String, usize> = Default::default();
    for (_, model, _) in registry_snapshot() {
        if !model.is_empty() {
            *counts.entry(model).or_default() += 1;
        }
    }
    let mut out: Vec<(String, usize)> = counts.into_iter().collect();
    // Commonest first: the answer someone most likely wants is the one most of
    // their repositories already gave.
    out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    out
}

/// The names of every registered project, for the "copy from" offer.
fn registered_project_names() -> Vec<String> {
    registry_snapshot()
        .into_iter()
        .map(|(name, _, _)| name)
        .collect()
}

/// The registry as `(name, embedding model, config path)`, or empty when it
/// cannot be read.
///
/// Routed through the daemon when one is up. Opening the file directly fails
/// whenever a daemon holds it — which is most of the time — and every caller
/// here treats failure as "nothing registered", so the questions that depend on
/// the registry silently vanished exactly when the machine had projects to
/// report.
fn registry_snapshot() -> Vec<(String, String, String)> {
    let field = |v: &serde_json::Value, k: &str| {
        v.get(k)
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string()
    };
    if let Ok(paths) = CentralPaths::resolve() {
        if let Some(client) = devctx_central::client::ensure(&paths) {
            if let Ok(list) = client.list(false) {
                return list
                    .iter()
                    .map(|p| {
                        (
                            field(p, "name"),
                            field(p, "embed_model"),
                            field(p, "config_path"),
                        )
                    })
                    .collect();
            }
        }
    }
    match Central::open().and_then(|c| c.list(false)) {
        Ok(ps) => ps
            .into_iter()
            .map(|p| (p.name, p.embed_model, p.config_path))
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// The groups already in use, with how many repositories each holds.
///
/// Read from each project's config, not the registry: the registry caches the
/// model but not the group.
fn groups_in_use() -> Vec<(String, usize)> {
    let mut counts: std::collections::BTreeMap<String, usize> = Default::default();
    for (_, _, config_path) in registry_snapshot() {
        let Ok(cfg) = ProjectConfig::load(std::path::Path::new(&config_path)) else {
            continue;
        };
        if !cfg.project.group.is_empty() {
            *counts.entry(cfg.project.group).or_default() += 1;
        }
    }
    let mut out: Vec<(String, usize)> = counts.into_iter().collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    out
}

/// The machine's default embeddings and reranking for a new project.
///
/// Read straight from the central config file rather than through the daemon:
/// `init` runs before any project exists, and needing a daemon up to create one
/// would be a circular requirement. A machine with no central config yet falls
/// back to the built-in defaults, which is the correct answer for a first run.
fn central_defaults() -> devctx_central::Defaults {
    CentralPaths::resolve()
        .ok()
        .and_then(|p| devctx_central::CentralConfig::load_or_default(&p.config).ok())
        .map(|c| c.defaults)
        .unwrap_or_default()
}

/// Which project the TUI opens on.
///
/// Refusing to start outside a repository is the wrong answer for a UI whose
/// whole job is browsing what is registered: the projects view, the memories
/// view and the global scope selector are all reachable once it is up, and any
/// project will do as an entry point. So a bare `devctx tui` from a home
/// directory opens the registry's project rather than erroring, and says which
/// one it picked.
fn tui_project(project: Option<String>) -> Result<ProjectConfig> {
    if let Some(name) = project {
        let root = devctx_mcp::state::resolve_project_root(&name).map_err(|e| anyhow!(e))?;
        return ProjectConfig::load(&root.join(devctx_core::CONFIG_FILE_NAME))
            .with_context(|| format!("loading project at {}", root.display()));
    }
    if let Ok(cfg) = load_project() {
        return Ok(cfg);
    }

    let paths = CentralPaths::resolve().context("resolving the central store location")?;
    let registered: Vec<String> = match devctx_central::client::ensure(&paths) {
        Some(c) => c
            .list(false)?
            .iter()
            .filter_map(|p| p.get("name").and_then(|n| n.as_str()).map(String::from))
            .collect(),
        None => Central::open()?
            .list(false)?
            .into_iter()
            .map(|p| p.name)
            .collect(),
    };
    let Some(first) = registered.first() else {
        bail!("no DevCtxEngine project here and none registered (run `devctx init` first)");
    };
    if registered.len() > 1 {
        eprintln!(
            "No project here; opening `{first}`. Others: {}. Switch with F4 or --project.",
            registered[1..].join(", ")
        );
    }
    let root = devctx_mcp::state::resolve_project_root(first).map_err(|e| anyhow!(e))?;
    ProjectConfig::load(&root.join(devctx_core::CONFIG_FILE_NAME))
        .with_context(|| format!("loading project at {}", root.display()))
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
        return Ok(devctx_central::client::stop(&paths)?);
    }
    if devctx_central::client::discover(&paths).is_some() {
        bail!(
            "a central daemon is already running for {} (stop it with \
             `devctx serve --central --stop`)",
            paths.dir.display()
        );
    }

    // `--addr` defaults to the project-server port, which would be wrong here;
    // when it is left at that default, use the per-home address instead.
    let addr = if addr == DEFAULT_ADDR {
        devctx_central::client::default_addr(&paths)
    } else {
        addr
    };
    let socket: SocketAddr = addr
        .parse()
        .with_context(|| format!("invalid --addr `{addr}`"))?;
    let token = token.or_else(|| std::env::var("DEVCTX_API_TOKEN").ok());

    let central = Central::open().context("opening the central store")?;
    devctx_central::client::write_serve_file(&paths, socket, token.as_deref())?;
    println!("DevCtxEngine central store → http://{addr}");
    println!("  Database: {}", paths.db.display());
    println!("  Config:   {}", paths.config.display());
    println!("Registry commands will route through it while it runs. Ctrl-C to stop.");

    let idle = (idle > 0).then(|| std::time::Duration::from_secs(idle));
    let result = devctx_api::central::run_blocking(central, socket, token, idle);
    devctx_central::client::remove_serve_file(&paths);
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
    // Checked once here, at startup, and handed to the tools through the
    // environment: a per-call check would put a network round-trip on the path
    // of every `list_projects`, and the answer changes at most daily.
    if let Some(v) = update_check::available(RELEASE_REPO, env!("CARGO_PKG_VERSION")) {
        std::env::set_var("DEVCTX_UPDATE_AVAILABLE", v);
    }
    // Registering this server globally is the normal thing to do, and it means
    // the client launches it from whatever directory it happens to be in —
    // usually the user's home, which is inside no repository at all. Refusing to
    // start there would reach the user as a bare transport error, so instead the
    // server comes up unbound and says which projects exist.
    let cfg = match &project {
        Some(root) => Some(
            ProjectConfig::load(&root.join(devctx_core::CONFIG_FILE_NAME))
                .with_context(|| format!("loading project at {}", root.display()))?,
        ),
        None => load_project().ok(),
    };
    let backend = match cfg {
        Some(cfg) => Some(mcp_backend(cfg)?),
        None => {
            eprintln!(
                "Starting DevCtxEngine MCP server (stdio, no project here); \
                 use the use_project tool to bind one."
            );
            None
        }
    };
    devctx_mcp::run_stdio(
        backend,
        std::sync::Arc::new(|root: &std::path::Path| {
            let cfg = ProjectConfig::load(&root.join(devctx_core::CONFIG_FILE_NAME))
                .map_err(|e| format!("loading project at {}: {e}", root.display()))?;
            mcp_backend(cfg).map_err(|e| e.to_string())
        }),
    )
}

/// A tool backend for one project.
///
/// Routes through a shared server (auto-spawned if needed) so many MCP sessions
/// plus the web/CLI/TUI of the same project coexist without lock fights; owning
/// the database here is the fallback when no server can be reached.
fn mcp_backend(cfg: ProjectConfig) -> Result<devctx_mcp::Backend> {
    let server = match remote::ensure(&cfg) {
        Some(r) => {
            let (base, token) = r.into_parts();
            eprintln!("DevCtxEngine MCP → routing to {base}");
            Some(devctx_mcp::ServerConn { base, token })
        }
        None => {
            remote::reclaim_db(&cfg);
            eprintln!("DevCtxEngine MCP → local database");
            None
        }
    };
    devctx_mcp::backend_for(cfg, server)
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

/// The wire name of a scope, as `/remember` expects it.
fn scope_name(scope: MemoryScope) -> &'static str {
    match scope {
        MemoryScope::Local => "local",
        MemoryScope::Group => "group",
        MemoryScope::Global => "global",
        // `all` is a recall filter, not a place to write to; the server treats
        // anything it does not recognize as local, and so do we, explicitly.
        MemoryScope::All => "local",
    }
}

/// Link a memory that was stored centrally to this repository's call graph.
///
/// Best-effort throughout: the memory is already saved, and a project whose
/// store cannot be opened — because a server holds it, because the repository
/// was never indexed — loses only the shortcut from symbol to decision.
fn link_shared_memory(
    cfg: &ProjectConfig,
    id: String,
    content: &str,
    files: &str,
    repo: &str,
    branch: &str,
) {
    if id.is_empty() || files.is_empty() {
        return;
    }
    let Ok(embedder) = build_embedder(cfg) else {
        return;
    };
    let Ok(store) = open_store(cfg, embedder.dimension()) else {
        return;
    };
    devctx_memory::links::link_memory(
        &store,
        &devctx_store::Memory {
            id,
            content: content.to_string(),
            files: files.to_string(),
            repo: repo.to_string(),
            branch: branch.to_string(),
            ..Default::default()
        },
    );
}

/// `devctx remember` — save a memory.
fn cmd_remember(
    content: String,
    title: Option<String>,
    memory_type: String,
    topic: Option<String>,
    tags: Option<String>,
    scope: MemoryScope,
    files: Option<String>,
) -> Result<()> {
    let cfg = load_project()?;
    let files = files.unwrap_or_default();

    // A shared memory — group or global — belongs to the central store, which
    // only the daemon may write, so this path never touches a project database.
    if scope == MemoryScope::Group && cfg.project.group.is_empty() {
        bail!(
            "this project declares no group; set `project.group` in {} or use \
             `--scope global`",
            cfg.project.name
        );
    }
    let group = if scope == MemoryScope::Group {
        cfg.project.group.clone()
    } else {
        String::new()
    };
    // A running server owns this project's database, and it is also the process
    // that can link the memory to the code — so hand it the whole job, scope and
    // all, rather than doing the central half here and then failing to open a
    // store something else holds. Found by smoke test: the link was skipped in
    // silence whenever a server happened to be up, which is the normal case.
    if let Some(r) = remote::ensure(&cfg) {
        println!(
            "{}",
            r.remember(
                &content,
                title.as_deref().unwrap_or(""),
                &memory_type,
                topic.as_deref().unwrap_or(""),
                tags.as_deref().unwrap_or(""),
                &files,
                scope_name(scope),
            )?
        );
        return Ok(());
    }

    if scope != MemoryScope::Local {
        // Provenance must never be blank: a directory that is not a git repo
        // still belongs to a named project, and "which project taught me this"
        // is the whole point of recording it.
        let project = project_name(&cfg);
        let (repo, branch) = repo_branch(&cfg);
        let repo = if repo.is_empty() {
            project.clone()
        } else {
            repo
        };
        let out = with_central(|c| {
            c.remember(
                &content,
                title.as_deref().unwrap_or(""),
                &memory_type,
                topic.as_deref().unwrap_or(""),
                tags.as_deref().unwrap_or(""),
                &project,
                &repo,
                &branch,
                &group,
                &files,
            )
        })?;
        // The memory lives centrally; its link to this repository's code does
        // not — see `devctx_memory::links`.
        link_shared_memory(&cfg, field(&out, "id"), &content, &files, &repo, &branch);
        let tier = if group.is_empty() {
            "global".to_string()
        } else {
            format!("group:{group}")
        };
        println!(
            "{}: {} — {} [{tier}]",
            field(&out, "status"),
            field(&out, "id"),
            title_or_untitled(&field(&out, "title")),
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
        files,
        repo: repo_branch(&cfg).0,
        branch: repo_branch(&cfg).1,
        now: now_epoch(),
        ..Default::default()
    };
    let res = remember(&store, embedder.as_ref(), &req)?;
    devctx_memory::links::link_memory(&store, &res.memory);
    let title = if res.memory.title.is_empty() {
        "(untitled)"
    } else {
        &res.memory.title
    };
    println!("{:?}: {} — {}", res.status, res.memory.id, title);
    Ok(())
}

/// `devctx recall` — recall memories relevant to a query.
///
/// With both scopes the two result lists are fused **by rank**: the project and
/// the central store may embed with different models, so their similarity scores
/// are not on comparable scales.
fn cmd_recall(query: String, limit: usize, scope: MemoryScope, repo: Option<String>) -> Result<()> {
    let cfg = load_project()?;

    let wants = |t: MemoryScope| scope == t || scope == MemoryScope::All;

    let global: Vec<serde_json::Value> = if wants(MemoryScope::Global) {
        with_central(|c| c.recall(&query, limit, repo.as_deref()))?
    } else {
        Vec::new()
    };

    // The group tier only exists for a repository that declares one; for a
    // standalone repo `--scope all` stays the two-tier search it was.
    let group: Vec<serde_json::Value> =
        if wants(MemoryScope::Group) && !cfg.project.group.is_empty() {
            let g = cfg.project.group.clone();
            with_central(|c| c.recall_scoped(&query, limit, repo.as_deref(), Some(&g)))?
        } else {
            Vec::new()
        };

    let local = if wants(MemoryScope::Local) {
        local_recall(&cfg, &query, limit)?
    } else {
        Vec::new()
    };

    let hits = fuse_memory_lists(vec![local, group, global], limit);
    if hits.is_empty() {
        println!("No memories.");
        return Ok(());
    }
    for (h, origin) in &hits {
        println!(
            "[{origin}] {} — {}",
            field(h, "type"),
            title_or_untitled(&field(h, "title"))
        );
        println!("        {}", snippet(&field(h, "content"), 100));
    }
    Ok(())
}

/// Recall a project's own memories, routing through its server when one is
/// running so no second process takes the DuckDB lock.
fn local_recall(cfg: &ProjectConfig, query: &str, limit: usize) -> Result<Vec<serde_json::Value>> {
    if let Some(r) = remote::ensure(cfg) {
        let raw = r.recall(query, limit)?;
        let parsed: serde_json::Value = serde_json::from_str(&raw).context("parsing recall")?;
        return Ok(parsed.as_array().cloned().unwrap_or_default());
    }
    let embedder = build_embedder(cfg)?;
    let store = open_store(cfg, embedder.dimension())?;
    Ok(recall(
        &store,
        embedder.as_ref(),
        &RecallQuery {
            query,
            project: Some(&project_name(cfg)),
            repo: None,
            limit,
        },
    )?
    .iter()
    .map(|h| {
        serde_json::json!({
            "id": h.memory.id, "title": h.memory.title, "content": h.memory.content,
            "type": h.memory.memory_type, "tags": h.memory.tags, "repo": h.memory.repo,
            "score": h.score,
        })
    })
    .collect())
}

/// Open the central store — through the daemon when one is reachable, directly
/// otherwise — and run `f` against it.
fn with_central<T>(
    f: impl FnOnce(&devctx_central::CentralClient) -> devctx_central::Result<T>,
) -> Result<T> {
    let paths = CentralPaths::resolve().context("resolving the central store location")?;
    match devctx_central::client::ensure(&paths) {
        Some(r) => Ok(f(&r)?),
        None => bail!(
            "no central daemon and one could not be started; run \
             `devctx serve --central` (store: {})",
            paths.db.display()
        ),
    }
}

fn title_or_untitled(title: &str) -> String {
    if title.is_empty() {
        "(untitled)".to_string()
    } else {
        title.to_string()
    }
}

/// Fuse local, group and global recall results by rank, tagging each with its
/// origin. Callers pass the lists in that order.
fn fuse_memory_lists(
    lists: Vec<Vec<serde_json::Value>>,
    limit: usize,
) -> Vec<(serde_json::Value, &'static str)> {
    let origins = ["local", "group", "global"];
    let tagged: Vec<Vec<(serde_json::Value, &'static str)>> = lists
        .into_iter()
        .enumerate()
        .map(|(i, list)| {
            let origin = origins.get(i).copied().unwrap_or("local");
            list.into_iter().map(|v| (v, origin)).collect()
        })
        .collect();
    devctx_core::fuse_by_rank(tagged, |(v, _)| field(v, "id"), limit)
}

/// The short repo name and branch for the active project, empty when not a repo.
fn repo_branch(cfg: &ProjectConfig) -> (String, String) {
    let root = if cfg.project.path.is_empty() {
        std::path::PathBuf::from(".")
    } else {
        std::path::PathBuf::from(&cfg.project.path)
    };
    match devctx_index::GitRepo::open(&root) {
        Ok(git) => (git.short_name(), git.state().branch),
        Err(_) => (String::new(), String::new()),
    }
}

/// `devctx impact` — show the blast radius of a symbol.
/// `devctx memories backfill-links` — link memories saved before the junction.
///
/// Routed through the project server, which owns both the store the links are
/// written into and the client for the shared memories that need linking.
fn cmd_backfill_links(dry_run: bool, from_text: bool) -> Result<()> {
    let cfg = load_project()?;
    let Some(r) = remote::ensure(&cfg) else {
        bail!(
            "backfilling needs this project's server; start one with `devctx serve` \
             (or run any indexing command, which spawns it)"
        );
    };
    let raw = r.backfill_links(dry_run, from_text)?;
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap_or_default();
    let n = |k: &str| v.get(k).and_then(|x| x.as_u64()).unwrap_or(0);

    if dry_run {
        println!("Dry run — nothing was written.");
    }
    println!(
        "Examined {} memories ({} of this project, {} shared).",
        n("examined"),
        v.pointer("/sources/local")
            .and_then(|x| x.as_u64())
            .unwrap_or(0),
        v.pointer("/sources/shared")
            .and_then(|x| x.as_u64())
            .unwrap_or(0),
    );
    // A dry run stops before linking, so it has no symbol counts to give.
    // Printing them as zero would read as "matched, but nothing linkable" —
    // the opposite of what a dry run is saying.
    if dry_run {
        println!(
            "  {} name a file this repository indexes and would be linked.",
            n("matched"),
        );
    } else {
        println!(
            "  {} name a file this repository indexes; {} of those gained symbol \
             links ({} rows).",
            n("matched"),
            n("linked_with_symbols"),
            n("rows_written"),
        );
        // Reported separately because it is the weaker derivation: a path read
        // out of a sentence, not one anybody stated. Folding it into the total
        // would hide how much of the sweep rests on it.
        if n("linked_from_text") > 0 {
            println!(
                "  Of those, {} were matched from the memory's own text rather \
                 than a `files` field.",
                n("linked_from_text"),
            );
        }
    }
    // The two skip counts mean different things, and collapsing them would hide
    // the one that is actionable: files belonging to another repository are
    // expected, while memories with no files at all are the gap a second pass
    // over their text would close.
    println!(
        "  Skipped: {} name no file this repository has{}, {} name only files \
         this repository does not index.",
        n("skipped_no_files"),
        if from_text { "" } else { " (try --from-text)" },
        n("skipped_not_in_this_repo"),
    );
    Ok(())
}

/// `devctx symbol` — a symbol's definition and code.
fn cmd_symbol(name: String, limit: usize) -> Result<()> {
    let cfg = load_project()?;
    let raw = match remote::ensure(&cfg) {
        Some(r) => r.read_symbol(&name, limit)?,
        None => {
            let store = open_store(&cfg, configured_dimension(&cfg))?;
            let git = devctx_index::GitRepo::open(&project_root(&cfg)?)?;
            let branch = git.state().branch;
            let found = store.symbol_definitions(&git.short_name(), &branch, &name, limit)?;
            if found.is_empty() {
                println!("No definition of `{name}` is indexed.");
                return Ok(());
            }
            for p in &found {
                println!(
                    "\n{} ({}) — {}:{}-{}",
                    p.metadata.symbol,
                    p.metadata.symbol_type,
                    p.metadata.file,
                    p.metadata.start_line,
                    p.metadata.end_line
                );
                println!("{}", p.text);
            }
            return Ok(());
        }
    };
    println!("{raw}");
    Ok(())
}

/// `devctx context` — one budgeted brief for a question.
///
/// Always routed through the server when one is up: the brief needs the
/// embedder, the central store and this project's graph at once, and the server
/// is the process that already holds all three.
fn cmd_context(query: String, max_tokens: usize, include_memories: bool) -> Result<()> {
    let cfg = load_project()?;
    let Some(r) = remote::ensure(&cfg) else {
        bail!(
            "`context` needs this project's server; start one with `devctx serve` \
             (or run any indexing command, which spawns it)"
        );
    };
    println!("{}", r.build_context(&query, max_tokens, include_memories)?);
    Ok(())
}

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

impl IndexBar {
    /// Place the bar at an absolute position, for a run being *observed*
    /// rather than driven.
    ///
    /// [`ProgressSink::file`] advances by one because the local path calls it
    /// once per file. A poller instead receives the server's running total, and
    /// incrementing on each poll would count the same file once per tick.
    fn set(&self, done: usize, file: &str) {
        self.bar.set_position(done as u64);
        self.bar.set_message(file.to_string());
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

/// Resolve a scope name to the project key its memories are stored under, and
/// whether that key lives in the central store rather than this project's.
fn scope_key(cfg: &ProjectConfig, scope: &str) -> Result<(String, bool)> {
    match scope {
        "local" => Ok((project_name(cfg), false)),
        "global" => Ok((devctx_memory::GLOBAL_PROJECT.to_string(), true)),
        "group" => {
            if cfg.project.group.is_empty() {
                bail!(
                    "this project declares no group; set `project.group` in its config, \
                     or use --scope local or --scope global"
                );
            }
            Ok((devctx_memory::group_project(&cfg.project.group), true))
        }
        other => bail!("unknown scope `{other}`; expected local, group or global"),
    }
}

/// Open whichever store holds `key`.
///
/// The daemon must be stopped either way: DuckDB allows one writing process,
/// and these commands are rare enough that saying so is better than routing
/// bulk reads and writes through HTTP.
fn store_for_key(cfg: &ProjectConfig, central: bool) -> Result<Store> {
    if central {
        Ok(Central::open()
            .context(
                "opening the central store (stop a running daemon first: \
                 `devctx serve --central --stop`)",
            )?
            .store()
            .try_clone()?)
    } else {
        open_store(cfg, configured_dimension(cfg))
            .context("opening the store (stop a running server first: `devctx serve --stop`)")
    }
}

/// `devctx memories export` — write a scope's memories to stdout as JSONL.
fn cmd_memories_export(scope: &str, repo: Option<&str>) -> Result<()> {
    let cfg = load_project()?;
    let (key, central) = scope_key(&cfg, scope)?;
    let store = store_for_key(&cfg, central)?;

    let model = cfg.embeddings.model.clone();
    let dim = configured_dimension(&cfg);
    let mut written = 0usize;
    for m in store.all_memories(&key)? {
        if let Some(r) = repo {
            if m.repo != r {
                continue;
            }
        }
        let embedding = store
            .vector_by_id(&m.id)?
            .map(|vector| transfer::Embedding {
                model: model.clone(),
                dim,
                vector,
            });
        println!("{}", transfer::to_line(&m, embedding)?);
        written += 1;
    }
    // stderr, so it does not land in the file being redirected.
    eprintln!("Exported {written} memories from `{key}`.");
    Ok(())
}

/// `devctx memories import` — add memories from a JSONL file.
fn cmd_memories_import(file: &std::path::Path, scope: Option<&str>, dry_run: bool) -> Result<()> {
    let cfg = load_project()?;
    let raw =
        std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;

    let mut lines: Vec<transfer::TransferLine> = Vec::new();
    for (n, l) in raw.lines().enumerate() {
        if l.trim().is_empty() {
            continue;
        }
        lines
            .push(transfer::from_line(l).with_context(|| format!("{}:{}", file.display(), n + 1))?);
    }

    // Grouped by destination first: a file may carry memories of several
    // scopes, and each destination is a different store.
    let mut by_key: std::collections::BTreeMap<String, Vec<transfer::TransferLine>> =
        Default::default();
    for line in lines {
        let key = match scope {
            Some(s) => scope_key(&cfg, s)?.0,
            None => line.memory.project.clone(),
        };
        by_key.entry(key).or_default().push(line);
    }

    let model = cfg.embeddings.model.clone();
    let dim = configured_dimension(&cfg);
    for (key, incoming) in by_key {
        let central = key.starts_with('@');
        let store = store_for_key(&cfg, central)?;
        let mut existing = store.all_memories(&key)?;
        let mut report = transfer_apply::ImportReport::default();
        let (mut reused, mut recomputed) = (0usize, 0usize);
        let mut embedder = None;

        for line in incoming {
            let outcome = transfer_apply::decide(&line.memory, &existing);
            report.record(&line.memory, outcome);
            if outcome == transfer_apply::Outcome::AlreadyPresent || dry_run {
                continue;
            }
            let mut m = transfer_apply::prepare(&line.memory, outcome);
            m.project = key.clone();
            m.vector_id = m.id.clone();

            // Reuse the embedding only on an exact match. The same model name
            // across two implementations produced vectors 0.76-0.87 apart from
            // each other's — close enough to look right, wrong enough to rank
            // everything incorrectly.
            let vector = match &line.embedding {
                Some(e) if e.model == model && e.dim == dim => {
                    reused += 1;
                    e.vector.clone()
                }
                _ => {
                    recomputed += 1;
                    if embedder.is_none() {
                        embedder = Some(build_embedder(&cfg)?);
                    }
                    let e = embedder.as_ref().expect("just built");
                    e.embed(&[m.content.clone()])?.remove(0)
                }
            };

            store.upsert_memory(&m)?;
            store.upsert(&[devctx_core::VectorPoint {
                id: m.id.clone(),
                vector,
                text: m.content.clone(),
                metadata: devctx_core::VectorMetadata {
                    repo: m.repo.clone(),
                    branch: m.branch.clone(),
                    symbol: m.title.clone(),
                    symbol_type: m.memory_type.clone(),
                    language: "memory".to_string(),
                    chunk_level: "memory".to_string(),
                    memory_type: m.memory_type.clone(),
                    memory_scope: m.scope.clone(),
                    memory_tags: m.tags.clone(),
                    ..Default::default()
                },
            }])?;
            existing.push(m);
        }

        let verb = if dry_run { "would import" } else { "imported" };
        println!(
            "{verb} {} memories into `{key}` · {} already present",
            report.added, report.already
        );
        if reused > 0 || recomputed > 0 {
            println!("  embeddings: {reused} reused ({model}/{dim}), {recomputed} recomputed");
        }
        if !report.collisions.is_empty() {
            println!(
                "  {} topic collisions kept separately:",
                report.collisions.len()
            );
            for t in &report.collisions {
                println!("    · {t}");
            }
        }
    }
    Ok(())
}

/// `devctx memory-forget` — remove a single memory, wherever it lives.
///
/// Looks in this project's store and then the central one, because the caller
/// has an id from a `recall` that already blended the tiers and has no reason
/// to know which of them answered.
fn cmd_memory_forget(id: String) -> Result<()> {
    let cfg = load_project()?;
    // A locked project store is not a reason to give up: the memory is as
    // likely to be in the central one, and refusing because an unrelated
    // server holds this project's file would make deleting a shared memory
    // depend on stopping something that has nothing to do with it.
    match open_store(&cfg, configured_dimension(&cfg)) {
        Ok(store) => {
            if store.forget_memory(&id)? {
                println!("Forgot {id} (project `{}`).", cfg.project.name);
                return Ok(());
            }
        }
        Err(e) => eprintln!(
            "note: could not open this project's store ({e}); looking in the central one. \
             Stop its server with `devctx serve --stop` if the memory is a local one."
        ),
    }
    let central = Central::open().context(
        "opening the central store (if a central daemon is running, stop it first: \
         `devctx serve --central --stop`)",
    )?;
    if central.store().forget_memory(&id)? {
        println!("Forgot {id} (central store).");
        return Ok(());
    }
    bail!("no memory with id `{id}` in this project or the central store");
}

/// `devctx memory-purge` — drop memories that were written under a key this
/// store has no business holding, and the vectors that came with them.
///
/// Goes straight at the database rather than through a running server: this is
/// destructive and rare, and having to stop the server first is a feature.
fn cmd_memory_purge(project: String, dry_run: bool) -> Result<()> {
    let cfg = load_project()?;
    let store = open_store(&cfg, configured_dimension(&cfg)).context(
        "opening the store (if a `devctx serve` is running, stop it first: `devctx serve --stop`)",
    )?;
    let n = store.count_memories_for_project(&project)?;
    if n == 0 {
        println!(
            "No memories stored under `{project}` in {}.",
            cfg.project.name
        );
        return Ok(());
    }
    if dry_run {
        println!(
            "{n} memories under `{project}` would be deleted from {} (dry run).",
            cfg.project.name
        );
        return Ok(());
    }
    let removed = store.purge_memories_for_project(&project)?;
    println!(
        "Deleted {removed} memories under `{project}` from {}, with their vectors.",
        cfg.project.name
    );
    Ok(())
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
    let mut to_central = 0usize;

    // Old stores had two scopes and used the shared one by default, so nearly
    // every row claims to be shared with the whole world when it is really
    // shared with this product's repositories. When the project declares a
    // group, that is where those memories belong.
    let group = cfg.project.group.clone();
    let shared_scope = if group.is_empty() {
        devctx_memory::SCOPE_GLOBAL
    } else {
        devctx_memory::SCOPE_GROUP
    };

    // Opened directly rather than through the daemon client, whose `remember`
    // takes a flat argument list and would drop each memory's original author,
    // files and timestamp — the history this import exists to preserve.
    let central = if mems.iter().any(|m| devctx_memory::is_global(&m.scope)) {
        Some(Central::open().context(
            "opening the central store (if a central daemon is running, stop it first: \
             `devctx serve --central --stop`)",
        )?)
    } else {
        None
    };

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
            scope: if devctx_memory::is_global(&m.scope) {
                shared_scope.to_string()
            } else {
                m.scope.clone()
            },
            group: group.clone(),
            author: m.author.clone(),
            // Old stores left `repo` empty on almost every row, which would
            // strand the imported globals: `recall --scope global --repo X`
            // filters on it. The originating project is the best attribution
            // available, so fall back to it rather than importing them blind.
            repo: if m.repo.is_empty() {
                m.project.clone()
            } else {
                m.repo.clone()
            },
            branch: m.branch.clone(),
            files: m.files.clone(),
            session_id: String::new(),
            now: m.created_at.clone(),
        };
        // A globally-scoped memory belongs to the central store, which is the
        // source of truth for it. Writing it to the project store instead left
        // it unreachable: `recall` looks for globals in the central store, and
        // the local path filters by project — which a global row never matches.
        let shared = devctx_memory::is_global(&req.scope) || devctx_memory::is_group(&req.scope);
        let status = if let (true, Some(c)) = (shared, central.as_ref()) {
            to_central += 1;
            c.remember(&req)?.status
        } else {
            remember(&store, embedder.as_ref(), &req)?.status
        };
        match status {
            devctx_memory::RememberStatus::Duplicate => dup += 1,
            _ => created += 1,
        }
    }
    if to_central > 0 {
        let tier = if group.is_empty() {
            "the global space".to_string()
        } else {
            format!("group `{group}`")
        };
        println!("{to_central} shared memories went to the central store, in {tier}.");
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
fn cmd_init(
    path: Option<PathBuf>,
    name: Option<String>,
    group: Option<String>,
    model: Option<String>,
    state_dir: Option<String>,
    yes: bool,
) -> Result<()> {
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

    // Inherit the machine's defaults — the embedding model, its directory, the
    // reranker — rather than the built-in ones.
    //
    // `projects add --init` already did this and `init` did not, so the two
    // ways of registering a repository produced different projects. The
    // difference was invisible where it mattered most: a machine configured for
    // a 384-dimensional multilingual model would get a *different*
    // 384-dimensional English one here, and nothing errors, because the widths
    // agree. The repository simply indexes itself into another vector space
    // than every other repository on the machine.
    let mut defaults = central_defaults();
    // Flags decide outright; otherwise ask, when there is someone to ask. A
    // non-interactive run keeps the machine defaults silently, which is what a
    // script or an agent needs — and `--yes` is how to get that on a terminal.
    let answers = if yes {
        init_wizard::Answers {
            model: model.clone(),
            state_dir: state_dir.clone(),
            group: group.clone(),
            ..Default::default()
        }
    } else {
        let asked = init_wizard::ask(
            &root,
            &defaults.embeddings,
            &models_in_use(),
            &groups_in_use(),
            &registered_project_names(),
        )?;
        init_wizard::Answers {
            // A flag that was passed was meant; it wins over the answer.
            model: model.clone().or(asked.model),
            state_dir: state_dir.clone().or(asked.state_dir),
            group: group.clone().or(asked.group),
            ..asked
        }
    };

    // Copying takes everything but this repository's own identity: the name and
    // path are what make it a different project, and the group is a claim about
    // this repository that the source cannot make for it.
    let mut copied: Option<ProjectConfig> = None;
    if let Some(src) = &answers.copy_from {
        let root = devctx_mcp::state::resolve_project_root(src).map_err(|e| anyhow!(e))?;
        let cfg = ProjectConfig::load(&root.join(devctx_core::CONFIG_FILE_NAME))
            .with_context(|| format!("reading the configuration of `{src}`"))?;
        defaults.embeddings = cfg.embeddings.clone();
        defaults.reranking = cfg.reranking.clone();
        copied = Some(cfg);
    }
    if let Some(key) = &answers.model {
        defaults.embeddings = choose_model(key, &defaults.embeddings)?;
    }
    if let Some(o) = answers.offline {
        defaults.embeddings.offline = o;
    }
    if let Some(r) = &answers.reranking {
        defaults.reranking = r.clone();
    }

    if !yes {
        println!(
            "\n{}",
            init_wizard::summary(&name, &answers, &defaults.embeddings.model)
        );
        if !init_wizard::confirm(answers.language.unwrap_or_default()) {
            println!(
                "{}",
                wizard_text::Text::new(answers.language.unwrap_or_default()).nothing_written()
            );
            return Ok(());
        }
    }

    let base = copied.unwrap_or_default();
    let cfg = ProjectConfig {
        project: Project {
            name,
            path: root.to_string_lossy().into_owned(),
            group: answers.group.clone().unwrap_or_default(),
        },
        state_dir: answers.state_dir.clone().unwrap_or(base.state_dir),
        language: answers.language.unwrap_or(base.language),
        embeddings: defaults.embeddings,
        storage: answers.storage.clone().unwrap_or(base.storage),
        indexing: answers.indexing.clone().unwrap_or(base.indexing),
        reranking: defaults.reranking,
        summarization: answers.summarization.clone().unwrap_or(base.summarization),
    };

    devctx_central::write_project_config(&cfg_path, &cfg)
        .with_context(|| format!("writing {}", cfg_path.display()))?;
    if answers.model.is_some() {
        adopt_as_machine_default(&cfg.embeddings);
    }

    println!("Initialized DevCtxEngine project at {}", cfg_path.display());
    warn_if_not_a_repo(&root);
    register_centrally(&root);
    Ok(())
}

/// Draws an indexing run that is happening inside the server.
///
/// The local path drives [`IndexBar`] straight from the pipeline. A routed
/// index runs in another process, so there is nothing to drive it with: this
/// polls `/index/progress` and places the bar at whatever the server reports.
///
/// Until the server has diffed there is no total to draw, and a server built
/// before that endpoint existed never answers at all. Both cases fall back to
/// the spinner and elapsed seconds that came before — because a progress bar
/// must never be the reason an index fails or looks broken.
struct ServerProgress {
    done: Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl ServerProgress {
    /// How long to wait between polls. Fast enough to feel live against a run
    /// measured in minutes, slow enough to be free.
    const POLL: std::time::Duration = std::time::Duration::from_millis(200);
    /// Consecutive failures after which we stop asking and just spin. Three
    /// covers a request lost to a busy moment; a server without the endpoint
    /// fails every time and settles here.
    const GIVE_UP_AFTER: u32 = 3;

    fn start(remote: remote::Remote, label: &str) -> Self {
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = done.clone();
        let label = label.to_string();
        let handle = std::thread::spawn(move || {
            let start = std::time::Instant::now();
            let frames = ['|', '/', '-', '\\'];
            let (mut tick, mut failures) = (0usize, 0u32);
            let mut bar: Option<IndexBar> = None;

            while !flag.load(std::sync::atomic::Ordering::Relaxed) {
                if failures < Self::GIVE_UP_AFTER {
                    match remote.index_progress() {
                        // `running` matters as much as the count: when a run
                        // ends its final totals stay behind for whoever polls
                        // last, so a second index would otherwise draw the
                        // previous run's finished bar until this one resets it.
                        Ok(p) if p.running && p.total > 0 => {
                            failures = 0;
                            let b = bar.get_or_insert_with(|| {
                                let b = IndexBar::new();
                                b.start(p.total);
                                b
                            });
                            b.set(p.done, &p.file);
                        }
                        // Reachable but not counting yet: still diffing.
                        Ok(_) => failures = 0,
                        Err(_) => failures += 1,
                    }
                }
                if bar.is_none() {
                    eprint!(
                        "\r{} {label}… {}s ",
                        frames[tick % frames.len()],
                        start.elapsed().as_secs()
                    );
                    let _ = std::io::Write::flush(&mut std::io::stderr());
                }
                tick += 1;
                std::thread::sleep(Self::POLL);
            }

            match bar {
                Some(b) => b.finish(),
                None => {
                    eprint!("\r{}\r", " ".repeat(label.len() + 24));
                    let _ = std::io::Write::flush(&mut std::io::stderr());
                }
            }
        });
        Self {
            done,
            handle: Some(handle),
        }
    }

    fn stop(mut self) {
        self.done.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Whether `path` sits inside a git work tree.
///
/// Indexing is built on `git diff`, so a directory outside one can be
/// registered and configured but never indexed. Saying so at the moment of
/// registering costs nothing and saves a confusing failure later.
fn is_git_repo(path: &std::path::Path) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn warn_if_not_a_repo(path: &std::path::Path) {
    if !is_git_repo(path) {
        warn_not_a_repo(&path.display().to_string());
    }
}

fn warn_not_a_repo(path: &str) {
    eprintln!(
        "· {path} is not a git repository, so it cannot be indexed yet.\n  \
         Run `git init` there (and commit something), then `devctx index`."
    );
}

/// Add a freshly initialized repo to the central registry so it is discoverable
/// from other projects.
///
/// Best-effort on purpose: a central store that cannot be opened (unwritable
/// home, or a daemon holding the file) must not make `devctx init` fail — the
/// project config, which is what actually matters here, is already written.
fn register_centrally(root: &std::path::Path) {
    // Route through the daemon exactly as `projects add` does. Opening the
    // central store directly would take a second lock on a file another project
    // may already hold — which is the whole reason the daemon exists, and which
    // `init` was quietly bypassing.
    let name = match CentralPaths::resolve() {
        Ok(paths) => match devctx_central::client::ensure(&paths) {
            Some(r) => r
                .add(root, None, "", "", false)
                .map(|v| field(&v, "name"))
                .map_err(|e| e.to_string()),
            None => Central::open()
                .and_then(|c| {
                    c.register(&RegisterRequest {
                        root: root.to_path_buf(),
                        now: devctx_central::now_stamp(),
                        ..Default::default()
                    })
                })
                .map(|rec| rec.name)
                .map_err(|e| e.to_string()),
        },
        Err(e) => Err(e.to_string()),
    };
    match name {
        Ok(name) => println!("Registered in the central store as `{name}`"),
        Err(e) => eprintln!("· not registered centrally ({e}); run `devctx projects add` later"),
    }
}

/// `devctx status` — discover and summarize the active project config.
fn cmd_status() -> Result<()> {
    note_update();
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
fn cmd_index(full: bool, branch: Option<String>) -> Result<()> {
    let cfg = load_project()?;
    // An explicit `--branch` wins; otherwise the project's declared default;
    // otherwise whatever is checked out, which is what the pipeline does with
    // `None`.
    let branch = branch.or_else(|| cfg.indexing.default_branch().map(str::to_string));
    if let Some(r) = remote::ensure(&cfg) {
        // The server does the work, so nothing local can drive the bar. Poll it
        // instead: elapsed seconds alone cannot tell a run that is nearly done
        // from one that has barely started, and on a large repository both look
        // exactly like a hang.
        let ticker = ServerProgress::start(r.clone(), "indexing on the server");
        let out = r.index(full);
        ticker.stop();
        println!("{}", out?);
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
        paths: None,
        exclude: &cfg.indexing.exclude,
        branch: branch.as_deref(),
    })?;
    progress.finish();
    devctx_mcp::state::report_index(&store, &root, &res);

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
        if store.enable_hnsw(&cfg.storage.metric)? {
            println!(
                "  HNSW index ready (VSS, metric {})",
                if cfg.storage.metric.is_empty() {
                    "cosine"
                } else {
                    &cfg.storage.metric
                }
            );
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

/// `devctx repair` — rebuild an index whose constraints no longer match its rows.
///
/// A server killed without closing its connection leaves a write-ahead log, and
/// DuckDB's replay of one does not restore the ART index behind a `PRIMARY KEY`
/// or `UNIQUE`. Everything still reads correctly; the first delete does not, and
/// since indexing begins by deleting, the repository can no longer be reindexed.
/// Rebuilding each table from its own rows puts the two back in agreement.
fn cmd_repair() -> Result<()> {
    let cfg = load_project()?;
    let path = cfg.db_path();
    if !path.exists() {
        println!("Nothing to repair: no index at {}.", path.display());
        return Ok(());
    }
    // The rebuild drops and recreates tables, so nothing else may hold the file.
    if remote::reclaim_db(&cfg) {
        println!("Stopped the server holding this index.");
    }
    // Any dimension opens an existing database — the schema is only created when
    // absent — and the rebuild reads the real width off the stored column.
    let store = open_store(&cfg, DEFAULT_DIM)?;
    let dim = store.stored_dimension()?;
    println!("Repairing {}…", path.display());
    let repaired = store.rebuild_indexes()?;
    println!(
        "  rebuilt {} table{} ({})",
        repaired.len(),
        if repaired.len() == 1 { "" } else { "s" },
        repaired.join(", ")
    );
    match dim {
        Some(d) => println!("  vector width preserved: {d}"),
        None => println!("  no vectors table was present"),
    }
    println!("Run `devctx index` to confirm.");
    Ok(())
}

/// Placeholder width for opening a database that already exists. The schema is
/// created only when absent, so this never reaches disk in that case.
const DEFAULT_DIM: usize = 768;

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
        let json = r.search(&query, limit, language.as_deref(), mode, !no_rerank)?;
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
            pool: cfg.reranking.pool,
            model: cfg.reranking.model.clone(),
            model_dir: (!cfg.reranking.model_dir.is_empty())
                .then(|| PathBuf::from(&cfg.reranking.model_dir)),
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
