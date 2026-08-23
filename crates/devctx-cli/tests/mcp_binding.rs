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
    let solo = tmp.dir("solo-workspace");
    make_project(&home, &solo, "lonely", None);
    let inside = solo.join("lonely");
    let local = call_tool(
        &home,
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

    // And inside a single repository the same call is ordinary.
    let solo = tmp.dir("solo");
    make_project(&home, &solo, "lonely", None);
    let ok = call_tool(
        &home,
        &solo.join("lonely"),
        "remember",
        serde_json::json!({"content": "algo", "scope": "local"}),
    );
    assert!(
        ok.get("id").is_some(),
        "a project binding must accept `scope: local`, got: {ok}"
    );
}
