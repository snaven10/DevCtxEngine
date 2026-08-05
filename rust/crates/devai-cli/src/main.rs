//! `devai` — command-line entry point for the DevAI Rust rewrite (F0 skeleton).
//!
//! Only the scaffolding commands are implemented here (`init`, `status`).
//! Indexing, search, memory and the MCP server land in later phases — see
//! `docs/rust-rewrite-plan.md` §8.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use devai_core::config::{find_config_file, Project, ProjectConfig};

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
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Init { path, name } => cmd_init(path, name),
        Command::Status => cmd_status(),
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
