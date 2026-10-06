//! Derives the local engine's identity from `Cargo.lock`.
//!
//! The embedding fingerprint names the engine that makes local vectors, and a
//! vector may only be reused across runs when it is the same engine. A string
//! typed by hand goes stale the day `cargo update` moves `fastembed` or `ort`;
//! reading the lock at build time makes the fingerprint change with them.

use std::path::PathBuf;

fn locked_version(lock: &str, package: &str) -> Option<String> {
    let mut lines = lock.lines();
    while let Some(l) = lines.next() {
        if l.trim() == format!("name = \"{package}\"") {
            let v = lines.next()?.trim();
            return v
                .strip_prefix("version = \"")
                .and_then(|v| v.strip_suffix('"'))
                .map(str::to_string);
        }
    }
    None
}

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    // The workspace lock sits two levels up; a crate built outside the
    // workspace (a published tarball) has none and says so in the fingerprint.
    let lock_path = manifest.join("../../Cargo.lock");
    println!("cargo:rerun-if-changed={}", lock_path.display());
    println!("cargo:rerun-if-changed=build.rs");
    let lock = std::fs::read_to_string(&lock_path).unwrap_or_default();
    let get = |p: &str| locked_version(&lock, p).unwrap_or_else(|| "unlocked".to_string());
    println!(
        "cargo:rustc-env=DEVCTX_LOCAL_ENGINE=fastembed-{}/ort-{}",
        get("fastembed"),
        get("ort")
    );
}
