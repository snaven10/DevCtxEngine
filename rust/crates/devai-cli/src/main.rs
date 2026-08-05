//! `devai` — command-line entry point for the DevAI Rust rewrite.
//!
//! F5 wires the real pipeline: `init`, `status`, `index`, `search`. Building the
//! embedder pulls in fastembed/ort (the `local` provider) and downloads the
//! model on first use.

mod mcp_configure;

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use devai_core::config::{find_config_file, Project, ProjectConfig};
use devai_core::{SearchFilter, SearchResult};
use devai_embed::{create_provider, EmbedSettings, EmbeddingProvider};
use devai_index::{run as index_run, IndexRequest};
use devai_memory::{memory_stats, recall, remember, RememberRequest};
use devai_rerank::{create_reranker, RerankSettings};
use devai_store::Store;
use mcp_configure::{McpClient, Options, Scope};
use serde::Serialize;

/// Git-aware AI code intelligence tool.
#[derive(Debug, Parser)]
#[command(name = "devai", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Initialize DevAI tracking for a repository.
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
}

/// Search output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum OutputFormat {
    /// Human-readable table.
    Table,
    /// JSON array.
    Json,
}

/// Actions under `devai mcp`.
#[derive(Debug, Subcommand)]
enum McpAction {
    /// Register DevAI as an MCP server in an AI client.
    Configure {
        /// Target client.
        #[arg(long, value_enum, default_value_t = McpClient::ClaudeCode)]
        client: McpClient,
        /// Config scope.
        #[arg(long, value_enum, default_value_t = Scope::Project)]
        scope: Scope,
        /// Server name under `mcpServers`.
        #[arg(long, default_value = "devai")]
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
        } => cmd_search(query, limit, language, format, no_rerank),
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
        Command::Impact { symbol, depth } => cmd_impact(symbol, depth),
        Command::Routes { method, path } => cmd_routes(method, path),
    }
}

/// `devai mcp` — run the MCP server over stdio.
fn cmd_mcp(project: Option<PathBuf>) -> Result<()> {
    let cfg = match project {
        Some(root) => ProjectConfig::load(&root.join(devai_core::CONFIG_FILE_NAME))
            .with_context(|| format!("loading project at {}", root.display()))?,
        None => load_project()?,
    };
    eprintln!("Starting DevAI MCP server (stdio)…");
    devai_mcp::run_stdio(cfg)
}

/// `devai mcp configure` — register DevAI as an MCP server in an AI client.
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
    let exe = std::env::current_exe().context("resolving the devai binary path")?;
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

/// `devai remember` — save a memory.
fn cmd_remember(
    content: String,
    title: Option<String>,
    memory_type: String,
    topic: Option<String>,
    tags: Option<String>,
) -> Result<()> {
    let cfg = load_project()?;
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

/// `devai recall` — recall memories relevant to a query.
fn cmd_recall(query: String, limit: usize) -> Result<()> {
    let cfg = load_project()?;
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

/// `devai impact` — show the blast radius of a symbol.
fn cmd_impact(symbol: String, depth: usize) -> Result<()> {
    let cfg = load_project()?;
    let store = open_store(&cfg, configured_dimension(&cfg))?;
    let git = devai_index::GitRepo::open(&project_root(&cfg)?)?;
    let branch = git.state().branch;
    let impact = store.impact_analysis(&git.short_name(), &branch, &symbol, depth)?;

    println!("Impact of `{symbol}` (depth {depth}):");
    print_impact("callers (upstream)", &impact.upstream);
    print_impact("callees (downstream)", &impact.downstream);
    Ok(())
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

/// `devai routes` — list framework-aware HTTP routes.
fn cmd_routes(method: Option<String>, path: Option<String>) -> Result<()> {
    let cfg = load_project()?;
    let store = open_store(&cfg, configured_dimension(&cfg))?;
    let git = devai_index::GitRepo::open(&project_root(&cfg)?)?;
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

/// `devai memory-stats` — show memory counts for the project.
fn cmd_memory_stats() -> Result<()> {
    let cfg = load_project()?;
    let store = open_store(&cfg, configured_dimension(&cfg))?;
    let stats = memory_stats(&store, &project_name(&cfg))?;
    println!("{} memories in {}", stats.total, cfg.project.name);
    for (ty, n) in &stats.by_type {
        println!("  {ty}: {n}");
    }
    Ok(())
}

/// `devai init` — write a `.devai/config.yaml` for the target repo.
fn cmd_init(path: Option<PathBuf>, name: Option<String>) -> Result<()> {
    let root = match path {
        Some(p) => p,
        None => std::env::current_dir().context("resolving current directory")?,
    };
    let root = std::fs::canonicalize(&root)
        .with_context(|| format!("resolving path {}", root.display()))?;

    let cfg_path = root.join(devai_core::CONFIG_FILE_NAME);
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

    println!("Initialized DevAI project at {}", cfg_path.display());
    Ok(())
}

/// `devai status` — discover and summarize the active project config.
fn cmd_status() -> Result<()> {
    let cwd = std::env::current_dir().context("resolving current directory")?;
    let Some(cfg_path) = find_config_file(&cwd) else {
        println!("No DevAI project found (run `devai init` first).");
        return Ok(());
    };
    let cfg = ProjectConfig::load(&cfg_path)?;

    println!("DevAI {}", devai_core::VERSION);
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

/// `devai index` — run the indexing pipeline.
fn cmd_index(full: bool) -> Result<()> {
    let cfg = load_project()?;
    let root = project_root(&cfg)?;
    eprintln!(
        "Loading embedder ({} / {})…",
        cfg.embeddings.model, cfg.embeddings.provider
    );
    let embedder = build_embedder(&cfg)?;
    let store = open_store(&cfg, embedder.dimension())?;

    let res = index_run(IndexRequest {
        store: &store,
        embedder: embedder.as_ref(),
        repo_root: &root,
        incremental: !full,
        model_name: &cfg.embeddings.model,
    })?;

    println!(
        "Indexed {} ({}) @ {}",
        cfg.project.name,
        res.branch,
        short_commit(&res.commit)
    );
    println!(
        "  {} ({} files, {} skipped, {} deleted, {} renamed)",
        if res.full_reindex {
            "full reindex"
        } else {
            "incremental"
        },
        res.files_indexed,
        res.files_skipped,
        res.files_deleted,
        res.files_renamed,
    );
    println!("  {} symbols, {} chunks stored", res.symbols, res.chunks);
    Ok(())
}

/// Candidates fetched from vector search before reranking down to `limit`.
const RERANK_FETCH: usize = 15;

/// `devai search` — embed the query, search the store, and (optionally) rerank.
fn cmd_search(
    query: String,
    limit: usize,
    language: Option<String>,
    format: OutputFormat,
    no_rerank: bool,
) -> Result<()> {
    let cfg = load_project()?;
    let embedder = build_embedder(&cfg)?;
    let store = open_store(&cfg, embedder.dimension())?;

    let rerank = !no_rerank && cfg.reranking.enabled;
    let reranker = if rerank {
        eprintln!("Loading reranker ({})…", cfg.reranking.model);
        Some(create_reranker(&RerankSettings {
            enabled: true,
            model: cfg.reranking.model.clone(),
        })?)
    } else {
        None
    };

    let qvec = embedder.embed_query(&query)?;
    let filter = SearchFilter {
        languages: language.into_iter().collect(),
        exclude_deletions: true,
        ..Default::default()
    };
    let fetch_k = if rerank {
        limit.max(RERANK_FETCH)
    } else {
        limit
    };
    let hits = store.search(&qvec, &filter, fetch_k)?;

    let hits = match reranker {
        Some(r) => {
            let texts: Vec<String> = hits.iter().map(|h| h.point.text.clone()).collect();
            r.rerank(&query, &texts, limit)?
                .into_iter()
                .map(|ranked| {
                    let mut hit = hits[ranked.index].clone();
                    hit.score = ranked.score;
                    hit
                })
                .collect()
        }
        None => hits,
    };

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
        bail!("No DevAI project found (run `devai init` first).");
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
    use devai_embed::registry;
    let model = &cfg.embeddings.model;
    match cfg.embeddings.provider.as_str() {
        "openai" => registry::openai_dimension(model).unwrap_or(1536),
        "voyage" => registry::voyage_dimension(model).unwrap_or(1024),
        "custom" => std::env::var("DEVAI_EMBED_DIMENSION")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(384),
        _ => registry::find_local(model)
            .map(|m| m.dimension)
            .unwrap_or(384),
    }
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
    use devai_core::{VectorMetadata, VectorPoint};

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
