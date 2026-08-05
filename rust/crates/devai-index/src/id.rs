//! Deterministic chunk IDs.

use std::fmt::Write as _;

use sha2::{Digest, Sha256};

/// Deterministic id for a code chunk: `sha256("{repo}:{branch}:{file}:{line}:{ordinal}")[:32]`.
///
/// The legacy scheme keyed on `(repo, branch, file, start_line)` only; the
/// `ordinal` disambiguates chunks that share a start line (e.g. the file-level
/// chunk and a top-of-file symbol), so no chunk is lost. Since the shared-store
/// sync that depended on cross-store id equality was dropped, ids are internal.
pub fn chunk_id(repo: &str, branch: &str, file: &str, start_line: u32, ordinal: usize) -> String {
    let key = format!("{repo}:{branch}:{file}:{start_line}:{ordinal}");
    let digest = Sha256::digest(key.as_bytes());
    let mut s = String::with_capacity(32);
    for b in &digest[..16] {
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_is_32_hex_and_deterministic() {
        let a = chunk_id("repo", "main", "src/a.rs", 1, 0);
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(a, chunk_id("repo", "main", "src/a.rs", 1, 0));
    }

    #[test]
    fn ordinal_disambiguates_same_line() {
        assert_ne!(
            chunk_id("repo", "main", "f.rs", 1, 0),
            chunk_id("repo", "main", "f.rs", 1, 1)
        );
    }
}
