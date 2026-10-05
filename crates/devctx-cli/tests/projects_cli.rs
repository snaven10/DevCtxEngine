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

mod common;

/// A scratch directory that cleans up after itself.
struct Tmp(PathBuf);

impl Tmp {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("devctx_cli_it_{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        common::share_models(&dir.join("central"));
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
        // Stop any daemon this test auto-spawned before the directory (and its
        // database) disappears underneath it, so tests never leak processes.
        let _ = Command::new(env!("CARGO_BIN_EXE_devctx"))
            .env("DEVCTX_HOME", self.home())
            .args(["serve", "--central", "--stop"])
            .output();
        common::reap_servers_under(&self.0);
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Run `devctx` against `home` with the store opened directly.
///
/// Auto-spawn is off so these exercise the no-daemon path deterministically;
/// [`devctx_routed`] covers the daemon.
fn devctx(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_devctx"))
        .env("DEVCTX_HOME", home)
        .env("DEVCTX_NO_AUTOSERVE", "1")
        .args(args)
        .output()
        .expect("running devctx")
}

/// Run `devctx` with auto-spawn enabled, so the call routes through the daemon.
fn devctx_routed(home: &Path, args: &[&str]) -> Output {
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

/// The reason the daemon exists: DuckDB permits one writing process per file, so
/// without it concurrent registry commands knock each other over. With it, they
/// queue behind a single owner.
#[test]
fn the_daemon_serializes_concurrent_writers() {
    let tmp = Tmp::new("concurrent");
    let home = tmp.home();

    // The first routed call auto-spawns the daemon and advertises it.
    let out = devctx_routed(&home, &["projects", "list"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        home.join("serve.json").is_file(),
        "a routed call should have spawned and advertised a daemon"
    );

    // Four writers and a reader, all at once.
    let repos: Vec<PathBuf> = ["w1", "w2", "w3", "w4"]
        .iter()
        .map(|n| tmp.repo(n))
        .collect();
    let handles: Vec<_> = repos
        .into_iter()
        .map(|repo| {
            let home = home.clone();
            std::thread::spawn(move || {
                let out = devctx_routed(
                    &home,
                    &["projects", "add", repo.to_str().unwrap(), "--init"],
                );
                (
                    out.status.success(),
                    String::from_utf8_lossy(&out.stderr).into_owned(),
                )
            })
        })
        .chain(std::iter::once({
            let home = home.clone();
            std::thread::spawn(move || {
                let out = devctx_routed(&home, &["projects", "list"]);
                (
                    out.status.success(),
                    String::from_utf8_lossy(&out.stderr).into_owned(),
                )
            })
        }))
        .collect();

    for h in handles {
        let (ok, err) = h.join().expect("thread panicked");
        assert!(ok, "a concurrent command failed: {err}");
    }

    let json = devctx_routed(&home, &["projects", "list", "--format", "json"]);
    let parsed: serde_json::Value = serde_json::from_slice(&json.stdout).expect("valid JSON");
    assert_eq!(
        parsed.as_array().unwrap().len(),
        4,
        "every concurrent write should have landed"
    );
}

/// Routed and direct reads must agree — the daemon is a transport, not a
/// different source of truth.
#[test]
fn routed_and_direct_reads_agree() {
    let tmp = Tmp::new("agreement");
    let home = tmp.home();
    ok(
        &home,
        &[
            "projects",
            "add",
            tmp.repo("zeta").to_str().unwrap(),
            "--init",
        ],
    );

    let direct = ok(&home, &["projects", "show", "zeta"]);
    let routed = devctx_routed(&home, &["projects", "show", "zeta"]);
    assert!(routed.status.success());
    assert_eq!(direct, String::from_utf8_lossy(&routed.stdout));
}

/// A second daemon for the same home is refused rather than racing the first.
#[test]
fn a_second_daemon_is_refused() {
    let tmp = Tmp::new("singleton");
    let home = tmp.home();
    devctx_routed(&home, &["projects", "list"]); // spawns one

    let out = devctx_routed(&home, &["serve", "--central"]);
    assert!(!out.status.success(), "a second daemon must not start");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("already running"), "got: {err}");
}

/// The end-to-end payoff of the central store: a lesson learned in one
/// repository is recalled from another, project-local notes stay private, and
/// the same lesson contributed twice converges on one memory.
///
/// Ignored by default because it loads a real embedding model — the engine
/// semantics themselves are covered by unit tests in `devctx-memory`.
///
/// Re-checked while hardening `memories_of` against unexpected payload
/// shapes: this test drives `remember`/`recall` end to end through a real
/// server, and both embed unconditionally (see `RememberRequest` /
/// `RecallQuery` in `devctx-memory`) — there is no path through them that
/// skips the model, so it cannot be un-ignored without changing what it tests.
#[test]
#[ignore = "loads an embedding model (downloads it on a cold cache)"]
fn global_memories_cross_projects_while_local_ones_stay_put() {
    let tmp = Tmp::new("globalmem");
    let home = tmp.home();
    let one = tmp.repo("one");
    let two = tmp.repo("two");
    ok(&home, &["projects", "add", one.to_str().unwrap(), "--init"]);
    ok(&home, &["projects", "add", two.to_str().unwrap(), "--init"]);

    let in_repo = |repo: &PathBuf, args: &[&str]| -> String {
        let out = Command::new(env!("CARGO_BIN_EXE_devctx"))
            .env("DEVCTX_HOME", &home)
            .current_dir(repo)
            .args(args)
            .output()
            .expect("running devctx");
        assert!(
            out.status.success(),
            "`devctx {}` failed:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    };

    let lesson = "always verify the HMAC signature on an incoming webhook";
    let created = in_repo(
        &one,
        &[
            "remember",
            lesson,
            "--title",
            "Webhook signatures",
            "--scope",
            "global",
        ],
    );
    assert!(created.contains("created"), "got: {created}");
    in_repo(
        &one,
        &[
            "remember",
            "refunds live in src/refunds.rs",
            "--scope",
            "local",
        ],
    );

    // The other repository sees the global lesson…
    let recalled = in_repo(
        &two,
        &["recall", "how do I validate a webhook", "--scope", "global"],
    );
    assert!(recalled.contains("Webhook signatures"), "got: {recalled}");

    // …but never the first one's private note.
    let all = in_repo(&two, &["recall", "where do refunds live", "--scope", "all"]);
    assert!(
        !all.contains("src/refunds.rs"),
        "local memory leaked: {all}"
    );

    // Learning the same thing again converges instead of duplicating.
    let again = in_repo(
        &two,
        &[
            "remember",
            lesson,
            "--title",
            "Webhook signatures",
            "--scope",
            "global",
        ],
    );
    assert!(again.contains("duplicate"), "got: {again}");
}

/// Routed and direct reads have to agree in SHAPE, not only in content.
///
/// This is the class of defect that went unnoticed longest. `local_recall` read
/// the server's answer with `as_array()`; the endpoint returns
/// `{"memories": [...]}`, so it was always `None` and every recall made while a
/// server was running reported nothing — one project held sixteen memories and
/// answered "No memories." Nothing failed, nothing logged, and "nothing recorded
/// about that" is a perfectly ordinary answer.
///
/// Both paths are exercised here without an embedding model, so this stays cheap
/// enough to run every time. What it cannot cover is the `recall` path itself,
/// which needs a model loaded; that gap is why the bug survived, and it is still
/// open.
#[test]
fn routed_and_direct_agree_on_shape_not_just_content() {
    let tmp = Tmp::new("shape");
    let home = tmp.home();
    ok(
        &home,
        &[
            "projects",
            "add",
            tmp.repo("shapes").to_str().unwrap(),
            "--init",
        ],
    );

    // Each of these crosses the CLI/HTTP boundary, and each parses what comes
    // back. A reader that assumes the wrong container returns empty rather than
    // failing, which is what makes the mistake invisible.
    for args in [
        vec!["projects", "list", "--format", "json"],
        vec!["projects", "show", "shapes"],
    ] {
        let direct = ok(&home, &args);
        let routed = devctx_routed(&home, &args);
        assert!(
            routed.status.success(),
            "`devctx {}` failed when routed:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&routed.stderr)
        );
        let routed = String::from_utf8_lossy(&routed.stdout).into_owned();
        assert_eq!(
            direct,
            routed,
            "`devctx {}` answers differently through the daemon",
            args.join(" ")
        );
        assert!(
            !routed.trim().is_empty(),
            "`devctx {}` came back empty through the daemon — the shape of the \
             answer probably changed and the reader silently got nothing",
            args.join(" ")
        );
    }
}

/// TASK-017 item 5: a child that never exits fails the helper after its
/// deadline, with what it printed, instead of hanging the caller.
#[test]
fn a_hung_child_fails_the_wait_instead_of_hanging_the_suite() {
    // The trailing `:` keeps `sleep` a forked child under every `sh`: bash
    // exec'd a final `sleep` in place of the shell and dash does not, so the
    // orphaned sleep holding the pipe after the kill happened only on CI's
    // dash and the output printed before it was lost there.
    let child = Command::new("sh")
        .args(["-c", "echo started; sleep 60; :"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let t0 = std::time::Instant::now();
    let err = common::wait_with_timeout(child, std::time::Duration::from_secs(1))
        .expect_err("a hung child must time out");
    assert!(err.contains("started"), "{err}");
    assert!(t0.elapsed() < std::time::Duration::from_secs(10));
    let done = Command::new("sh")
        .args(["-c", "echo ok"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let out = common::wait_with_timeout(done, std::time::Duration::from_secs(10)).unwrap();
    assert!(out.status.success() && out.stdout == b"ok\n");
}

/// TASK-017 fixup M5: a grandchild that inherited the pipes and outlives the
/// child must not make `wait_with_timeout` hang past its own deadline.
#[test]
fn wait_with_timeout_does_not_wait_for_a_grandchild_holding_the_pipes() {
    let child = Command::new("sh")
        // The grandchild keeps stdout/stderr open for 30 s; the child hangs.
        .args(["-c", "sleep 30 & sleep 30"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let t0 = std::time::Instant::now();
    let err = common::wait_with_timeout(child, std::time::Duration::from_secs(1))
        .expect_err("a hung child must time out");
    assert!(err.contains("still running"), "{err}");
    assert!(
        t0.elapsed() < std::time::Duration::from_secs(8),
        "hung on the grandchild's pipe: {:?}",
        t0.elapsed()
    );
}

/// m-4 / TASK-017 fixup M3: under cargo the workspace's `.cargo/config.toml`
/// puts `DEVCTX_MODEL_CACHE` in the environment of every test process (the
/// `#[ignore]`d ones in other crates call `model_cache_dir()` directly, with no
/// helper to go through). Checked on the effective environment, not on the text
/// of the file, and never the user's real models.
#[test]
fn the_test_model_cache_is_pinned_for_the_whole_workspace() {
    let effective = std::env::var_os("DEVCTX_MODEL_CACHE")
        .filter(|v| !v.is_empty())
        .expect("cargo must set DEVCTX_MODEL_CACHE for tests (.cargo/config.toml [env])");
    let effective = PathBuf::from(effective);
    // An explicit value (CI, a developer) wins; otherwise it is the workspace's
    // directory outside `target/`, absolute (`relative = true` resolves it).
    if !effective.starts_with(std::env::temp_dir()) && !effective.starts_with("/var/tmp") {
        assert!(
            effective.ends_with(".devctx-test-models") || std::env::var_os("CI").is_some(),
            "{effective:?}"
        );
    }
    assert!(
        !effective.components().any(|c| c.as_os_str() == "target"),
        "{effective:?} is inside target/: `cargo clean` would delete the models"
    );
    assert_eq!(common::shared_model_cache(), effective);
}

/// Test hygiene: a run without `DEVCTX_MODEL_CACHE` must never resolve to the
/// user's real data directory, whichever way HOME / XDG_DATA_HOME point.
#[test]
fn without_an_explicit_cache_the_tests_never_use_the_users_real_models() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let got = common::resolve_model_cache(None, manifest);
    assert!(got.ends_with(".devctx-test-models"), "{got:?}");
    for real in [
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share/devctx")),
        std::env::var_os("XDG_DATA_HOME").map(|x| PathBuf::from(x).join("devctx")),
    ]
    .into_iter()
    .flatten()
    {
        assert!(!got.starts_with(&real), "{got:?} is under {real:?}");
    }
    // An explicit cache still wins, and an empty one counts as unset.
    assert_eq!(
        common::resolve_model_cache(Some("/x/cache".into()), manifest),
        Path::new("/x/cache")
    );
    assert!(common::resolve_model_cache(Some("".into()), manifest).ends_with(".devctx-test-models"));
}

/// N4 (field): `init --model ml-granite` on a machine without the model's
/// files used to fail with a command that does not exist (`models download`).
/// Without a terminal and without `--yes` nothing is downloaded behind the
/// caller's back: the error carries the exact command to run.
#[test]
fn init_with_a_model_that_is_not_on_disk_names_the_download_command() {
    let tmp = Tmp::new("init_model_missing");
    let repo = tmp.repo("alpha");
    let out = Command::new(env!("CARGO_BIN_EXE_devctx"))
        .env("DEVCTX_HOME", tmp.home())
        .env("DEVCTX_NO_AUTOSERVE", "1")
        // An empty cache of its own: whatever the shared one holds is not the point.
        .env("DEVCTX_MODEL_CACHE", tmp.0.join("empty-cache"))
        .current_dir(&repo)
        .args(["init", "--model", "ml-granite"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("devctx models --download ml-granite"), "{err}");
    assert!(!repo.join(".devctx").join("config.yaml").exists());
}

/// TASK-017 item 6: a machine default that names a files-needing model with
/// no `model_dir` (what an earlier interactive `init` could leave behind),
/// and the machine is offline. `init` used to inherit it and write a config
/// with an empty `model_dir` that only failed at the first index; it now
/// refuses and names the download command.
#[test]
fn init_never_writes_an_empty_model_dir_for_a_model_that_needs_files() {
    let tmp = Tmp::new("init_empty_dir");
    let repo = tmp.repo("alpha");
    std::fs::create_dir_all(tmp.home()).unwrap();
    std::fs::write(
        tmp.home().join("config.yaml"),
        "defaults:\n  embeddings:\n    provider: local\n    model: ml-granite\n    \
         model_dir: \"\"\n    offline: true\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_devctx"))
        .env("DEVCTX_HOME", tmp.home())
        .env("DEVCTX_NO_AUTOSERVE", "1")
        .env("DEVCTX_MODEL_CACHE", tmp.0.join("empty-cache"))
        .current_dir(&repo)
        .args(["init", "--yes"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{err}");
    assert!(err.contains("devctx models --download ml-granite"), "{err}");
    assert!(!repo.join(".devctx").join("config.yaml").exists());
}

/// m-5: `--yes` is "defaults without asking": with a model whose files are
/// missing it does not download (CI would pull hundreds of MB), it names the
/// command, and `--download` is the explicit way to opt in.
#[test]
fn init_yes_does_not_download_and_names_the_command() {
    let tmp = Tmp::new("init_yes_nodl");
    let repo = tmp.repo("alpha");
    let out = Command::new(env!("CARGO_BIN_EXE_devctx"))
        .env("DEVCTX_HOME", tmp.home())
        .env("DEVCTX_NO_AUTOSERVE", "1")
        .env("DEVCTX_MODEL_CACHE", tmp.0.join("empty-cache"))
        .current_dir(&repo)
        .args(["init", "--yes", "--model", "ml-granite"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("devctx models --download ml-granite"), "{err}");
    assert!(!err.contains("fetching them now"), "{err}");
    assert!(!tmp.0.join("empty-cache").join("ml-granite").exists());
    assert!(!repo.join(".devctx").join("config.yaml").exists());
}
