//! `devai mcp configure` — register DevAI as an MCP server in an AI client.
//!
//! Writes an `mcpServers.<name>` entry (command + args + optional env) into the
//! client's JSON config, preserving any other content. Supports Claude Desktop
//! (global), Cursor and Claude Code (project or global).

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::ValueEnum;
use serde_json::{json, Map, Value};

/// Which AI client to configure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum McpClient {
    /// Claude Desktop (`claude_desktop_config.json`, global only).
    ClaudeDesktop,
    /// Claude Code (`.mcp.json` project, or `~/.claude.json` global).
    ClaudeCode,
    /// Cursor (`.cursor/mcp.json`).
    Cursor,
}

/// Where the config lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Scope {
    /// Project-local config.
    Project,
    /// User-global config.
    Global,
}

/// Options for a configure run.
pub struct Options {
    /// Target client.
    pub client: McpClient,
    /// Config scope.
    pub scope: Scope,
    /// Server name (key under `mcpServers`).
    pub name: String,
    /// Absolute project root (for `--project` arg and project-scope paths).
    pub project_root: PathBuf,
    /// Path to the `devai` binary.
    pub exe: PathBuf,
    /// `KEY=VALUE` env entries.
    pub envs: Vec<String>,
    /// Remove the entry instead of adding it.
    pub remove: bool,
    /// Only print the resulting config; don't write.
    pub show: bool,
}

/// Run the configure command.
pub fn run(opts: &Options) -> Result<()> {
    let home = home_dir().context("could not determine home directory")?;
    let path = config_path(opts.client, opts.scope, &opts.project_root, &home)?;

    let existing = read_json(&path)?;
    let entry = build_entry(&opts.exe, &opts.project_root, &opts.envs)?;
    let updated = apply_config(existing, &opts.name, entry, opts.remove);
    let pretty = serde_json::to_string_pretty(&updated)?;

    if opts.show {
        println!("# {}\n{pretty}", path.display());
        return Ok(());
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(&path, format!("{pretty}\n"))
        .with_context(|| format!("writing {}", path.display()))?;

    let verb = if opts.remove { "Removed" } else { "Configured" };
    println!("{verb} `{}` in {}", opts.name, path.display());
    Ok(())
}

/// The MCP server entry: `{ command, args, env? }`.
fn build_entry(exe: &Path, project_root: &Path, envs: &[String]) -> Result<Value> {
    let mut entry = json!({
        "command": exe.to_string_lossy(),
        "args": ["mcp", "--project", project_root.to_string_lossy()],
    });
    if !envs.is_empty() {
        let mut env_map = Map::new();
        for e in envs {
            let (k, v) = e
                .split_once('=')
                .with_context(|| format!("invalid --env `{e}` (expected KEY=VALUE)"))?;
            env_map.insert(k.to_string(), Value::String(v.to_string()));
        }
        entry
            .as_object_mut()
            .unwrap()
            .insert("env".to_string(), Value::Object(env_map));
    }
    Ok(entry)
}

/// Insert or remove `mcpServers.<name>` in `existing`, preserving other content.
fn apply_config(existing: Option<Value>, name: &str, entry: Value, remove: bool) -> Value {
    let mut root = match existing {
        Some(v) if v.is_object() => v,
        _ => json!({}),
    };
    let obj = root.as_object_mut().unwrap();
    let servers = obj.entry("mcpServers").or_insert_with(|| json!({}));
    if !servers.is_object() {
        *servers = json!({});
    }
    let servers = servers.as_object_mut().unwrap();
    if remove {
        servers.remove(name);
    } else {
        servers.insert(name.to_string(), entry);
    }
    root
}

/// Resolve the client's config file path.
fn config_path(
    client: McpClient,
    scope: Scope,
    project_root: &Path,
    home: &Path,
) -> Result<PathBuf> {
    Ok(match client {
        McpClient::ClaudeDesktop => {
            if scope == Scope::Project {
                bail!("Claude Desktop has no project scope; use --scope global");
            }
            claude_desktop_path(home)
        }
        McpClient::Cursor => match scope {
            Scope::Project => project_root.join(".cursor").join("mcp.json"),
            Scope::Global => home.join(".cursor").join("mcp.json"),
        },
        McpClient::ClaudeCode => match scope {
            Scope::Project => project_root.join(".mcp.json"),
            Scope::Global => home.join(".claude.json"),
        },
    })
}

/// OS-specific Claude Desktop config path.
fn claude_desktop_path(home: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library")
            .join("Application Support")
            .join("Claude")
            .join("claude_desktop_config.json")
    } else if cfg!(target_os = "windows") {
        // %APPDATA%\Claude — fall back to home-relative if APPDATA is unset.
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData").join("Roaming"))
            .join("Claude")
            .join("claude_desktop_config.json")
    } else {
        home.join(".config")
            .join("Claude")
            .join("claude_desktop_config.json")
    }
}

fn read_json(path: &Path) -> Result<Option<Value>> {
    match std::fs::read_to_string(path) {
        Ok(s) if s.trim().is_empty() => Ok(None),
        Ok(s) => Ok(Some(
            serde_json::from_str(&s).with_context(|| format!("parsing {}", path.display()))?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_inserts_and_preserves_other_keys() {
        let existing = json!({ "theme": "dark", "mcpServers": { "other": { "command": "x" } } });
        let entry = json!({ "command": "devai", "args": ["mcp"] });
        let out = apply_config(Some(existing), "devai", entry, false);
        assert_eq!(out["theme"], "dark");
        assert_eq!(out["mcpServers"]["other"]["command"], "x");
        assert_eq!(out["mcpServers"]["devai"]["command"], "devai");
    }

    #[test]
    fn apply_creates_structure_from_nothing() {
        let out = apply_config(None, "devai", json!({ "command": "d" }), false);
        assert_eq!(out["mcpServers"]["devai"]["command"], "d");
    }

    #[test]
    fn apply_removes_entry() {
        let existing = json!({ "mcpServers": { "devai": { "command": "d" }, "keep": {} } });
        let out = apply_config(Some(existing), "devai", json!({}), true);
        assert!(out["mcpServers"].get("devai").is_none());
        assert!(out["mcpServers"].get("keep").is_some());
    }

    #[test]
    fn entry_has_project_arg_and_env() {
        let e = build_entry(
            Path::new("/usr/bin/devai"),
            Path::new("/home/u/proj"),
            &["KEY=VAL".to_string()],
        )
        .unwrap();
        assert_eq!(e["command"], "/usr/bin/devai");
        assert_eq!(e["args"][0], "mcp");
        assert_eq!(e["args"][2], "/home/u/proj");
        assert_eq!(e["env"]["KEY"], "VAL");
    }

    #[test]
    fn paths_resolve_per_client() {
        let home = Path::new("/home/u");
        let proj = Path::new("/home/u/proj");
        assert_eq!(
            config_path(McpClient::ClaudeCode, Scope::Project, proj, home).unwrap(),
            proj.join(".mcp.json")
        );
        assert_eq!(
            config_path(McpClient::Cursor, Scope::Global, proj, home).unwrap(),
            home.join(".cursor/mcp.json")
        );
        assert!(config_path(McpClient::ClaudeDesktop, Scope::Project, proj, home).is_err());
    }

    #[test]
    fn invalid_env_is_rejected() {
        assert!(build_entry(Path::new("d"), Path::new("p"), &["NOEQ".to_string()]).is_err());
    }
}
