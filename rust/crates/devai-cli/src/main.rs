//! `devai` — command-line entry point for the DevAI Rust rewrite.
//!
//! F5 wires the real pipeline: `init`, `status`, `index`, `search`. Building the
//! embedder pulls in fastembed/ort (the `local` provider) and downloads the
//! model on first use.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use devai_core::config::{find_config_file, Project, ProjectConfig};
use devai_core::{SearchFilter, SearchResult};
use devai_embed::{create_provider, EmbedSettings, EmbeddingProvider};
use devai_index::{run as index_run, IndexRequest};
use devai_store::Store;
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
        } => cmd_search(query, limit, language, format),
    }
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

/// `devai search` — embed the query and search the store.
fn cmd_search(
    query: String,
    limit: usize,
    language: Option<String>,
    format: OutputFormat,
) -> Result<()> {
    let cfg = load_project()?;
    let embedder = build_embedder(&cfg)?;
    let store = open_store(&cfg, embedder.dimension())?;

    let qvec = embedder.embed_query(&query)?;
    let filter = SearchFilter {
        languages: language.into_iter().collect(),
        exclude_deletions: true,
        ..Default::default()
    };
    let hits = store.search(&qvec, &filter, limit)?;

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
