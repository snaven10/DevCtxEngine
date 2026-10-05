//! Locating the running `devctx` binary, even after it was replaced on disk.

use std::io;
use std::path::{Path, PathBuf};

/// Suffix the kernel appends to `/proc/self/exe` once the file was unlinked or
/// replaced (`cp`/`install` over a running binary).
const DELETED_SUFFIX: &str = " (deleted)";

/// Path to spawn when this process needs to run `devctx` again.
///
/// `std::env::current_exe()` reads `/proc/self/exe`; after a reinstall it
/// returns `"<path> (deleted)"`, which does not exist, so `Command::new` fails
/// with ENOENT and a long-lived MCP can no longer start its server. Prefer the
/// path without the suffix when something is there (the freshly installed
/// binary); otherwise `/proc/self/exe` still executes the inode this process
/// is running from.
pub fn self_exe() -> io::Result<PathBuf> {
    let raw = std::env::current_exe()?;
    Ok(resolve_exe(&raw, Path::exists))
}

/// The pure part of [`self_exe`]: pick a path given what `current_exe` said and
/// a way to ask whether a path exists.
pub fn resolve_exe(raw: &Path, exists: impl Fn(&Path) -> bool) -> PathBuf {
    if exists(raw) {
        return raw.to_path_buf();
    }
    let stripped = raw
        .to_str()
        .and_then(|s| s.strip_suffix(DELETED_SUFFIX))
        .map(PathBuf::from);
    if let Some(p) = stripped {
        if exists(&p) {
            return p;
        }
        return PathBuf::from("/proc/self/exe");
    }
    raw.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn existing_path_is_kept() {
        let p = Path::new("/opt/devctx");
        assert_eq!(resolve_exe(p, |_| true), p);
    }

    #[test]
    fn deleted_suffix_is_stripped_when_replacement_exists() {
        let raw = Path::new("/home/u/.local/bin/devctx (deleted)");
        let got = resolve_exe(raw, |p| p == Path::new("/home/u/.local/bin/devctx"));
        assert_eq!(got, Path::new("/home/u/.local/bin/devctx"));
    }

    #[test]
    fn deleted_without_replacement_uses_proc_self_exe() {
        let raw = Path::new("/home/u/.local/bin/devctx (deleted)");
        assert_eq!(resolve_exe(raw, |_| false), Path::new("/proc/self/exe"));
    }

    #[test]
    fn missing_without_suffix_is_returned_as_is() {
        let raw = Path::new("/gone/devctx");
        assert_eq!(resolve_exe(raw, |_| false), raw);
    }
}
