//! Git hook installation: keep a repository's index fresh without anyone
//! remembering to run `devctx index`.
//!
//! A `post-commit` hook is the cheapest automation that actually works. It fires
//! exactly when the commit-diff pipeline has something new to look at, needs no
//! daemon watching the filesystem, and costs nothing when idle. (Uncommitted
//! work is a different problem, solved by indexing explicit paths.)
//!
//! The hook body is written between markers so an existing hook is extended
//! rather than clobbered, and can be removed again cleanly.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

/// Opening marker of the managed block.
const BEGIN: &str = "# >>> devctx (managed) >>>";
/// Closing marker of the managed block.
const END: &str = "# <<< devctx (managed) <<<";

/// The hooks DevCtxEngine manages.
const HOOK: &str = "post-commit";

/// Path of the repository's hooks directory, honouring `core.hooksPath`.
fn hooks_dir(repo_root: &Path) -> Result<PathBuf> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["rev-parse", "--git-path", "hooks"])
        .output()
        .context("asking git where its hooks live")?;
    if !out.status.success() {
        bail!("{} is not a git repository", repo_root.display());
    }
    let rel = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let path = PathBuf::from(&rel);
    Ok(if path.is_absolute() {
        path
    } else {
        repo_root.join(path)
    })
}

/// The managed block: re-index in the background so committing stays instant.
///
/// `devctx index` routes to the project's server, which owns the database and
/// keeps the model warm, so the work is usually finished before the next
/// command needs it.
fn block(exe: &Path) -> String {
    format!(
        "{BEGIN}\n\
         # Re-index this repository after each commit. Detached and silent: a\n\
         # commit must never wait on indexing, and must never fail because of it.\n\
         (\"{exe}\" index >/dev/null 2>&1 &) || true\n\
         {END}\n",
        exe = exe.display()
    )
}

/// Strip any existing managed block, returning the rest of the script.
fn without_block(existing: &str) -> String {
    let (Some(start), Some(end)) = (existing.find(BEGIN), existing.find(END)) else {
        return existing.to_string();
    };
    if end < start {
        return existing.to_string();
    }
    let tail = &existing[end + END.len()..];
    let mut out = String::with_capacity(existing.len());
    out.push_str(&existing[..start]);
    out.push_str(tail.strip_prefix('\n').unwrap_or(tail));
    out
}

/// Whether a hook script carries our managed block.
fn has_block(script: &str) -> bool {
    script.contains(BEGIN)
}

/// Install (or refresh) the `post-commit` hook.
pub fn install(repo_root: &Path) -> Result<PathBuf> {
    let dir = hooks_dir(repo_root)?;
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(HOOK);

    let exe = std::env::current_exe().context("locating the devctx binary")?;
    let existing = std::fs::read_to_string(&path).unwrap_or_default();

    let mut script = if existing.trim().is_empty() {
        "#!/bin/sh\n".to_string()
    } else {
        // Refreshing: drop our old block, keep whatever else the user has.
        let kept = without_block(&existing);
        if kept.trim_end().is_empty() {
            "#!/bin/sh\n".to_string()
        } else {
            let mut k = kept.trim_end().to_string();
            k.push('\n');
            k
        }
    };
    script.push_str(&block(&exe));

    std::fs::write(&path, script).with_context(|| format!("writing {}", path.display()))?;
    make_executable(&path)?;
    Ok(path)
}

/// Remove the managed block, deleting the hook if nothing else remains.
pub fn uninstall(repo_root: &Path) -> Result<bool> {
    let path = hooks_dir(repo_root)?.join(HOOK);
    let Ok(existing) = std::fs::read_to_string(&path) else {
        return Ok(false);
    };
    if !has_block(&existing) {
        return Ok(false);
    }

    let kept = without_block(&existing);
    // Only a shebang left means the hook was ours alone.
    if kept
        .lines()
        .all(|l| l.trim().is_empty() || l.starts_with("#!"))
    {
        std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
    } else {
        std::fs::write(&path, kept).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(true)
}

/// Whether the managed hook is currently installed.
pub fn installed(repo_root: &Path) -> Result<bool> {
    let path = hooks_dir(repo_root)?.join(HOOK);
    Ok(std::fs::read_to_string(&path)
        .map(|s| has_block(&s))
        .unwrap_or(false))
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(perms.mode() | 0o755);
    std::fs::set_permissions(path, perms).with_context(|| format!("chmod {}", path.display()))
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_managed_block() {
        let script = format!("#!/bin/sh\necho hi\n{BEGIN}\nmanaged\n{END}\necho bye\n");
        assert!(has_block(&script));
        let stripped = without_block(&script);
        assert_eq!(stripped, "#!/bin/sh\necho hi\necho bye\n");
        assert!(!has_block(&stripped));
    }

    /// A hook we do not manage must survive untouched.
    #[test]
    fn leaves_a_foreign_hook_alone() {
        let foreign = "#!/bin/sh\nmake lint\n";
        assert!(!has_block(foreign));
        assert_eq!(without_block(foreign), foreign);
    }

    /// Malformed markers must not corrupt the script.
    #[test]
    fn ignores_reversed_or_partial_markers() {
        let reversed = format!("#!/bin/sh\n{END}\n{BEGIN}\n");
        assert_eq!(without_block(&reversed), reversed);
        let partial = format!("#!/bin/sh\n{BEGIN}\nno end marker\n");
        assert_eq!(without_block(&partial), partial);
    }

    #[test]
    fn the_block_runs_index_detached_and_never_fails_a_commit() {
        let b = block(Path::new("/usr/local/bin/devctx"));
        assert!(b.contains("\"/usr/local/bin/devctx\" index"));
        assert!(b.contains('&'), "must not block the commit");
        assert!(b.contains("|| true"), "must not fail the commit");
    }
}
