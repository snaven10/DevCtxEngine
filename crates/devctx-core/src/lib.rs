//! `devctx-core` — shared types, configuration and errors for the DevCtxEngine Rust rewrite.
//!
//! See `docs/architecture-spec.md` for the overall architecture. This crate is
//! dependency-light on purpose: every other crate builds on top of it.

pub mod config;
pub mod dirs;
pub mod error;
pub mod exe;
pub mod fallback;
pub mod gitenv;
pub mod hits;
pub mod kind;
pub mod modelload;
pub mod plans;
pub mod procmem;
pub mod procown;
pub mod rank;
pub mod types;

pub use config::{ProjectConfig, CONFIG_FILE_NAME};
pub use error::{Error, Result};
pub use exe::self_exe;
pub use fallback::CudaFallback;
pub use gitenv::{clean_git_env, git_vars_to_strip, GIT_IDENTITY_ENV, GIT_REPO_ENV};
pub use hits::{hit_raw_score, search_hits, SearchHits};
pub use kind::{path_kind, KindPenalty, PathKind};
pub use rank::{fuse_by_rank, rank_score};
pub use types::{SearchFilter, SearchResult, VectorMetadata, VectorPoint};

/// The DevCtxEngine version, sourced from the crate's `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
