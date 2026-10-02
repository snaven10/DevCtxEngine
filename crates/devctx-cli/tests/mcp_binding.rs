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

/// Serialises every test that writes a memory — across test binaries, not just
/// within this one.
///
/// Writing a memory embeds it, which loads the model: around 1.5 GB resident
/// per server. Cargo runs each test file as its own process AND runs the tests
/// inside it in parallel, so a plain `Mutex` only holds back the neighbours in
/// this file while `mcp_tools.rs` embeds alongside it. Several gigabytes and
/// every core land on a machine already busy, the server never answers, and the
/// call surfaces as "timed out reading response" — which reads like a broken
/// daemon rather than a busy one.
///
/// So the lock has to live where every process can see it: a file. Acquired by
/// atomic create, released on drop.
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
                    // A test that panicked while holding it would block the rest
                    // of the run forever, so a lock older than this is assumed
                    // abandoned rather than held.
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

struct Tmp(PathBuf);

impl Tmp {
    fn new(tag: &str) -> Self {
        // The path must be unique per run, not just per test. `auto_addr`
        // derives a server's port from an FNV hash of the project path, so a
        // fixed path means a fixed port — and a server left over from an
        // earlier run answers on it while its database directory has already
        // been deleted. The call then hangs until the client gives up, which
        // reads as flakiness rather than as the collision it is.
        let dir = std::env::temp_dir().join(format!("devctx_bind_it_{tag}_{}", std::process::id()));
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
        // Stop every server this test started, not just the central one.
        //
        // `devctx mcp` auto-spawns a server per project it touches, each with
        // `--idle 900`. Stopping only the central daemon left those running for
        // fifteen minutes apiece: a suite run leaked one per project, they
        // accumulated across runs, and at fifteen of them holding ~3 GB the
        // next run's servers lost the race to answer and the tests failed as
        // "timed out reading response". The leak looked like flakiness.
        if let Ok(entries) = std::fs::read_dir(&self.0) {
            for entry in entries.flatten() {
                let candidate = entry.path();
                if candidate.join(".devctx").is_dir() {
                    let _ = Command::new(env!("CARGO_BIN_EXE_devctx"))
                        .env("DEVCTX_HOME", self.home())
                        .current_dir(&candidate)
                        .args(["serve", "--stop"])
                        .output();
                }
                // Workspaces hold the projects one level further down.
                if let Ok(inner) = std::fs::read_dir(&candidate) {
                    for sub in inner.flatten() {
                        if sub.path().join(".devctx").is_dir() {
                            let _ = Command::new(env!("CARGO_BIN_EXE_devctx"))
                                .env("DEVCTX_HOME", self.home())
                                .current_dir(sub.path())
                                .args(["serve", "--stop"])
                                .output();
                        }
                    }
                }
            }
        }
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
    let out = devctx(home, &["projects", "add", root.to_str().unwrap(), "--init"]);
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

// ---------------------------------------------------------------------------
// The two scenarios above assert on the startup banner. The two below have to
// call a tool, because what they check is behaviour the banner cannot show:
// which member a call is answered from, and where a memory is filed.
// ---------------------------------------------------------------------------

/// Speak MCP over stdio: initialise, call `tool`, return its parsed JSON.
///
/// stdin stays open until the answer arrives — closing it signals shutdown to
/// the stdio transport and would race a slow call. Mirrors the helper in
/// `mcp_tools.rs`; kept local so neither test file constrains the other.
fn call_tool(
    home: &Path,
    cwd: &Path,
    tool: &str,
    arguments: serde_json::Value,
) -> serde_json::Value {
    call_tool_with_user_home(home, cwd, None, tool, arguments)
}

/// Give a pinned `HOME` the real one's DuckDB extension cache.
///
/// Every store opens with `INSTALL vss; INSTALL fts;`, and DuckDB keeps those
/// extensions under `$HOME/.duckdb`. A test that pins `HOME` to a temp directory
/// therefore made each of its processes — the central daemon and every project
/// server — download ~40 MB before the store could open. Several such tests in
/// parallel on an ordinary connection took longer than `ensure`'s budget, and
/// the central daemon was reported as "could not be started" while it was still
/// downloading. CI's network hid it. A link to the shared cache keeps the walk
/// stopping at the pinned `HOME` without paying for the download.
fn share_duckdb_extensions(user_home: &Path) {
    let Some(real) = std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".duckdb")) else {
        return;
    };
    let link = user_home.join(".duckdb");
    if link.exists() {
        return;
    }
    let _ = std::fs::create_dir_all(&real);
    #[cfg(unix)]
    let _ = std::os::unix::fs::symlink(&real, &link);
}

/// [`call_tool`] with the process's `HOME` pinned (when given).
///
/// The server decides where a group member finds the workspace's `plans/` by walking upwards and
/// stopping at `$HOME` (PLAN-007 DD-2), and the per-project daemon it auto-spawns inherits this
/// environment. A test about that walk has to say where `HOME` is: the real one is not an
/// ancestor of a temp directory.
fn call_tool_with_user_home(
    home: &Path,
    cwd: &Path,
    user_home: Option<&Path>,
    tool: &str,
    arguments: serde_json::Value,
) -> serde_json::Value {
    use std::io::{BufRead, BufReader, Write};

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_devctx"));
    cmd.env("DEVCTX_HOME", home)
        .current_dir(cwd)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(h) = user_home {
        share_duckdb_extensions(h);
        cmd.env("HOME", h);
    }
    let mut child = cmd.spawn().expect("spawning the MCP server");

    let mut stdin = child.stdin.take().expect("stdin");
    let init = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"#,
        r#""2024-11-05","capabilities":{},"clientInfo":{"name":"it","version":"1"}}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        "\n",
    );
    let call = format!(
        r#"{{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{{"name":"{tool}","arguments":{arguments}}}}}"#
    );
    stdin.write_all(init.as_bytes()).unwrap();
    stdin.write_all(call.as_bytes()).unwrap();
    stdin.write_all(b"\n").unwrap();
    stdin.flush().unwrap();

    let stdout = child.stdout.take().expect("stdout");
    let mut lines = BufReader::new(stdout).lines();
    let out = loop {
        let Some(Ok(line)) = lines.next() else {
            let _ = child.kill();
            panic!("the MCP server closed before answering `{tool}`");
        };
        let Ok(msg) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if msg.get("id").and_then(|v| v.as_u64()) != Some(2) {
            continue;
        }
        if let Some(err) = msg.get("error") {
            let _ = child.kill();
            panic!("tool `{tool}` returned an error: {err}");
        }
        let text = msg["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("no text content in: {msg}"))
            .to_string();
        break serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text));
    };
    drop(stdin);
    let _ = child.wait();
    out
}

/// Scenario H — where a memory lands when nobody said.
///
/// A group binding means "the product", so an unqualified `remember` belongs to
/// the group. A project binding means one repository, so it stays `local`. The
/// default has to follow the binding, because the caller who omitted `scope`
/// is exactly the caller who does not know which one they are in.
#[test]
fn scope_defaults_follow_the_binding() {
    let _serial = EmbedLock::acquire();
    let tmp = Tmp::new("scopedefault");
    let home = tmp.home();
    let ws = tmp.dir("workspace");
    make_project(&home, &ws, "api", Some("ACME"));
    make_project(&home, &ws, "web", Some("ACME"));

    // Bound to the group: no `scope` given, so the memory is the product's.
    let grouped = call_tool(
        &home,
        &ws,
        "remember",
        serde_json::json!({"content": "el gateway deduplica por request id"}),
    );
    let as_text = grouped.to_string();
    assert!(
        as_text.contains("group"),
        "a group binding must file an unqualified memory as `group`, got: {as_text}"
    );

    // Bound to one project: the same call stays local.
    //
    // Its own `Tmp`, so its own central store and its own daemon. Sharing one
    // with the group phase above made this flaky under load: the second phase
    // could reach a daemon the first was still starting or stopping, and the
    // failure looked like a wrong default rather than a race.
    let solo_tmp = Tmp::new("scopedefault-solo");
    let solo_home = solo_tmp.home();
    let solo = solo_tmp.dir("workspace");
    make_project(&solo_home, &solo, "lonely", None);
    let inside = solo.join("lonely");
    let local = call_tool(
        &solo_home,
        &inside,
        "remember",
        serde_json::json!({"content": "el reintento genera un id nuevo por intento"}),
    );
    // A project binding does not echo `scope` back, so the invariant to assert
    // is the one that matters: it must NOT have been filed against the group.
    let as_text = local.to_string();
    assert!(
        !as_text.contains("group"),
        "a project binding must not file an unqualified memory as `group`, got: {as_text}"
    );
}

/// Scenario F — a `project` hint names the member a call is answered from.
///
/// In a group binding the code tools need a default, and a default is a guess.
/// `project` is how a caller says which member it means. The answer carries
/// `resolved_project`, because otherwise the caller cannot tell whether the
/// hint was honoured or silently ignored — and a silently ignored hint returns
/// another repository's code with no sign that it did.
#[test]
fn a_project_hint_selects_the_member() {
    let tmp = Tmp::new("hint");
    let home = tmp.home();
    let ws = tmp.dir("workspace");
    let alpha = make_project(&home, &ws, "alpha", Some("ACME"));
    let beta = make_project(&home, &ws, "beta", Some("ACME"));

    // Distinct contents: the file each member answers with is what proves which
    // one answered.
    std::fs::write(alpha.join("who.txt"), "i am alpha").unwrap();
    std::fs::write(beta.join("who.txt"), "i am beta").unwrap();

    // Some code tools ask git where the repository root is, so the members have
    // to be real repositories rather than registered directories.
    for member in [&alpha, &beta] {
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "t@t"],
            vec!["config", "user.name", "t"],
            vec!["add", "-A"],
            vec!["commit", "-qm", "init"],
        ] {
            Command::new("git")
                .args(args)
                .current_dir(member)
                .output()
                .expect("git");
        }
    }

    for (name, expected) in [("alpha", "i am alpha"), ("beta", "i am beta")] {
        let out = call_tool(
            &home,
            &ws,
            "read_file",
            serde_json::json!({"path": "who.txt", "project": name}),
        );
        let text = out.to_string();
        assert!(
            text.contains(expected),
            "`project: {name}` must be answered from {name}, got: {text}"
        );
    }

    // `read_file` returns a bare string, and `annotate` deliberately leaves
    // those alone rather than changing a shape callers already parse. A tool
    // that answers with an object carries `resolved_project`, which is how a
    // caller tells an honoured hint from a silently ignored one.
    let obj = call_tool(
        &home,
        &ws,
        "impact_analysis",
        serde_json::json!({"symbol": "nothing_here", "project": "beta"}),
    );
    assert_eq!(
        obj.get("resolved_project").and_then(|v| v.as_str()),
        Some("beta"),
        "an object answer must name the project it resolved to, got: {obj}"
    );
}

/// Call a tool and return the raw JSON-RPC message, so a test can assert on an
/// *error* rather than a result. `call_tool` panics on errors by design; here
/// the error is the thing under test.
fn call_tool_raw(
    home: &Path,
    cwd: &Path,
    tool: &str,
    arguments: serde_json::Value,
) -> serde_json::Value {
    use std::io::{BufRead, BufReader, Write};

    let mut child = Command::new(env!("CARGO_BIN_EXE_devctx"))
        .env("DEVCTX_HOME", home)
        .current_dir(cwd)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawning the MCP server");

    let mut stdin = child.stdin.take().expect("stdin");
    let init = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"#,
        r#""2024-11-05","capabilities":{},"clientInfo":{"name":"it","version":"1"}}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        "\n",
    );
    let call = format!(
        r#"{{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{{"name":"{tool}","arguments":{arguments}}}}}"#
    );
    stdin.write_all(init.as_bytes()).unwrap();
    stdin.write_all(call.as_bytes()).unwrap();
    stdin.write_all(b"\n").unwrap();
    stdin.flush().unwrap();

    let stdout = child.stdout.take().expect("stdout");
    let mut lines = BufReader::new(stdout).lines();
    let msg = loop {
        let Some(Ok(line)) = lines.next() else {
            let _ = child.kill();
            panic!("the MCP server closed before answering `{tool}`");
        };
        let Ok(m) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if m.get("id").and_then(|v| v.as_u64()) == Some(2) {
            break m;
        }
    };
    drop(stdin);
    let _ = child.wait();
    msg
}

/// TASK-009, first half — who a memory is attributed to.
///
/// Where a memory is written and who it came from are different questions. A
/// group binding answers the second with the group, never with a member:
/// picking one would invent a provenance nobody declared, and six months later
/// someone reads that a product-wide decision came out of one repository.
///
/// A `project` hint is the exception, and it has to be: the caller named the
/// repository, so that repository *is* the real provenance.
#[test]
fn a_group_binding_attributes_the_memory_to_the_group() {
    let _serial = EmbedLock::acquire();
    let tmp = Tmp::new("provenance");
    let home = tmp.home();
    let ws = tmp.dir("workspace");
    make_project(&home, &ws, "api", Some("ACME"));
    make_project(&home, &ws, "web", Some("ACME"));

    // No `project`: the product wrote this, so the product is the provenance.
    let grouped = call_tool(
        &home,
        &ws,
        "remember",
        serde_json::json!({"content": "el producto decidió X", "scope": "group"}),
    );
    let text = grouped.to_string();
    assert!(
        text.contains("ACME"),
        "a group binding must attribute to the group, got: {text}"
    );

    // Named a member: that member is the provenance, not the group. Otherwise
    // the override would be lying in the other direction.
    let hinted = call_tool(
        &home,
        &ws,
        "remember",
        serde_json::json!({"content": "esto salió de api", "scope": "group", "project": "api"}),
    );
    let text = hinted.to_string();
    assert!(
        text.contains("api"),
        "an explicit project must be the provenance, got: {text}"
    );
}

/// TASK-009, second half — `local` has nowhere to go in a group binding.
///
/// `local` means "this repository's own store", and a group binding has no such
/// repository. Choosing one would file the memory where nobody chose and nobody
/// will look — the silent-wrong-answer failure this whole plan exists to close.
/// So it fails, and the message carries both ways out rather than just the
/// diagnosis.
#[test]
fn local_scope_refuses_to_guess_a_repository() {
    let _serial = EmbedLock::acquire();
    let tmp = Tmp::new("localguard");
    let home = tmp.home();
    let ws = tmp.dir("workspace");
    make_project(&home, &ws, "api", Some("ACME"));
    make_project(&home, &ws, "web", Some("ACME"));

    let msg = call_tool_raw(
        &home,
        &ws,
        "remember",
        serde_json::json!({"content": "algo", "scope": "local"}),
    );
    let err = msg
        .get("error")
        .unwrap_or_else(|| panic!("`scope: local` in a group binding must fail, got: {msg}"))
        .to_string();

    // A refusal that does not say how to proceed just moves the problem.
    assert!(
        err.contains("project"),
        "the error must offer naming the project, got: {err}"
    );
    assert!(
        err.contains("group"),
        "the error must offer group scope, got: {err}"
    );

    // And inside a single repository the same call is ordinary. Its own `Tmp`
    // for the same reason as above: two phases sharing one central daemon race
    // under load, and the race reads as a behaviour failure.
    let solo_tmp = Tmp::new("localguard-solo");
    let solo_home = solo_tmp.home();
    let solo = solo_tmp.dir("workspace");
    make_project(&solo_home, &solo, "lonely", None);
    let ok = call_tool(
        &solo_home,
        &solo.join("lonely"),
        "remember",
        serde_json::json!({"content": "algo", "scope": "local"}),
    );
    assert!(
        ok.get("id").is_some(),
        "a project binding must accept `scope: local`, got: {ok}"
    );
}

/// The bug this task closes: `remember` on a fully unbound session (no
/// project, no group — an empty directory) used to fail outright and lose the
/// content, on every scope, because `backend_for` errored before `remember`
/// ever looked at what was asked for. It must now be saved centrally as
/// global instead of dropped.
#[test]
fn unbound_remember_saves_as_global_instead_of_losing_content() {
    let _serial = EmbedLock::acquire();
    let tmp = Tmp::new("unboundremember");
    let home = tmp.home();
    let empty = tmp.dir("nothing-here");

    let out = call_tool(
        &home,
        &empty,
        "remember",
        serde_json::json!({"content": "nada estaba vinculado, pero igual hay que guardarlo"}),
    );
    assert!(
        out.get("id").is_some(),
        "the memory must be saved, not dropped, got: {out}"
    );
    let text = out.to_string();
    assert!(
        text.contains("global"),
        "an unbound save must say it landed as global, got: {text}"
    );
}

/// `scope: local` while fully unbound truly has nowhere to go — there is not
/// even a group to redirect to. It must still fail, but the error has to carry
/// the original content back, or the model has no way to retry it without the
/// caller re-typing what it already sent.
#[test]
fn unbound_remember_with_explicit_local_preserves_content_in_the_error() {
    let _serial = EmbedLock::acquire();
    let tmp = Tmp::new("unboundlocal");
    let home = tmp.home();
    let empty = tmp.dir("nothing-here");

    let msg = call_tool_raw(
        &home,
        &empty,
        "remember",
        serde_json::json!({
            "content": "un contenido bien especifico que no se puede perder",
            "scope": "local",
        }),
    );
    let err = msg
        .get("error")
        .unwrap_or_else(|| panic!("`scope: local` while fully unbound must fail, got: {msg}"))
        .to_string();
    assert!(
        err.contains("un contenido bien especifico que no se puede perder"),
        "the original content must be echoed back so it is not lost, got: {err}"
    );
    assert!(
        err.contains("global"),
        "the error must point at `scope: global` as the way out, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// PLAN-007 TASK-006 — plans kept at the workspace root.
//
// A workspace is a directory that holds several repositories and is itself none of them, so
// there is no `.devctx/` in it and `devctx init` there would index the whole monorepo. Its
// `plans/` still has to be reachable: from an MCP session started at the root, from a session
// pinned to one member, and from `devctx web`. These drive the real binary, because the claim
// is about which directory a process reads — the thing a unit test on the resolver cannot reach.
//
// `HOME` is pinned to the temp root wherever a member has to find the workspace above it: the
// walk upwards stops at `$HOME`, and the real one is not an ancestor of a temp directory.
// ---------------------------------------------------------------------------

/// `plans/PLAN-N-<slug>/` with a plan doc and one pending task, for each `(dir, title)`.
fn write_plans(root: &Path, plans: &[(&str, &str)]) {
    for (dir, title) in plans {
        let plan = root.join("plans").join(dir);
        std::fs::create_dir_all(plan.join("tasks")).unwrap();
        let id = dir.split('-').take(2).collect::<Vec<_>>().join("-");
        std::fs::write(
            plan.join(format!("{dir}.md")),
            format!("# {id} — {title}\n"),
        )
        .unwrap();
        std::fs::write(
            plan.join("tasks/TASK-001-a.md"),
            "# TASK-001 — a\n\n- **Depende de:** —\n- **Estado:** `pending`\n",
        )
        .unwrap();
    }
}

fn plan_ids(value: &serde_json::Value) -> Vec<String> {
    value["plans"]
        .as_array()
        .unwrap_or_else(|| panic!("no `plans` array in: {value}"))
        .iter()
        .map(|p| p["id"].as_str().unwrap().to_string())
        .collect()
}

/// Whether `reported` (a path as the server printed it) is `expected`, ignoring symlinks.
fn same_path(reported: &serde_json::Value, expected: &Path) -> bool {
    let Some(reported) = reported.as_str() else {
        return false;
    };
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    canon(Path::new(reported)) == canon(expected)
}

/// Bound to a group from the workspace root, with `plans/` right there: the plans are the
/// workspace's and the answer says `workspace`. Neither member has `plans/`, and neither is asked.
#[test]
fn plan_status_in_a_group_reads_the_workspace_plans() {
    let tmp = Tmp::new("plans_group");
    let home = tmp.home();
    let ws = tmp.dir("workspace");
    make_project(&home, &ws, "api", Some("ACME"));
    make_project(&home, &ws, "web", Some("ACME"));
    write_plans(&ws, &[("PLAN-001-alfa", "alfa"), ("PLAN-002-beta", "beta")]);

    let out = call_tool_with_user_home(
        &home,
        &ws,
        Some(&tmp.0),
        "plan_status",
        serde_json::json!({}),
    );
    assert_eq!(plan_ids(&out), ["PLAN-001", "PLAN-002"], "{out}");
    assert_eq!(
        out["plans_root"]["source"].as_str(),
        Some("workspace"),
        "{out}"
    );
    assert!(same_path(&out["plans_root"]["path"], &ws), "{out}");

    // The detail form resolves against the same root.
    let detail = call_tool_with_user_home(
        &home,
        &ws,
        Some(&tmp.0),
        "plan_status",
        serde_json::json!({"plan": "PLAN-002"}),
    );
    assert_eq!(detail["plan"].as_str(), Some("PLAN-002"), "{detail}");
    assert_eq!(
        detail["plans_root"]["source"].as_str(),
        Some("workspace"),
        "{detail}"
    );
}

/// A `project` hint is specific: it goes to that member's own daemon, which finds the plans one
/// level up because its project declares `group:` — `ancestor`, not `workspace`.
#[test]
fn plan_status_with_a_project_hint_reads_the_workspace_as_an_ancestor() {
    let tmp = Tmp::new("plans_hint");
    let home = tmp.home();
    let ws = tmp.dir("workspace");
    make_project(&home, &ws, "api", Some("ACME"));
    make_project(&home, &ws, "web", Some("ACME"));
    write_plans(&ws, &[("PLAN-001-alfa", "alfa"), ("PLAN-002-beta", "beta")]);

    let out = call_tool_with_user_home(
        &home,
        &ws,
        Some(&tmp.0),
        "plan_status",
        serde_json::json!({"project": "api"}),
    );
    assert_eq!(plan_ids(&out), ["PLAN-001", "PLAN-002"], "{out}");
    assert_eq!(
        out["plans_root"]["source"].as_str(),
        Some("ancestor"),
        "{out}"
    );
    assert!(same_path(&out["plans_root"]["path"], &ws), "{out}");
    assert_eq!(
        out["resolved_project"].as_str(),
        Some("api"),
        "the answer must name the member that gave it: {out}"
    );
}

/// Started in a directory that holds `plans/` and no registered project: not an error. The plans
/// are listed from the directory itself.
#[test]
fn plan_status_unbound_in_a_directory_with_plans_lists_them() {
    let tmp = Tmp::new("plans_unbound");
    let home = tmp.home();
    let bare = tmp.dir("bare");
    write_plans(&bare, &[("PLAN-001-alfa", "alfa")]);

    let out = call_tool_with_user_home(
        &home,
        &bare,
        Some(&tmp.0),
        "plan_status",
        serde_json::json!({}),
    );
    assert_eq!(plan_ids(&out), ["PLAN-001"], "{out}");
    assert_eq!(
        out["plans_root"]["source"].as_str(),
        Some("workspace"),
        "{out}"
    );
    assert!(same_path(&out["plans_root"]["path"], &bare), "{out}");
}

/// The other half of "not an error": with no `plans/` either, the unbound server still fails, and
/// the failure still names the way out. Nothing about this changed.
#[test]
fn plan_status_unbound_without_plans_still_explains_itself() {
    let tmp = Tmp::new("plans_unbound_empty");
    let home = tmp.home();
    let empty = tmp.dir("nothing-here");

    let msg = call_tool_raw(&home, &empty, "plan_status", serde_json::json!({}));
    let err = msg
        .get("error")
        .unwrap_or_else(|| panic!("an unbound server with no plans/ must still fail, got: {msg}"))
        .to_string();
    assert!(
        err.contains("not bound to a project"),
        "the unbound error should say so, got: {err}"
    );
}

/// Bound to one project that has its own `plans/`: unchanged. The project's plans are served, as
/// `project`, even though the workspace above it has plans of its own and the project declares a
/// group.
#[test]
fn plan_status_bound_to_a_project_with_its_own_plans_is_unchanged() {
    let tmp = Tmp::new("plans_own");
    let home = tmp.home();
    let ws = tmp.dir("workspace");
    let api = make_project(&home, &ws, "api", Some("ACME"));
    make_project(&home, &ws, "web", Some("ACME"));
    write_plans(&ws, &[("PLAN-900-del-workspace", "workspace")]);
    write_plans(&api, &[("PLAN-001-propio", "propio")]);

    let out = call_tool_with_user_home(
        &home,
        &api,
        Some(&tmp.0),
        "plan_status",
        serde_json::json!({}),
    );
    assert_eq!(plan_ids(&out), ["PLAN-001"], "{out}");
    assert_eq!(
        out["plans_root"]["source"].as_str(),
        Some("project"),
        "{out}"
    );
    assert!(same_path(&out["plans_root"]["path"], &api), "{out}");
}

// --- `devctx web` ----------------------------------------------------------

/// A port nobody holds right now: bind to 0, read it, let go. Never a fixed one — the repository
/// has been bitten by fixed ports and by servers left over from earlier runs.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("binding an ephemeral port")
        .local_addr()
        .unwrap()
        .port()
}

/// Kills the child when dropped, so a failing assertion cannot leave a dashboard running.
struct KillOnDrop(std::process::Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// `GET path` over plain HTTP/1.0. `None` while nothing is listening yet.
fn http_get(port: u16, path: &str) -> Option<(String, String)> {
    use std::io::{Read, Write};

    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(30)))
        .unwrap();
    write!(stream, "GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n").unwrap();
    let mut raw = String::new();
    stream
        .read_to_string(&mut raw)
        .unwrap_or_else(|e| panic!("reading the response to GET {path}: {e}"));
    let (head, body) = raw
        .split_once("\r\n\r\n")
        .unwrap_or_else(|| panic!("no HTTP head in: {raw}"));
    Some((
        head.lines().next().unwrap_or("").to_string(),
        body.to_string(),
    ))
}

/// `devctx web` from a workspace root that has no `.devctx/`: it serves one member of the group,
/// and `/plans/status` answers with the workspace's plans — a member of a group reads the
/// workspace above it.
#[test]
fn web_in_a_workspace_without_devctx_serves_the_workspace_plans() {
    let tmp = Tmp::new("web_ws");
    let home = tmp.home();
    let ws = tmp.dir("workspace");
    make_project(&home, &ws, "api", Some("ACME"));
    make_project(&home, &ws, "web", Some("ACME"));
    write_plans(&ws, &[("PLAN-001-alfa", "alfa"), ("PLAN-002-beta", "beta")]);
    assert!(
        !ws.join(".devctx").exists(),
        "the workspace root must not be a project"
    );

    share_duckdb_extensions(&tmp.0);
    let port = free_port();
    let log_path = tmp.0.join("web.log");
    let log = std::fs::File::create(&log_path).unwrap();
    // Declared after `tmp`, so it is killed before `Tmp::drop` stops the servers and deletes the tree.
    let mut web = KillOnDrop(
        Command::new(env!("CARGO_BIN_EXE_devctx"))
            .env("DEVCTX_HOME", &home)
            .env("HOME", &tmp.0)
            .current_dir(&ws)
            .args(["web", "--addr", &format!("127.0.0.1:{port}"), "--no-open"])
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .expect("spawning `devctx web`"),
    );

    let started = std::time::Instant::now();
    let (status, body) = loop {
        if let Some(answer) = http_get(port, "/plans/status") {
            break answer;
        }
        if let Ok(Some(exit)) = web.0.try_wait() {
            panic!(
                "`devctx web` exited early ({exit}):\n{}",
                std::fs::read_to_string(&log_path).unwrap_or_default()
            );
        }
        assert!(
            started.elapsed().as_secs() < 90,
            "the dashboard never came up:\n{}",
            std::fs::read_to_string(&log_path).unwrap_or_default()
        );
        std::thread::sleep(std::time::Duration::from_millis(250));
    };

    assert!(
        status.contains("200"),
        "GET /plans/status: {status}\n{body}"
    );
    let value: serde_json::Value =
        serde_json::from_str(&body).unwrap_or_else(|e| panic!("not JSON ({e}): {body}"));
    assert_eq!(plan_ids(&value), ["PLAN-001", "PLAN-002"], "{value}");
    assert_eq!(
        value["plans_root"]["source"].as_str(),
        Some("ancestor"),
        "{value}"
    );
    assert!(same_path(&value["plans_root"]["path"], &ws), "{value}");

    // The banner names where the plans come from, so nobody has to guess.
    let log_text = std::fs::read_to_string(&log_path).unwrap_or_default();
    assert!(
        log_text.contains("Workspace") && log_text.contains("plans from"),
        "expected the workspace banner, got:\n{log_text}"
    );
}

/// `devctx web` where there is no project at all — neither above nor below — fails, and the
/// failure carries the same explanation `devctx mcp` gives, not just "run `devctx init`". Plans in
/// that directory do not change it: the dashboard needs a project to serve.
#[test]
fn web_without_any_project_fails_with_the_unbound_explanation() {
    let tmp = Tmp::new("web_none");
    let home = tmp.home();
    let bare = tmp.dir("bare");
    write_plans(&bare, &[("PLAN-001-alfa", "alfa")]);

    share_duckdb_extensions(&tmp.0);
    let port = free_port();
    let mut child = Command::new(env!("CARGO_BIN_EXE_devctx"))
        .env("DEVCTX_HOME", &home)
        .env("HOME", &tmp.0)
        .current_dir(&bare)
        .args(["web", "--addr", &format!("127.0.0.1:{port}"), "--no-open"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawning `devctx web`");

    // A bound dashboard would never exit: poll with a deadline rather than block on `output()`.
    let started = std::time::Instant::now();
    let exit = loop {
        if let Some(exit) = child.try_wait().unwrap() {
            break exit;
        }
        if started.elapsed().as_secs() > 60 {
            let _ = child.kill();
            let _ = child.wait();
            panic!("`devctx web` with no project must exit, but it kept running");
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    let out = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(!exit.success(), "expected a failure, stderr:\n{stderr}");
    assert!(
        stderr.contains("No DevCtxEngine project found"),
        "the original error should survive:\n{stderr}"
    );
    assert!(
        stderr.contains("neither inside a DevCtxEngine repository nor above one"),
        "the why-unbound explanation should be appended:\n{stderr}"
    );
}
