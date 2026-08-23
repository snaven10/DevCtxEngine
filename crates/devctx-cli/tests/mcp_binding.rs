//! End-to-end tests for how `devctx mcp` decides which project it is bound to.
//!
//! These drive the real binary from a real directory, because the thing under
//! test *is* the relationship between a working directory and the registry —
//! and that is exactly what a unit test cannot reach. `DEVCTX_HOME` keeps the
//! registry inside a temp directory, so the user's own one is never touched.
//!
//! The server is started with its stdin closed: it comes up, prints how it
//! resolved a binding, fails the MCP handshake against an empty stream and
//! exits. That stderr line is the assertion target.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Tmp(PathBuf);

impl Tmp {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("devctx_bind_it_{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn home(&self) -> PathBuf {
        self.0.join("central")
    }

    fn dir(&self, name: &str) -> PathBuf {
        let p = self.0.join(name);
        std::fs::create_dir_all(&p).unwrap();
        p
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = Command::new(env!("CARGO_BIN_EXE_devctx"))
            .env("DEVCTX_HOME", self.home())
            .args(["serve", "--central", "--stop"])
            .output();
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn devctx(home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_devctx"))
        .env("DEVCTX_HOME", home)
        .env("DEVCTX_NO_AUTOSERVE", "1")
        .args(args)
        .output()
        .expect("running devctx")
}

/// Register a project under `parent/name`, optionally declaring a group.
fn make_project(home: &Path, parent: &Path, name: &str, group: Option<&str>) -> PathBuf {
    let root = parent.join(name);
    std::fs::create_dir_all(&root).unwrap();
    let out = devctx(
        home,
        &["projects", "add", root.to_str().unwrap(), "--init"],
    );
    assert!(
        out.status.success(),
        "registering {name}:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    if let Some(g) = group {
        let cfg_path = root.join(".devctx/config.yaml");
        let cfg = std::fs::read_to_string(&cfg_path).unwrap();
        // `group:` is written by `init` as an empty string; fill it in place so
        // the rest of the config keeps whatever defaults it was given.
        let filled = if cfg.contains("group:") {
            cfg.lines()
                .map(|l| {
                    if l.trim_start().starts_with("group:") {
                        format!("  group: {g}")
                    } else {
                        l.to_string()
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            cfg.replace("project:", &format!("project:\n  group: {g}"))
        };
        std::fs::write(&cfg_path, filled).unwrap();
    }
    root
}

/// Start `devctx mcp` in `cwd` and return what it said about its binding.
fn start_mcp_in(home: &Path, cwd: &Path, extra: &[&str]) -> String {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_devctx"));
    // Autoserve stays ON here: the descent reads the registry through the
    // central daemon, so switching it off would test a server that cannot see
    // any project — a different scenario, with its own test.
    cmd.env("DEVCTX_HOME", home)
        .arg("mcp")
        .args(extra)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = cmd.output().expect("running devctx mcp");
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// The bug this whole change exists for: a workspace root used to bind nothing,
/// and `remember` then failed outright rather than degrading.
#[test]
fn workspace_root_binds_the_group() {
    let tmp = Tmp::new("group");
    let home = tmp.home();
    let ws = tmp.dir("workspace");
    make_project(&home, &ws, "api", Some("ACME"));
    make_project(&home, &ws, "web", Some("ACME"));
    make_project(&home, &ws, "worker", Some("ACME"));

    let err = start_mcp_in(&home, &ws, &[]);
    assert!(
        err.contains("Bound to group ACME"),
        "expected a group binding, got:\n{err}"
    );
    assert!(err.contains('3'), "expected the member count, got:\n{err}");
}

#[test]
fn workspace_root_with_one_project_binds_that_project() {
    let tmp = Tmp::new("single");
    let home = tmp.home();
    let ws = tmp.dir("workspace");
    make_project(&home, &ws, "only", None);

    let err = start_mcp_in(&home, &ws, &[]);
    assert!(
        err.contains("Bound to project only"),
        "expected a single-project binding, got:\n{err}"
    );
}

/// Members that disagree about their group must not be guessed between.
#[test]
fn mixed_groups_stay_unbound_and_name_the_candidates() {
    let tmp = Tmp::new("mixed");
    let home = tmp.home();
    let ws = tmp.dir("workspace");
    make_project(&home, &ws, "alpha", Some("ONE"));
    make_project(&home, &ws, "beta", Some("TWO"));

    let err = start_mcp_in(&home, &ws, &[]);
    assert!(
        !err.contains("Bound to group"),
        "must not invent a group binding:\n{err}"
    );
    assert!(
        err.contains("alpha") && err.contains("beta"),
        "the candidates should be named:\n{err}"
    );
    assert!(
        err.contains("do not share a group"),
        "the reason should be stated:\n{err}"
    );
}

/// The walk upwards must keep working exactly as before.
#[test]
fn inside_a_repository_binds_that_repository() {
    let tmp = Tmp::new("inside");
    let home = tmp.home();
    let ws = tmp.dir("workspace");
    let api = make_project(&home, &ws, "api", Some("ACME"));
    make_project(&home, &ws, "web", Some("ACME"));

    let err = start_mcp_in(&home, &api, &[]);
    assert!(
        !err.contains("Bound to group"),
        "a directory inside a repository is not a workspace root:\n{err}"
    );
}

/// An explicit `--project` outranks every inference.
#[test]
fn explicit_project_wins_over_the_descent() {
    let tmp = Tmp::new("explicit");
    let home = tmp.home();
    let ws = tmp.dir("workspace");
    let api = make_project(&home, &ws, "api", Some("ACME"));
    make_project(&home, &ws, "web", Some("ACME"));

    let err = start_mcp_in(&home, &ws, &["--project", api.to_str().unwrap()]);
    assert!(
        !err.contains("Bound to group"),
        "--project must not be overridden by the descent:\n{err}"
    );
}

/// `/x/ws` is a string prefix of `/x/ws-other` while being no ancestor of it.
/// Comparing rendered paths instead of components would bind the wrong siblings.
#[test]
fn a_name_prefix_is_not_a_parent_directory() {
    let tmp = Tmp::new("prefix");
    let home = tmp.home();
    let ws = tmp.dir("ws");
    let other = tmp.dir("ws-other");
    make_project(&home, &ws, "mine", Some("MINE"));
    make_project(&home, &other, "theirs", Some("THEIRS"));

    let err = start_mcp_in(&home, &ws, &[]);
    assert!(
        err.contains("Bound to project mine"),
        "only the project under ws should have been found:\n{err}"
    );
    assert!(
        !err.contains("theirs"),
        "a sibling directory sharing a name prefix leaked in:\n{err}"
    );
}

/// Nothing above and nothing below: the old message, unchanged.
#[test]
fn an_empty_directory_starts_unbound() {
    let tmp = Tmp::new("empty");
    let home = tmp.home();
    let empty = tmp.dir("nothing-here");

    let err = start_mcp_in(&home, &empty, &[]);
    assert!(
        err.contains("no project bound") || err.contains("not bound to a project"),
        "expected the unbound message, got:\n{err}"
    );
}
