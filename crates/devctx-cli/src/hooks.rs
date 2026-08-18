//! Git hook installation: keep a repository's index fresh without anyone
//! remembering to run `devctx index`.
//!
//! Git hooks are the cheapest automation that actually works: they fire exactly
//! when the diff pipeline has something new to look at, need no daemon watching
//! the filesystem, and cost nothing when idle. (Uncommitted work is a different
//! problem, solved by `devctx watch` or by indexing explicit paths.)
//!
//! Two hooks, because one does not cover the common case. `post-commit` does not
//! run on a merge or on a fast-forward `pull` — git uses `post-merge` for both.
//! Installing only `post-commit` therefore leaves the index stale exactly after
//! merging a PR or pulling someone else's work, which is when it is most likely
//! to be asked a question it can no longer answer correctly.
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
///
/// `post-commit` catches your own commits; `post-merge` catches merges and
/// fast-forward pulls, which `post-commit` never sees. Rebase, checkout and
/// reset are still uncovered — they are rare enough that a periodic sweep is
/// the honest answer there, not a fourth hook.
const HOOKS: &[&str] = &["post-commit", "post-merge"];

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

/// The managed block: re-index in the background so git stays instant.
///
/// `devctx index` routes to the project's server, which owns the database and
/// keeps the model warm, so the work is usually finished before the next
/// command needs it.
///
/// `when` is woven into the comment so a reader of the hook file learns what
/// this particular one is for; the body is identical either way.
fn block(exe: &Path, hook: &str) -> String {
    let when = match hook {
        "post-merge" => "after a merge or a fast-forward pull",
        _ => "after each commit",
    };
    format!(
        "{BEGIN}\n\
         # Re-index this repository {when}. Detached and silent: git must never\n\
         # wait on indexing, and must never fail because of it.\n\
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

/// Install (or refresh) every managed hook. Returns the paths written.
///
/// Safe to re-run, and re-running is how an older install that predates
/// `post-merge` picks it up.
pub fn install(repo_root: &Path) -> Result<Vec<PathBuf>> {
    let dir = hooks_dir(repo_root)?;
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let exe = std::env::current_exe().context("locating the devctx binary")?;

    let mut written = Vec::with_capacity(HOOKS.len());
    for hook in HOOKS {
        let path = dir.join(hook);
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
        script.push_str(&block(&exe, hook));

        std::fs::write(&path, script).with_context(|| format!("writing {}", path.display()))?;
        make_executable(&path)?;
        written.push(path);
    }
    Ok(written)
}

/// Remove our block from every managed hook, deleting a hook file when nothing
/// else remains in it. Returns whether anything was removed.
///
/// Each hook is handled independently: a `post-commit` that also runs your
/// linter keeps the linter, while a `post-merge` that was ours alone is deleted.
pub fn uninstall(repo_root: &Path) -> Result<bool> {
    let dir = hooks_dir(repo_root)?;
    let mut removed_any = false;

    for hook in HOOKS {
        let path = dir.join(hook);
        let Ok(existing) = std::fs::read_to_string(&path) else {
            continue;
        };
        if !has_block(&existing) {
            continue;
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
        removed_any = true;
    }
    Ok(removed_any)
}

/// Which managed hooks are currently installed, and which are missing.
///
/// Reported per hook rather than as one boolean: an install that predates
/// `post-merge` is *partly* there, and saying "installed" would hide exactly the
/// gap the user needs to close by re-running `hooks install`.
pub fn status(repo_root: &Path) -> Result<Vec<(&'static str, bool)>> {
    let dir = hooks_dir(repo_root)?;
    Ok(HOOKS
        .iter()
        .map(|hook| {
            let present = std::fs::read_to_string(dir.join(hook))
                .map(|s| has_block(&s))
                .unwrap_or(false);
            (*hook, present)
        })
        .collect())
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
        let b = block(Path::new("/usr/local/bin/devctx"), "post-commit");
        assert!(b.contains("\"/usr/local/bin/devctx\" index"));
        assert!(b.contains('&'), "must not block the commit");
        assert!(b.contains("|| true"), "must not fail the commit");
    }

    /// The gap this exists to close: `post-commit` never fires on a merge or a
    /// fast-forward pull, so an index kept only by that hook goes stale exactly
    /// after merging a PR.
    #[test]
    fn post_merge_is_managed_too() {
        assert!(HOOKS.contains(&"post-commit"));
        assert!(HOOKS.contains(&"post-merge"));
    }

    /// Both hooks run the same command; only the comment differs, so whoever
    /// opens the file learns which event this one is for.
    #[test]
    fn each_hook_explains_when_it_fires() {
        let exe = Path::new("/usr/local/bin/devctx");
        let commit = block(exe, "post-commit");
        let merge = block(exe, "post-merge");

        assert!(commit.contains("after each commit"));
        assert!(merge.contains("after a merge or a fast-forward pull"));

        // Same body, or one of them is not actually indexing.
        for b in [&commit, &merge] {
            assert!(b.contains("\"/usr/local/bin/devctx\" index"));
            assert!(b.contains("|| true"));
        }
    }

    /// A block written for one hook must still be strippable, or `uninstall`
    /// would leave the merge hook behind.
    #[test]
    fn a_post_merge_block_round_trips_like_any_other() {
        let script = format!(
            "#!/bin/sh\nmake lint\n{}",
            block(Path::new("/usr/local/bin/devctx"), "post-merge")
        );
        assert!(has_block(&script));
        assert_eq!(without_block(&script), "#!/bin/sh\nmake lint\n");
    }
}
