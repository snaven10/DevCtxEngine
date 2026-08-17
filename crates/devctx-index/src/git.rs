//! Thin git wrapper: repo state and diff detection (shells out to `git`).

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{IndexError, Result};

/// A file change between two commits (or an initial full listing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// File added.
    Added(String),
    /// File modified.
    Modified(String),
    /// File deleted.
    Deleted(String),
    /// File renamed (from → to).
    Renamed {
        /// Old path.
        from: String,
        /// New path.
        to: String,
    },
}

/// Current repo HEAD state.
#[derive(Debug, Clone)]
pub struct GitState {
    /// HEAD commit (empty if the repo has no commits).
    pub commit: String,
    /// Current branch (or `HEAD` when detached).
    pub branch: String,
}

/// A git repository rooted at its top-level directory.
pub struct GitRepo {
    root: PathBuf,
}

impl GitRepo {
    /// Open the repository containing `path` (resolves the work-tree root).
    pub fn open(path: &Path) -> Result<Self> {
        let top = run(path, &["rev-parse", "--show-toplevel"])?;
        Ok(Self {
            root: PathBuf::from(top.trim()),
        })
    }

    /// The work-tree root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The short repo name (basename of the root).
    pub fn short_name(&self) -> String {
        self.root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("repo")
            .to_string()
    }

    /// Read the current HEAD commit and branch.
    pub fn state(&self) -> GitState {
        let commit = run(&self.root, &["rev-parse", "HEAD"])
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let branch = run(&self.root, &["rev-parse", "--abbrev-ref", "HEAD"])
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| "HEAD".to_string());
        GitState { commit, branch }
    }

    /// List what to index: everything in the work tree when `from` is `None`
    /// (initial / full index), otherwise the diff since that commit.
    ///
    /// Both include files git is not tracking yet, as long as it is not ignoring
    /// them. The index mirrors the **work tree**, not the last commit: a file you
    /// have written but not `git add`ed is exactly the code you are most likely
    /// to ask about, and a full reindex that dropped it would silently undo what
    /// the watcher had picked up.
    pub fn changes(&self, from: Option<&str>) -> Result<Vec<Change>> {
        let mut changes = match from {
            None => {
                let out = run(&self.root, &["ls-files"])?;
                out.lines()
                    .filter(|l| !l.is_empty())
                    .map(|l| Change::Added(l.to_string()))
                    .collect()
            }
            Some(commit) => {
                let out = run(&self.root, &["diff", "--name-status", "-M", commit, "HEAD"])?;
                parse_name_status(&out)
            }
        };

        // `--exclude-standard` honours .gitignore, .git/info/exclude and the
        // user's global excludes, so build output stays out.
        let untracked = run(&self.root, &["ls-files", "--others", "--exclude-standard"])?;
        let already: std::collections::HashSet<&str> = changes.iter().map(change_path).collect();
        let new: Vec<Change> = untracked
            .lines()
            .filter(|l| !l.is_empty() && !already.contains(l))
            .map(|l| Change::Added(l.to_string()))
            .collect();
        changes.extend(new);
        Ok(changes)
    }

    /// True if `commit` exists in the repo.
    pub fn commit_exists(&self, commit: &str) -> bool {
        if commit.is_empty() {
            return false;
        }
        run(
            &self.root,
            &["cat-file", "-e", &format!("{commit}^{{commit}}")],
        )
        .is_ok()
    }

    /// Read a file's content from the work tree.
    pub fn read_file(&self, rel: &str) -> Result<String> {
        Ok(std::fs::read_to_string(self.root.join(rel))?)
    }

    /// Whether `branch` exists as a local branch.
    ///
    /// Indexing a branch git does not have is an error, not something to
    /// create: `index` reads a repository, and a read command that quietly
    /// makes a branch would be a surprise nobody asked for and a mess to undo.
    pub fn has_branch(&self, branch: &str) -> bool {
        run(
            &self.root,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ],
        )
        .is_ok()
    }

    /// The commit a branch points at.
    pub fn commit_of(&self, branch: &str) -> Option<String> {
        run(&self.root, &["rev-parse", branch])
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// What to index for `branch`, read from git rather than from disk.
    ///
    /// Used when the branch is not the checked-out one — the case a repository
    /// with worktrees is in most of the time. Unlike [`changes`](Self::changes)
    /// this cannot see untracked files, and that is correct rather than a
    /// limitation: a file you have not committed exists only in the worktree
    /// you wrote it in, and does not belong to the branch as anyone else would
    /// find it.
    pub fn changes_at(&self, branch: &str, from: Option<&str>) -> Result<Vec<Change>> {
        Ok(match from {
            None => run(&self.root, &["ls-tree", "-r", "--name-only", branch])?
                .lines()
                .filter(|l| !l.is_empty())
                .map(|l| Change::Added(l.to_string()))
                .collect(),
            Some(commit) => parse_name_status(&run(
                &self.root,
                &["diff", "--name-status", "-M", commit, branch],
            )?),
        })
    }

    /// Read a file as it is on `branch`, without checking it out.
    pub fn read_file_at(&self, branch: &str, rel: &str) -> Result<String> {
        run(&self.root, &["show", &format!("{branch}:{rel}")])
    }
}

/// The path a change refers to (the destination, for a rename).
fn change_path(c: &Change) -> &str {
    match c {
        Change::Added(p) | Change::Modified(p) | Change::Deleted(p) => p,
        Change::Renamed { to, .. } => to,
    }
}

fn parse_name_status(out: &str) -> Vec<Change> {
    let mut changes = Vec::new();
    for line in out.lines().filter(|l| !l.is_empty()) {
        let parts: Vec<&str> = line.split('\t').collect();
        let Some(status) = parts.first() else {
            continue;
        };
        let code = status.chars().next().unwrap_or(' ');
        match code {
            'A' => push_path(&mut changes, &parts, 1, Change::Added),
            'M' | 'T' => push_path(&mut changes, &parts, 1, Change::Modified),
            'D' => push_path(&mut changes, &parts, 1, Change::Deleted),
            'C' => push_path(&mut changes, &parts, 2, Change::Added), // copy: new path
            'R' => {
                if let (Some(from), Some(to)) = (parts.get(1), parts.get(2)) {
                    changes.push(Change::Renamed {
                        from: from.to_string(),
                        to: to.to_string(),
                    });
                }
            }
            _ => {}
        }
    }
    changes
}

fn push_path(changes: &mut Vec<Change>, parts: &[&str], idx: usize, make: fn(String) -> Change) {
    if let Some(p) = parts.get(idx) {
        changes.push(make(p.to_string()));
    }
}

fn run(cwd: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git").arg("-C").arg(cwd).args(args).output()?;
    if !out.status.success() {
        return Err(IndexError::Git(
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_name_status_lines() {
        let out = "A\tsrc/new.rs\nM\tsrc/mod.rs\nD\tsrc/old.rs\nR100\tsrc/a.rs\tsrc/b.rs\n";
        let changes = parse_name_status(out);
        assert_eq!(changes.len(), 4);
        assert_eq!(changes[0], Change::Added("src/new.rs".into()));
        assert_eq!(changes[1], Change::Modified("src/mod.rs".into()));
        assert_eq!(changes[2], Change::Deleted("src/old.rs".into()));
        assert_eq!(
            changes[3],
            Change::Renamed {
                from: "src/a.rs".into(),
                to: "src/b.rs".into()
            }
        );
    }
}
