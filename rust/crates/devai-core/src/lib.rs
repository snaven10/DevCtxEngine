//! `devai-core` — shared types, configuration and errors for the DevAI Rust rewrite.
//!
//! See `docs/rust-rewrite-plan.md` for the overall architecture. This crate is
//! dependency-light on purpose: every other crate builds on top of it.

pub mod config;
pub mod error;

pub use config::{ProjectConfig, CONFIG_FILE_NAME};
pub use error::{Error, Result};

/// The DevAI version, sourced from the crate's `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
