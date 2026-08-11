//! End-to-end tests for `devctx projects` against a scratch central home.
//!
//! These drive the real binary, so every invocation is a separate process
//! against the same central database — which is exactly what makes them worth
//! having: they cover the wiring (CLI parsing, central store, persistence
//! between runs) that no unit test can reach.
//!
//! `DEVCTX_HOME` keeps the whole thing inside a temp directory, so the user's
//! real `~/.local/share/devctx` is never touched.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A scratch directory that cleans up after itself.
struct Tmp(PathBuf);

impl Tmp {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("devctx_cli_it_{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn home(&self) -> PathBuf {
        self.0.join("central")
    }

    fn repo(&self, name: &str) -> PathBuf {
        let p = self.0.join(name);
        std::fs::create_dir_all(&p).unwrap();
        p
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Run `devctx` with the central home pointed at `home`.
fn devctx(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_devctx"))
        .env("DEVCTX_HOME", home)
        .args(args)
        .output()
        .expect("running devctx")
}

fn ok(home: &Path, args: &[&str]) -> String {
    let out = devctx(home, args);
    assert!(
        out.status.success(),
        "`devctx {}` failed:\n{}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn fails(home: &Path, args: &[&str]) -> String {
    let out = devctx(home, args);
    assert!(
        !out.status.success(),
        "`devctx {}` unexpectedly succeeded",
        args.join(" ")
    );
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn add_list_show_and_remove_across_separate_runs() {
    let tmp = Tmp::new("lifecycle");
    let home = tmp.home();
    let repo = tmp.repo("alpha");

    // A bare directory is not a project until asked to initialize one.
    let err = fails(&home, &["projects", "add", repo.to_str().unwrap()]);
    assert!(err.contains("not a DevCtxEngine project"), "got: {err}");

    let out = ok(
        &home,
        &[
            "projects",
            "add",
            repo.to_str().unwrap(),
            "--init",
            "--description",
            "the alpha service",
        ],
    );
    assert!(out.contains("Registered `alpha`"), "got: {out}");
    assert!(
        repo.join(".devctx/config.yaml").is_file(),
        "--init should have written the project config"
    );

    // A *separate* process must see it: the registry really persisted.
    let listed = ok(&home, &["projects", "list"]);
    assert!(listed.contains("alpha"), "got: {listed}");
    assert!(listed.contains("never indexed"), "got: {listed}");

    let shown = ok(&home, &["projects", "show", "alpha"]);
    assert!(shown.contains("the alpha service"), "got: {shown}");
    assert!(shown.contains("minilm-l6"), "got: {shown}");
    assert!(shown.contains("index.duckdb"), "got: {shown}");

    assert!(fails(&home, &["projects", "show", "ghost"]).contains("no registered project"));

    let removed = ok(&home, &["projects", "rm", "alpha"]);
    assert!(removed.contains("Removed `alpha`"), "got: {removed}");
    assert!(ok(&home, &["projects", "list"]).contains("No projects registered"));
    assert!(
        repo.join(".devctx/config.yaml").is_file(),
        "removing from the registry must not touch the repository"
    );
}

#[test]
fn re_adding_the_same_repo_does_not_duplicate_it() {
    let tmp = Tmp::new("dedup");
    let home = tmp.home();
    let repo = tmp.repo("beta");
    let path = repo.to_str().unwrap();

    ok(&home, &["projects", "add", path, "--init"]);
    ok(&home, &["projects", "add", path]);
    ok(&home, &["projects", "add", path, "--tags", "backend,api"]);

    let json = ok(&home, &["projects", "list", "--format", "json"]);
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    let rows = parsed.as_array().expect("an array");
    assert_eq!(rows.len(), 1, "one row per repository, got: {json}");
    assert_eq!(rows[0]["name"], "beta");
    assert_eq!(rows[0]["tags"], "backend,api");
    assert_eq!(rows[0]["embed_dim"], 384);
    assert_eq!(rows[0]["active"], true);
}

#[test]
fn deactivating_hides_a_project_without_losing_it() {
    let tmp = Tmp::new("deactivate");
    let home = tmp.home();
    ok(
        &home,
        &[
            "projects",
            "add",
            tmp.repo("gamma").to_str().unwrap(),
            "--init",
        ],
    );

    ok(&home, &["projects", "rm", "gamma", "--deactivate"]);
    assert!(ok(&home, &["projects", "list"]).contains("No projects registered"));

    let all = ok(&home, &["projects", "list", "--all"]);
    assert!(all.contains("gamma"), "got: {all}");
    assert!(ok(&home, &["projects", "show", "gamma"]).contains("Active:      false"));
}

#[test]
fn two_repos_cannot_share_a_name() {
    let tmp = Tmp::new("collision");
    let home = tmp.home();
    let a = tmp.repo("one");
    std::fs::create_dir_all(a.join("dup")).unwrap();
    let b = tmp.repo("two");
    std::fs::create_dir_all(b.join("dup")).unwrap();

    ok(
        &home,
        &["projects", "add", a.join("dup").to_str().unwrap(), "--init"],
    );
    let err = fails(
        &home,
        &["projects", "add", b.join("dup").to_str().unwrap(), "--init"],
    );
    assert!(err.contains("already taken"), "got: {err}");

    // An explicit name resolves it.
    ok(
        &home,
        &[
            "projects",
            "add",
            b.join("dup").to_str().unwrap(),
            "--init",
            "--name",
            "dup-two",
        ],
    );
    let json = ok(&home, &["projects", "list", "--format", "json"]);
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.as_array().unwrap().len(), 2);
}

#[test]
fn refresh_picks_up_an_edited_project_config() {
    let tmp = Tmp::new("refresh");
    let home = tmp.home();
    let repo = tmp.repo("delta");
    ok(
        &home,
        &["projects", "add", repo.to_str().unwrap(), "--init"],
    );

    // Edit the project config the way a user would, then pull it into the registry.
    let cfg_path = repo.join(".devctx/config.yaml");
    let cfg = std::fs::read_to_string(&cfg_path).unwrap();
    std::fs::write(&cfg_path, cfg.replace("minilm-l6", "bge-base")).unwrap();

    let out = ok(&home, &["projects", "refresh", "delta"]);
    assert!(out.contains("bge-base"), "got: {out}");
    assert!(out.contains("768d"), "got: {out}");

    let shown = ok(&home, &["projects", "show", "delta"]);
    assert!(shown.contains("bge-base (local, 768d)"), "got: {shown}");
}

#[test]
fn init_registers_the_repo_centrally() {
    let tmp = Tmp::new("init");
    let home = tmp.home();
    let repo = tmp.repo("epsilon");

    let out = ok(&home, &["init", repo.to_str().unwrap()]);
    assert!(out.contains("Initialized"), "got: {out}");
    assert!(
        out.contains("Registered in the central store as `epsilon`"),
        "got: {out}"
    );

    assert!(ok(&home, &["projects", "list"]).contains("epsilon"));
}
