//! Where DevCtxEngine keeps its own files.
//!
//! Two things live outside any repository: the central store, and the models
//! downloaded on first use. Both are shared by every project on the machine, so
//! both belong under one user-level directory rather than inside whichever
//! checkout happened to be the working directory at the time.

use std::path::PathBuf;

/// Relocates everything DevCtxEngine owns — central store and model cache — to
/// one directory. Primarily for tests and CI.
pub const HOME_ENV: &str = "DEVCTX_HOME";

/// Overrides just the model cache, for putting a few gigabytes on a different
/// disk without moving anything else.
pub const MODEL_CACHE_ENV: &str = "DEVCTX_MODEL_CACHE";

/// The directory holding DevCtxEngine's own data.
///
/// [`HOME_ENV`] wins, then `$XDG_DATA_HOME/devctx`, then
/// `~/.local/share/devctx`. `None` when no home can be determined at all, which
/// leaves callers free to fall back to their own default rather than failing.
pub fn data_dir() -> Option<PathBuf> {
    if let Some(home) = env_dir(HOME_ENV) {
        return Some(home);
    }
    if let Some(xdg) = env_dir("XDG_DATA_HOME") {
        return Some(xdg.join("devctx"));
    }
    home_dir().map(|h| h.join(".local").join("share").join("devctx"))
}

/// The user's config directory for DevCtxEngine.
pub fn config_dir() -> Option<PathBuf> {
    if let Some(home) = env_dir(HOME_ENV) {
        return Some(home);
    }
    if let Some(xdg) = env_dir("XDG_CONFIG_HOME") {
        return Some(xdg.join("devctx"));
    }
    home_dir().map(|h| h.join(".config").join("devctx"))
}

/// Where downloaded embedding and reranking models are cached.
///
/// Deliberately **not** per project. The models are identical whoever asks for
/// them and run to hundreds of megabytes each, so a per-project cache would
/// re-download the same files for every repository and keep a copy of each. One
/// shared directory downloads once and is reused everywhere.
pub fn model_cache_dir() -> Option<PathBuf> {
    if let Some(explicit) = env_dir(MODEL_CACHE_ENV) {
        return Some(explicit);
    }
    data_dir().map(|d| d.join("models"))
}

fn env_dir(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The model cache must sit beside the central store, not inside a
    /// repository: it is shared, and it is large.
    #[test]
    fn the_model_cache_hangs_off_the_data_directory() {
        let data = data_dir().expect("a home in the test environment");
        let models = model_cache_dir().expect("a model cache");
        assert_eq!(models, data.join("models"));
        assert!(
            models.is_absolute(),
            "a relative cache would land in the working directory: {models:?}"
        );
    }
}
