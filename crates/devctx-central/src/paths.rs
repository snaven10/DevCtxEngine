//! Where the central store lives on disk.
//!
//! By default the data follows the XDG layout (`~/.local/share/devctx`) with the
//! config alongside the user's other configs (`~/.config/devctx/config.yaml`).
//! Setting [`HOME_ENV`] collapses both under a single directory, which is how
//! tests and CI stay off the real user directories.

use std::path::{Path, PathBuf};

use crate::error::{CentralError, Result};

/// Relocates the entire central home — data *and* config — to one directory.
pub const HOME_ENV: &str = "DEVCTX_HOME";

/// Resolved locations of everything the central store owns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CentralPaths {
    /// Directory holding the database and the discovery file.
    pub dir: PathBuf,
    /// The central config file.
    pub config: PathBuf,
    /// The central DuckDB file.
    pub db: PathBuf,
    /// Discovery file advertising the central daemon (`serve --central`).
    pub serve_file: PathBuf,
}

impl CentralPaths {
    /// Resolve from the environment: [`HOME_ENV`] wins, then XDG, then `$HOME`.
    pub fn resolve() -> Result<Self> {
        if let Some(home) = std::env::var_os(HOME_ENV).filter(|v| !v.is_empty()) {
            return Ok(Self::rooted_at(Path::new(&home)));
        }
        let data = match env_dir("XDG_DATA_HOME") {
            Some(d) => d.join("devctx"),
            None => home_dir()?.join(".local").join("share").join("devctx"),
        };
        let config = match env_dir("XDG_CONFIG_HOME") {
            Some(d) => d.join("devctx"),
            None => home_dir()?.join(".config").join("devctx"),
        };
        Ok(Self {
            config: config.join("config.yaml"),
            db: data.join("central.duckdb"),
            serve_file: data.join("serve.json"),
            dir: data,
        })
    }

    /// Put config, database and discovery file together under one directory.
    pub fn rooted_at(dir: &Path) -> Self {
        Self {
            config: dir.join("config.yaml"),
            db: dir.join("central.duckdb"),
            serve_file: dir.join("serve.json"),
            dir: dir.to_path_buf(),
        }
    }
}

fn env_dir(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .ok_or(CentralError::NoHome)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rooted_puts_everything_in_one_place() {
        let p = CentralPaths::rooted_at(Path::new("/tmp/devctx-home"));
        assert_eq!(p.config, Path::new("/tmp/devctx-home/config.yaml"));
        assert_eq!(p.db, Path::new("/tmp/devctx-home/central.duckdb"));
        assert_eq!(p.serve_file, Path::new("/tmp/devctx-home/serve.json"));
    }
}
