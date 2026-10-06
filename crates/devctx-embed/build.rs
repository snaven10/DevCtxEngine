//! Derives the local engine's identity from `Cargo.lock`.
//!
//! The embedding fingerprint names the engine that makes local vectors, and a
//! vector may only be reused across runs when it is the same engine. A string
//! typed by hand goes stale the day `cargo update` moves `fastembed` or `ort`;
//! reading the lock at build time makes the fingerprint change with them.

use std::path::PathBuf;

#[path = "build/lock.rs"]
mod lock;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    // The workspace lock sits two levels up; a crate built outside the
    // workspace (a published tarball) has none.
    let lock_path = manifest_dir.join("../../Cargo.lock");
    let manifest_path = manifest_dir.join("Cargo.toml");
    println!("cargo:rerun-if-changed={}", lock_path.display());
    println!("cargo:rerun-if-changed={}", manifest_path.display());
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=build/lock.rs");
    let lock_text = std::fs::read_to_string(&lock_path).ok();
    let manifest = std::fs::read_to_string(&manifest_path).unwrap_or_default();
    let get = |p: &str| -> String {
        if let Some(text) = &lock_text {
            match lock::locked_version(text, &manifest, p) {
                Ok(Some(v)) => return v,
                Ok(None) => {}
                Err(e) => panic!("{e}"),
            }
        }
        // No lock to read: name what the manifest pins and the crate version,
        // so two builds with different engines do not share a fingerprint.
        format!(
            "unlocked({}@{})",
            lock::manifest_spec(&manifest, p).unwrap_or_else(|| "?".into()),
            std::env::var("CARGO_PKG_VERSION").unwrap_or_default()
        )
    };
    println!(
        "cargo:rustc-env=DEVCTX_LOCAL_ENGINE=fastembed-{}/ort-{}",
        get("fastembed"),
        get("ort")
    );
}
