//! End-to-end tests for the MCP tools, driven over real stdio JSON-RPC.
//!
//! These matter because the MCP surface is the only one an agent ever sees: a
//! tool that works from the CLI but is missing or misnamed over the protocol is
//! invisible where it counts.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

mod common;

/// A scratch central home that cleans up after itself.
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
        // Unique per run: `auto_addr` hashes the project path into a port, so a
        // fixed path collides with a server left over from an earlier run —
        // one whose database directory no longer exists.
        let dir = std::env::temp_dir().join(format!("devctx_mcp_it_{tag}_{}", std::process::id()));
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
        // `serve --stop` acts on the project of the *current directory*, so
        // running it from the test process's cwd stopped a server belonging to
        // this repository rather than the temp ones. Each project has to be
        // asked from inside itself, or its server outlives the test by its
        // fifteen-minute idle window and the next run competes with it.
        if let Ok(entries) = std::fs::read_dir(&self.0) {
            for entry in entries.flatten() {
                if entry.path().join(".devctx").is_dir() {
                    let _ = Command::new(env!("CARGO_BIN_EXE_devctx"))
                        .env("DEVCTX_HOME", self.home())
                        .current_dir(entry.path())
                        .args(["serve", "--stop"])
                        .output();
                }
            }
        }
        let _ = Command::new(env!("CARGO_BIN_EXE_devctx"))
            .env("DEVCTX_HOME", self.home())
            .args(["serve", "--central", "--stop"])
            .output();
        common::reap_servers_under(&self.0);
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn devctx(home: &PathBuf, cwd: &PathBuf, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_devctx"))
        .env("DEVCTX_HOME", home)
        .current_dir(cwd)
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
}

/// Speak MCP over stdio: initialize, then call `tool` with `arguments`, and
/// return the tool's parsed JSON result.
///
/// stdin is deliberately held open until the response arrives. Closing it
/// signals shutdown to the stdio transport, which would race a slow call (one
/// that loads a model, say) and leave the request unanswered.
fn call_tool(
    home: &PathBuf,
    cwd: &PathBuf,
    tool: &str,
    arguments: serde_json::Value,
) -> serde_json::Value {
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
    let result = loop {
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
        break serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("tool `{tool}` returned non-JSON ({e}): {text}"));
    };

    drop(stdin);
    let _ = child.wait();
    result
}

/// Like [`call_tool`], but for a call expected to fail: returns the JSON-RPC error's message
/// instead of panicking on it, and panics if the call succeeds instead.
fn call_tool_error(
    home: &PathBuf,
    cwd: &PathBuf,
    tool: &str,
    arguments: serde_json::Value,
) -> String {
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
    let message = loop {
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
        let Some(err) = msg.get("error") else {
            panic!("expected tool `{tool}` to fail, but it answered: {msg}");
        };
        break err["message"].as_str().unwrap_or_default().to_string();
    };

    drop(stdin);
    let _ = child.wait();
    message
}

/// A `plans/` fixture: `PLAN-001` all done (4/4), `PLAN-002-x` with a ready task, an
/// in-progress task, a task blocked on the ready one, and a task depending on a nonexistent
/// `TASK-099` — one repository exercising every branch of `plan_status` (PLAN-005 TASK-008).
fn write_plan_status_fixture(repo: &std::path::Path) {
    let plan1 = repo.join("plans/PLAN-001/tasks");
    std::fs::create_dir_all(&plan1).unwrap();
    std::fs::write(
        repo.join("plans/PLAN-001/PLAN-001.md"),
        "# PLAN-001 — ya cerrado\n",
    )
    .unwrap();
    for n in 1..=4 {
        std::fs::write(
            plan1.join(format!("TASK-{n:03}-x.md")),
            format!("# TASK-{n:03} — hecho\n\n- **Depende de:** —\n- **Estado:** `done`\n\n## Objetivo\n"),
        )
        .unwrap();
    }

    let plan2 = repo.join("plans/PLAN-002-x/tasks");
    std::fs::create_dir_all(&plan2).unwrap();
    std::fs::write(
        repo.join("plans/PLAN-002-x/PLAN-002-x.md"),
        "# PLAN-002 — con pendientes\n",
    )
    .unwrap();
    std::fs::write(
        plan2.join("TASK-001-ready.md"),
        "# TASK-001 — lista\n\n- **Depende de:** —\n- **Estado:** `pending`\n\n## Objetivo\n",
    )
    .unwrap();
    std::fs::write(
        plan2.join("TASK-002-curso.md"),
        "# TASK-002 — en curso\n\n- **Depende de:** —\n- **Estado:** `in_progress`\n\n## Objetivo\n",
    )
    .unwrap();
    std::fs::write(
        plan2.join("TASK-003-bloqueada.md"),
        "# TASK-003 — bloqueada\n\n- **Depende de:** TASK-001\n- **Estado:** `pending`\n\n## Objetivo\n",
    )
    .unwrap();
    std::fs::write(
        plan2.join("TASK-004-faltante.md"),
        "# TASK-004 — dep faltante\n\n- **Depende de:** TASK-099\n- **Estado:** `pending`\n\n\
         ## Archivos\n\n- **Modificar:** `crates/a/src/x.rs`\n",
    )
    .unwrap();
}

/// `plan_status` with no argument lists every plan and names the active one (the plan with
/// pending work, never the one that is already 4/4).
#[test]
fn plan_status_lists_plans_and_names_the_active_one() {
    let tmp = Tmp::new("planstatus_list");
    let home = tmp.home();
    let repo = tmp.repo("proj");
    write_plan_status_fixture(&repo);
    devctx(&home, &repo, &["projects", "add", ".", "--init"]);

    let out = call_tool(&home, &repo, "plan_status", serde_json::json!({}));
    assert_eq!(out["active"].as_str(), Some("PLAN-002"));
    let plans = out["plans"].as_array().unwrap();
    let plan1 = plans.iter().find(|p| p["id"] == "PLAN-001").unwrap();
    assert_eq!(plan1["done"], 4);
    assert_eq!(plan1["total"], 4);
    assert_eq!(plan1["active"], false);
    let plan2 = plans.iter().find(|p| p["id"] == "PLAN-002").unwrap();
    assert_eq!(plan2["active"], true);
}

/// `plan_status` with `plan` gives ready/in-progress/blocked detail, including a `waiting_on`
/// for the task blocked on another, and a warning about the task with a missing dependency.
#[test]
fn plan_status_detail_reports_ready_in_progress_blocked_and_a_missing_dep_warning() {
    let tmp = Tmp::new("planstatus_detail");
    let home = tmp.home();
    let repo = tmp.repo("proj");
    write_plan_status_fixture(&repo);
    devctx(&home, &repo, &["projects", "add", ".", "--init"]);

    let out = call_tool(
        &home,
        &repo,
        "plan_status",
        serde_json::json!({ "plan": "2" }),
    );
    assert_eq!(out["plan"].as_str(), Some("PLAN-002"));
    let ready: Vec<&str> = out["ready"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap())
        .collect();
    assert_eq!(ready, vec!["TASK-001"]);
    let in_progress: Vec<&str> = out["in_progress"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap())
        .collect();
    assert_eq!(in_progress, vec!["TASK-002"]);
    let blocked = out["blocked"].as_array().unwrap();
    let task3 = blocked.iter().find(|b| b["id"] == "TASK-003").unwrap();
    assert_eq!(
        task3["waiting_on"].as_array().unwrap(),
        &vec![serde_json::json!("TASK-001")]
    );
    let warnings: Vec<&str> = out["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w.as_str().unwrap())
        .collect();
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("TASK-004") && w.contains("TASK-099")),
        "{warnings:?}"
    );
}

/// A plan id that does not exist fails with the ids that do, not an empty answer.
#[test]
fn plan_status_unknown_plan_errors_with_available_ids() {
    let tmp = Tmp::new("planstatus_unknown");
    let home = tmp.home();
    let repo = tmp.repo("proj");
    write_plan_status_fixture(&repo);
    devctx(&home, &repo, &["projects", "add", ".", "--init"]);

    let message = call_tool_error(
        &home,
        &repo,
        "plan_status",
        serde_json::json!({ "plan": "PLAN-009" }),
    );
    assert!(
        message.contains("PLAN-001") && message.contains("PLAN-002"),
        "{message}"
    );
}

/// `memories_by_file` over MCP carries `plan_tasks` for a file a task mentions.
#[test]
fn memories_by_file_carries_plan_tasks_over_mcp() {
    let tmp = Tmp::new("planstatus_memfile");
    let home = tmp.home();
    let repo = tmp.repo("proj");
    write_plan_status_fixture(&repo);
    devctx(&home, &repo, &["projects", "add", ".", "--init"]);

    let out = call_tool(
        &home,
        &repo,
        "memories_by_file",
        serde_json::json!({ "file": "crates/a/src/x.rs" }),
    );
    assert!(out.get("plan_tasks").is_some(), "{out}");
    let plan_tasks = out["plan_tasks"].as_array().unwrap();
    assert!(
        plan_tasks
            .iter()
            .any(|t| t["task"] == "TASK-004" && t["plan"] == "PLAN-002"),
        "{plan_tasks:?}"
    );
}

/// The CLI's `devctx plan-status --format json` returns exactly the same JSON as the
/// `plan_status` tool over MCP (unbudgeted — the CLI never applies `DEVCTX_MAX_OUTPUT_TOKENS`),
/// since both are built from the same `plan_status_value` (PLAN-005 TASK-004 Paso 3).
#[test]
fn cli_plan_status_json_matches_the_mcp_tool() {
    let tmp = Tmp::new("planstatus_cli_matches_tool");
    let home = tmp.home();
    let repo = tmp.repo("proj");
    write_plan_status_fixture(&repo);
    devctx(&home, &repo, &["projects", "add", ".", "--init"]);

    let tool_out = call_tool(
        &home,
        &repo,
        "plan_status",
        serde_json::json!({ "plan": "PLAN-002" }),
    );

    let cli_stdout = devctx(
        &home,
        &repo,
        &["plan-status", "PLAN-002", "--format", "json"],
    );
    let mut cli_out: serde_json::Value =
        serde_json::from_str(cli_stdout.trim()).expect("valid JSON");

    // The tool annotates `resolved_project` when a `project` hint resolved a different
    // repository than the session's binding; the CLI has no such concept. Strip it before
    // comparing — everything else must be identical.
    if let Some(obj) = cli_out.as_object_mut() {
        obj.remove("resolved_project");
    }
    let mut tool_out = tool_out;
    if let Some(obj) = tool_out.as_object_mut() {
        obj.remove("resolved_project");
    }
    assert_eq!(cli_out, tool_out);
}

/// An agent working in one repository must be able to discover the others.
/// Needs no model, so it runs on every suite.
#[test]
fn list_projects_sees_every_registered_repo() {
    let tmp = Tmp::new("listprojects");
    let home = tmp.home();
    let shop = tmp.repo("shop");
    let depot = tmp.repo("depot");
    devctx(
        &home,
        &shop,
        &[
            "projects",
            "add",
            ".",
            "--init",
            "--description",
            "the storefront",
        ],
    );
    devctx(&home, &depot, &["projects", "add", ".", "--init"]);

    // Called from `depot`, which knows nothing about `shop` on its own.
    let out = call_tool(&home, &depot, "list_projects", serde_json::json!({}));
    let projects = out["projects"].as_array().expect("a projects array");

    let mut names: Vec<&str> = projects
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, vec!["depot", "shop"]);

    let shop_row = projects.iter().find(|p| p["name"] == "shop").unwrap();
    assert_eq!(shop_row["description"], "the storefront");
    assert_eq!(shop_row["embed_model"], "minilm-l6");
    assert!(shop_row["path"].as_str().unwrap().ends_with("shop"));
}

/// Deactivated projects stay out of the agent's view unless it asks.
#[test]
fn list_projects_hides_deactivated_projects() {
    let tmp = Tmp::new("listinactive");
    let home = tmp.home();
    let repo = tmp.repo("solo");
    devctx(&home, &repo, &["projects", "add", ".", "--init"]);
    devctx(&home, &repo, &["projects", "rm", "solo", "--deactivate"]);

    let hidden = call_tool(&home, &repo, "list_projects", serde_json::json!({}));
    assert!(hidden["projects"].as_array().unwrap().is_empty());

    let shown = call_tool(
        &home,
        &repo,
        "list_projects",
        serde_json::json!({ "include_inactive": true }),
    );
    assert_eq!(shown["projects"].as_array().unwrap().len(), 1);
}

/// The payoff: a lesson saved in one repository is recalled, over MCP, from
/// another — tagged with the scope and the repository that contributed it.
///
/// Ignored by default because it loads a real embedding model.
#[test]
#[ignore = "loads an embedding model (downloads it on a cold cache)"]
fn recall_reaches_global_memories_from_another_project() {
    let _serial = EmbedLock::acquire();
    let tmp = Tmp::new("globalrecall");
    let home = tmp.home();
    let shop = tmp.repo("shop");
    let depot = tmp.repo("depot");
    devctx(&home, &shop, &["projects", "add", ".", "--init"]);
    devctx(&home, &depot, &["projects", "add", ".", "--init"]);

    devctx(
        &home,
        &shop,
        &[
            "remember",
            "never trust a price sent by the client; recompute it server-side against the catalogue",
            "--title",
            "Recompute prices server-side",
            "--type",
            "insight",
            "--scope",
            "global",
        ],
    );
    devctx(
        &home,
        &shop,
        &[
            "remember",
            "checkout lives in src/checkout.rs",
            "--scope",
            "local",
        ],
    );

    let out = call_tool(
        &home,
        &depot,
        "recall",
        serde_json::json!({ "query": "how should I handle pricing safely", "scope": "global" }),
    );
    let memories = out["memories"].as_array().expect("a memories array");
    let hit = memories
        .iter()
        .find(|m| m["title"] == "Recompute prices server-side")
        .expect("the global lesson should be reachable from another project");
    assert_eq!(
        hit["scope"], "global",
        "hits carry the scope they came from"
    );
    assert_eq!(
        hit["repo"], "shop",
        "and the repository that contributed them"
    );

    // The other project's private note must not be reachable.
    let all = call_tool(
        &home,
        &depot,
        "recall",
        serde_json::json!({ "query": "where does checkout live", "scope": "all" }),
    );
    let leaked = all["memories"].as_array().unwrap().iter().any(|m| {
        m["content"]
            .as_str()
            .unwrap_or_default()
            .contains("checkout.rs")
    });
    assert!(
        !leaked,
        "a project-local memory leaked across projects: {all}"
    );
}

/// The memory protocol document must be discoverable over MCP itself, not
/// only by pasting it into a project's own instructions file. Needs no model
/// (`list_prompts` is a static list), so it runs on every suite.
#[test]
fn lists_the_memory_protocol_prompt() {
    let tmp = Tmp::new("prompts");
    let home = tmp.home();
    let repo = tmp.repo("solo");
    devctx(&home, &repo, &["projects", "add", ".", "--init"]);

    let mut child = Command::new(env!("CARGO_BIN_EXE_devctx"))
        .env("DEVCTX_HOME", &home)
        .current_dir(&repo)
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
    let call = r#"{"jsonrpc":"2.0","id":2,"method":"prompts/list","params":{}}"#;
    stdin.write_all(init.as_bytes()).unwrap();
    stdin.write_all(call.as_bytes()).unwrap();
    stdin.write_all(b"\n").unwrap();
    stdin.flush().unwrap();

    let stdout = child.stdout.take().expect("stdout");
    let mut lines = BufReader::new(stdout).lines();
    let result = loop {
        let Some(Ok(line)) = lines.next() else {
            let _ = child.kill();
            panic!("the MCP server closed before answering prompts/list");
        };
        let Ok(msg) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if msg.get("id").and_then(|v| v.as_u64()) != Some(2) {
            continue;
        }
        if let Some(err) = msg.get("error") {
            let _ = child.kill();
            panic!("prompts/list returned an error: {err}");
        }
        break msg["result"].clone();
    };
    drop(stdin);
    let _ = child.wait();

    let prompts = result["prompts"].as_array().expect("a prompts array");
    let names: Vec<&str> = prompts.iter().filter_map(|p| p["name"].as_str()).collect();
    assert!(
        names.contains(&"memory-protocol"),
        "expected the memory-protocol prompt, got: {names:?}"
    );
}
