//! `devctx-core` — shared types, configuration and errors for the DevCtxEngine Rust rewrite.
//!
//! See `docs/rust-rewrite-plan.md` for the overall architecture. This crate is
//! dependency-light on purpose: every other crate builds on top of it.

pub mod config;
pub mod dirs;
pub mod error;
pub mod rank;
pub mod types;

pub use config::{ProjectConfig, CONFIG_FILE_NAME};
pub use error::{Error, Result};
pub use rank::{fuse_by_rank, rank_score};
pub use types::{SearchFilter, SearchResult, VectorMetadata, VectorPoint};

/// The DevCtxEngine version, sourced from the crate's `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
