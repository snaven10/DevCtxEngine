//! Watch the work tree and re-index saved files.
//!
//! This covers the one window the post-commit hook cannot: work you have
//! written but not committed. It is only possible because the pipeline accepts
//! an explicit path list — a save moves no commit, so the commit diff a normal
//! incremental run consults would be empty.
//!
//! Everything is routed to the project's server, which owns the database and
//! keeps the model warm. The watcher itself holds no state beyond the pending
//! set.

use std::collections::HashSet;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use devctx_core::config::ProjectConfig;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use notify::{EventKind, RecursiveMode, Watcher};

use crate::remote;

/// Directories never worth watching, whatever `.gitignore` says.
const ALWAYS_SKIP: &[&str] = &[
    ".git",
    ".devctx",
    "node_modules",
    "target",
    "vendor",
    "dist",
    "build",
    "__pycache__",
    ".venv",
    ".next",
    ".gradle",
];

/// Watch `root` until interrupted, re-indexing saved files as they settle.
///
/// `debounce` matters more than it looks: editors save in bursts — format on
/// save, then the write, then a temp-file rename — and a build touches hundreds
/// of files at once. Coalescing avoids re-embedding the same file three times a
/// second.
pub fn run(cfg: &ProjectConfig, root: &Path, debounce: Duration) -> Result<()> {
    let ignore = build_ignore(root);
    let (tx, rx) = mpsc::channel();

    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res {
            let _ = tx.send(event);
        }
    })
    .context("creating the file watcher")?;

    watcher
        .watch(root, RecursiveMode::Recursive)
        .map_err(|e| watch_error(root, e))?;

    println!("Watching {} (Ctrl-C to stop)", root.display());
    println!("  Debounce: {debounce:?}");
    println!("  Saved files are re-indexed; committed work is the hook's job.");

    let mut pending: HashSet<String> = HashSet::new();
    let mut due: Option<Instant> = None;

    loop {
        // Wake often enough to notice the debounce expiring, even when the
        // filesystem has gone quiet and no event will arrive to wake us.
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(event) => {
                if !is_content_change(&event.kind) {
                    continue;
                }
                for path in event.paths {
                    if let Some(rel) = relevant(root, &path, &ignore) {
                        pending.insert(rel);
                    }
                }
                if !pending.is_empty() {
                    due = Some(Instant::now() + debounce);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }

        if due.is_some_and(|t| Instant::now() >= t) && !pending.is_empty() {
            let batch: Vec<String> = pending.drain().collect();
            due = None;
            report(cfg, &batch);
        }
    }
    Ok(())
}

/// Index a batch, printing a one-line summary. Never fatal: a watcher that dies
/// on a transient failure is worse than one that logs and carries on.
fn report(cfg: &ProjectConfig, paths: &[String]) {
    let n = paths.len();
    match index_paths(cfg, paths) {
        Ok(indexed) => println!("· {n} file(s) changed → {indexed} re-indexed"),
        Err(e) => eprintln!("· indexing {n} file(s) failed: {e}"),
    }
}

/// Send the batch to the project's server, which owns the database.
fn index_paths(cfg: &ProjectConfig, paths: &[String]) -> Result<usize> {
    let Some(r) = remote::ensure(cfg) else {
        anyhow::bail!("no project server and one could not be started");
    };
    let raw = r.index_paths(paths)?;
    let v: serde_json::Value = serde_json::from_str(&raw).context("parsing the index result")?;
    Ok(v["files_indexed"].as_u64().unwrap_or(0) as usize)
}

/// Whether an event actually changed file content.
fn is_content_change(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    )
}

/// The repo-relative path to index, or `None` if this file is not our business.
fn relevant(root: &Path, path: &Path, ignore: &Gitignore) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let rel_str = rel.to_str()?;
    if rel_str.is_empty() {
        return None;
    }

    // A single skipped component is enough: `target/debug/x` is no more
    // interesting than `target`.
    if rel
        .components()
        .any(|c| ALWAYS_SKIP.contains(&c.as_os_str().to_str().unwrap_or("")))
    {
        return None;
    }
    if is_editor_noise(path) {
        return None;
    }
    // Directories are watched, not indexed; a deleted path cannot be stat'd, so
    // treat "gone" as a file (the pipeline turns it into a deletion).
    if path.is_dir() {
        return None;
    }
    // `matched` alone only tests the path itself, so a directory rule like
    // `secrets/` would let `secrets/key.txt` through — the parents have to be
    // consulted too.
    if ignore.matched_path_or_any_parents(rel, false).is_ignore() {
        return None;
    }
    Some(rel_str.to_string())
}

/// Temporary files editors leave behind while saving.
fn is_editor_noise(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return true;
    };
    name.ends_with('~')
        || name.ends_with(".swp")
        || name.ends_with(".swx")
        || name.ends_with(".tmp")
        || name.starts_with(".#")
        || name.starts_with('#')
        // JetBrains and friends write `foo.rs___jb_tmp___`
        || name.contains("___jb_")
}

/// Load the repository's ignore rules, so a build does not trigger a reindex.
fn build_ignore(root: &Path) -> Gitignore {
    let mut b = GitignoreBuilder::new(root);
    let _ = b.add(root.join(".gitignore"));
    let _ = b.add(root.join(".devctx/.gitignore"));
    b.build().unwrap_or_else(|_| Gitignore::empty())
}

/// Turn a watch-registration failure into advice rather than an error code.
fn watch_error(root: &Path, e: notify::Error) -> anyhow::Error {
    // On Linux each directory costs an inotify watch, and the per-user cap
    // (often 8192) is reachable in a large repository. That failure is
    // otherwise very hard to read.
    let hint = if e.to_string().contains("No space left") || e.to_string().contains("limit") {
        "\nThe kernel's inotify watch limit is exhausted. Raise it with:\n  \
         sudo sysctl fs.inotify.max_user_watches=524288"
    } else {
        ""
    };
    anyhow::anyhow!("watching {}: {e}{hint}", root.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ignore_with(root: &Path, rules: &str) -> Gitignore {
        let mut b = GitignoreBuilder::new(root);
        for line in rules.lines() {
            let _ = b.add_line(None, line);
        }
        b.build().unwrap()
    }

    #[test]
    fn build_output_and_vcs_directories_are_skipped() {
        let root = Path::new("/repo");
        let ig = Gitignore::empty();
        for skipped in [
            "/repo/target/debug/app",
            "/repo/.git/index",
            "/repo/node_modules/pkg/index.js",
            "/repo/.devctx/state/index.duckdb",
        ] {
            assert!(
                relevant(root, Path::new(skipped), &ig).is_none(),
                "{skipped} should be skipped"
            );
        }
    }

    #[test]
    fn editor_temporaries_are_ignored() {
        for noisy in [
            "/repo/src/lib.rs~",
            "/repo/src/.lib.rs.swp",
            "/repo/src/lib.rs.tmp",
            "/repo/src/.#lib.rs",
            "/repo/src/lib.rs___jb_tmp___",
        ] {
            assert!(is_editor_noise(Path::new(noisy)), "{noisy} is editor noise");
        }
        assert!(!is_editor_noise(Path::new("/repo/src/lib.rs")));
    }

    #[test]
    fn gitignored_files_do_not_trigger_a_reindex() {
        let root = Path::new("/repo");
        let ig = ignore_with(root, "*.log\nsecrets/\n");
        assert!(relevant(root, Path::new("/repo/run.log"), &ig).is_none());
        assert!(
            relevant(root, Path::new("/repo/secrets/key.txt"), &ig).is_none(),
            "a directory rule must cover what is inside it"
        );
        assert_eq!(
            relevant(root, Path::new("/repo/src/lib.rs"), &ig),
            Some("src/lib.rs".to_string())
        );
    }

    #[test]
    fn only_content_changes_count() {
        use notify::event::{AccessKind, CreateKind, ModifyKind, RemoveKind};
        assert!(is_content_change(&EventKind::Modify(ModifyKind::Any)));
        assert!(is_content_change(&EventKind::Create(CreateKind::File)));
        assert!(is_content_change(&EventKind::Remove(RemoveKind::File)));
        assert!(
            !is_content_change(&EventKind::Access(AccessKind::Read)),
            "reading a file must not trigger a reindex"
        );
    }

    #[test]
    fn paths_outside_the_repository_are_rejected() {
        let ig = Gitignore::empty();
        assert!(relevant(Path::new("/repo"), Path::new("/elsewhere/x.rs"), &ig).is_none());
        assert!(relevant(Path::new("/repo"), Path::new("/repo"), &ig).is_none());
    }
}
