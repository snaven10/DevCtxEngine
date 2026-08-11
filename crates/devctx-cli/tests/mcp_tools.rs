//! End-to-end tests for the MCP tools, driven over real stdio JSON-RPC.
//!
//! These matter because the MCP surface is the only one an agent ever sees: a
//! tool that works from the CLI but is missing or misnamed over the protocol is
//! invisible where it counts.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// A scratch central home that cleans up after itself.
struct Tmp(PathBuf);

impl Tmp {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("devctx_mcp_it_{tag}"));
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
        for args in [
            vec!["serve", "--central", "--stop"],
            vec!["serve", "--stop"],
        ] {
            let _ = Command::new(env!("CARGO_BIN_EXE_devctx"))
                .env("DEVCTX_HOME", self.home())
                .args(args)
                .output();
        }
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
