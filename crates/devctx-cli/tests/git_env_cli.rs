//! A clean `GIT_*` environment for the server a git hook launches (PLAN-008 B3).
//!
//! Git exports `GIT_DIR`, `GIT_INDEX_FILE`… to its hooks. The managed hook runs
//! `devctx index`, which auto-spawns a long-lived `serve`; if that server kept
//! those variables, every `git` it ran would fail the moment the committing
//! worktree was deleted. These tests run the real binary with poisoned `GIT_*`
//! variables and look at what the spawned server actually received.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Serialises tests that make a server load the embedding model, across test
/// binaries (same file as the one in `mcp_binding.rs`): around 1.5 GB each.
struct EmbedLock(PathBuf);

impl EmbedLock {
    fn acquire() -> Self {
        let path = std::env::temp_dir().join("devctx_it_embedding.lock");
        let start = std::time::Instant::now();
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => return Self(path),
                Err(_) => {
                    let stale = std::fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .map(|t| t.elapsed().unwrap_or_default().as_secs() > 300)
                        .unwrap_or(false);
                    if stale || start.elapsed().as_secs() > 600 {
                        let _ = std::fs::remove_file(&path);
                        continue;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
            }
        }
    }
}

impl Drop for EmbedLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// A scratch directory holding a private `DEVCTX_HOME` and the repositories.
/// Unique per test AND per run: the server's port is derived from the project
/// path, so a reused path can reach a server left over from an earlier run.
struct Tmp {
    root: PathBuf,
    projects: Vec<PathBuf>,
}

impl Tmp {
    fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("devctx_gitenv_it_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Self {
            root,
            projects: Vec::new(),
        }
    }

    fn home(&self) -> PathBuf {
        self.root.join("central")
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        for p in &self.projects {
            let _ = Command::new(env!("CARGO_BIN_EXE_devctx"))
                .env("DEVCTX_HOME", self.home())
                .current_dir(p)
                .args(["serve", "--stop"])
                .output();
        }
        let _ = Command::new(env!("CARGO_BIN_EXE_devctx"))
            .env("DEVCTX_HOME", self.home())
            .args(["serve", "--central", "--stop"])
            .output();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Run `git` with a clean environment and a fixed identity.
fn git(dir: &Path, args: &[&str]) {
    let mut cmd = Command::new("git");
    devctx_core::clean_git_env(&mut cmd);
    let out = cmd
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c"])
        .arg("commit.gpgsign=false")
        .args(args)
        .output()
        .expect("running git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A git repository with one commit on `main`, registered as a project.
fn make_repo(tmp: &mut Tmp, name: &str, file: &str, body: &str) -> PathBuf {
    let root = tmp.root.join(name);
    std::fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    std::fs::write(root.join(file), body).unwrap();
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "init"]);
    let out = devctx(&tmp.home(), &root, &["init", "--yes"]);
    assert!(
        out.status.success(),
        "init: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    tmp.projects.push(root.clone());
    root
}

fn devctx(home: &Path, cwd: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_devctx"))
        .env("DEVCTX_HOME", home)
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("running devctx")
}

fn text(out: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn spawned_server_does_not_inherit_hook_git_variables() {
    let _embed = EmbedLock::acquire();
    let mut tmp = Tmp::new("environ");
    let repo = make_repo(&mut tmp, "repo", "a.rs", "pub fn alpha_marker() {}\n");

    // What git exports to a hook of a worktree that is about to disappear.
    let out = Command::new(env!("CARGO_BIN_EXE_devctx"))
        .env("DEVCTX_HOME", tmp.home())
        .env("GIT_DIR", "/nonexistent/.git/worktrees/x")
        .env("GIT_INDEX_FILE", "/nonexistent/index")
        .env("GIT_PREFIX", "sub/")
        .env("GIT_AUTHOR_NAME", "someone")
        .current_dir(&repo)
        .arg("index")
        .output()
        .expect("running devctx index");
    assert!(out.status.success(), "index: {}", text(&out));

    // The server this `index` spawned: its pid is advertised next to the DB.
    let info = std::fs::read_to_string(repo.join(".devctx/state/serve.json"))
        .expect("`devctx index` should have spawned a server");
    let pid = serde_json::from_str::<serde_json::Value>(&info).unwrap()["pid"]
        .as_u64()
        .expect("serve.json carries the pid");

    let environ = std::fs::read(format!("/proc/{pid}/environ")).expect("server environ");
    let names: Vec<String> = environ
        .split(|b| *b == 0)
        .filter_map(|kv| {
            String::from_utf8_lossy(kv)
                .split('=')
                .next()
                .map(String::from)
        })
        .collect();
    for var in ["GIT_DIR", "GIT_INDEX_FILE", "GIT_PREFIX", "GIT_AUTHOR_NAME"] {
        assert!(!names.iter().any(|n| n == var), "server inherited {var}");
    }

    // And the first query against it works: git inside the server is healthy.
    let status = devctx(&tmp.home(), &repo, &["status"]);
    assert!(status.status.success(), "status: {}", text(&status));
}

/// PLAN-008 Q-2: a commit from a linked worktree runs the hook there; with a
/// clean environment `devctx index` resolves to the main worktree's project and
/// indexes the main worktree's `HEAD` — the linked worktree's own commit is
/// indexed once it reaches a tracked branch. Accepted and documented, not a bug.
#[test]
fn index_from_a_linked_worktree_indexes_the_main_worktree_head() {
    let _embed = EmbedLock::acquire();
    let mut tmp = Tmp::new("worktree");
    let repo = make_repo(&mut tmp, "repo", "a.rs", "pub fn alpha_marker() {}\n");

    let wt = tmp.root.join("wt");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            wt.to_str().unwrap(),
            "-b",
            "feature",
        ],
    );
    std::fs::write(wt.join("b.rs"), "pub fn beta_marker() {}\n").unwrap();
    git(&wt, &["add", "."]);
    git(&wt, &["commit", "-q", "-m", "on the worktree"]);

    let out = devctx(&tmp.home(), &wt, &["index"]);
    assert!(out.status.success(), "index from worktree: {}", text(&out));

    let alpha = devctx(
        &tmp.home(),
        &repo,
        &["search", "alpha_marker", "--format", "json"],
    );
    let beta = devctx(
        &tmp.home(),
        &repo,
        &["search", "beta_marker", "--format", "json"],
    );
    assert!(
        text(&alpha).contains("a.rs"),
        "main HEAD not indexed: {}",
        text(&alpha)
    );
    assert!(
        !text(&beta).contains("b.rs"),
        "the worktree's own commit must not be indexed from `main`'s HEAD: {}",
        text(&beta)
    );
}
