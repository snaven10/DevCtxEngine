//! A search answered from a fallback branch has the object shape
//! `{results, branch_fallback}`; the consumers of that answer must read it and
//! never lose the hits (PLAN-008 TASK-006 fixup).
//!
//! The scenario is the common one: a repository indexed on `main` but checked
//! out on a branch nobody indexed. Real binary, real index, real serve.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

mod common;

/// Serialises tests that load the embedding model, across test binaries (same
/// lock file as `mcp_binding.rs` and `git_env_cli.rs`).
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

/// Scratch directory with a private `DEVCTX_HOME`, unique per test and per run
/// (the server's port derives from the project path).
struct Tmp {
    root: PathBuf,
    projects: Vec<PathBuf>,
}

impl Tmp {
    fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("devctx_shapes_it_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        common::share_models(&root.join("central"));
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
        common::reap_servers_under(&self.root);
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

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

/// A git repository with one commit on `main`, registered in group `ACME`,
/// indexed on `main`, then checked out on `feat`, which nobody indexed.
fn make_member(tmp: &mut Tmp, parent: &Path, name: &str, file: &str, body: &str, on_feat: bool) {
    let root = parent.join(name);
    std::fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    std::fs::write(root.join(file), body).unwrap();
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "init"]);
    let out = devctx(&tmp.home(), &root, &["init", "--yes", "--group", "ACME"]);
    assert!(out.status.success(), "init {name}: {}", text(&out));
    tmp.projects.push(root.clone());
    // The fallback needs a declared default branch to fall back to.
    let cfg_path = root.join(".devctx/config.yaml");
    let cfg = std::fs::read_to_string(&cfg_path).unwrap();
    assert!(cfg.contains("branches: []"), "unexpected config:\n{cfg}");
    std::fs::write(&cfg_path, cfg.replace("branches: []", "branches: [main]")).unwrap();
    let out = devctx(&tmp.home(), &root, &["index"]);
    assert!(out.status.success(), "index {name}: {}", text(&out));
    if on_feat {
        git(&root, &["checkout", "-q", "-b", "feat"]);
    }
}

fn call_tool(
    home: &Path,
    cwd: &Path,
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
        let t = msg["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("no text content in: {msg}"))
            .to_string();
        break serde_json::from_str(&t)
            .unwrap_or_else(|e| panic!("tool `{tool}` returned non-JSON ({e}): {t}"));
    };
    drop(stdin);
    let _ = child.wait();
    result
}

/// Group search with one member checked out on an unindexed branch: its hits
/// are real (a file each), none is the wrapper object, and the fallback is
/// reported against that member.
#[test]
fn group_search_keeps_the_hits_of_a_member_answered_from_a_fallback_branch() {
    let _embed = EmbedLock::acquire();
    let mut tmp = Tmp::new("group");
    let ws = tmp.root.join("workspace");
    std::fs::create_dir_all(&ws).unwrap();
    make_member(
        &mut tmp,
        &ws,
        "api",
        "a.rs",
        "pub fn alpha_marker() {}\n",
        true,
    );
    make_member(
        &mut tmp,
        &ws,
        "web",
        "w.rs",
        "pub fn webby_marker() {}\n",
        false,
    );

    let out = call_tool(
        &tmp.home(),
        &ws,
        "search",
        serde_json::json!({ "query": "alpha_marker", "limit": 10 }),
    );
    let hits = out["hits"]
        .as_array()
        .unwrap_or_else(|| panic!("no hits: {out}"));
    assert!(!hits.is_empty(), "group search found nothing: {out}");
    for h in hits {
        assert!(
            h["file"].as_str().is_some_and(|f| !f.is_empty()),
            "a hit without a file (the wrapper leaked in?): {h}\nfull: {out}"
        );
    }
    assert!(
        hits.iter()
            .any(|h| h["project"] == "api" && h["file"] == "a.rs"),
        "the member on the fallback branch lost its hits: {out}"
    );
    let fb = &out["member_notes"]["api"]["branch_fallback"];
    assert_eq!(fb["current"], "feat", "fallback not reported: {out}");
    assert!(
        out["member_notes"].get("web").is_none(),
        "a member on its indexed branch needs no note: {out}"
    );
}

/// `devctx search` (table) against a serve answering `{results, branch_fallback}`
/// prints the rows, not "No results.".
#[test]
fn table_search_prints_rows_when_the_serve_answers_from_a_fallback_branch() {
    let _embed = EmbedLock::acquire();
    let mut tmp = Tmp::new("table");
    let ws = tmp.root.clone();
    make_member(
        &mut tmp,
        &ws,
        "solo",
        "a.rs",
        "pub fn alpha_marker() {}\n",
        true,
    );
    let repo = ws.join("solo");

    // The JSON form proves the serve really answered with the object shape.
    let json = devctx(
        &tmp.home(),
        &repo,
        &["search", "alpha_marker", "--format", "json"],
    );
    let parsed: serde_json::Value =
        serde_json::from_slice(&json.stdout).unwrap_or_else(|_| panic!("{}", text(&json)));
    assert!(
        parsed.get("branch_fallback").is_some(),
        "expected the object shape with a fallback: {parsed}"
    );

    let table = devctx(&tmp.home(), &repo, &["search", "alpha_marker"]);
    let t = text(&table);
    assert!(table.status.success(), "{t}");
    assert!(!t.contains("No results."), "table lost the rows: {t}");
    assert!(t.contains("a.rs:"), "no row printed: {t}");
    assert!(t.contains("branch"), "the fallback line is missing: {t}");
}

/// The contract after PLAN-008 TASK-010: `search` is an object even when there
/// is nothing to say — current branch indexed, no fallback, nothing omitted.
/// Through the CLI (`--format json`) and through the MCP tool.
#[test]
fn search_is_always_an_object_even_without_notes() {
    let _embed = EmbedLock::acquire();
    let mut tmp = Tmp::new("always_object");
    let ws = tmp.root.clone();
    make_member(
        &mut tmp,
        &ws,
        "solo",
        "a.rs",
        "pub fn alpha_marker() {}\n",
        false,
    );
    let repo = ws.join("solo");

    let json = devctx(
        &tmp.home(),
        &repo,
        &["search", "alpha_marker", "--format", "json"],
    );
    let parsed: serde_json::Value =
        serde_json::from_slice(&json.stdout).unwrap_or_else(|_| panic!("{}", text(&json)));
    assert!(parsed.is_object(), "the CLI printed a bare array: {parsed}");
    assert!(
        parsed["results"]
            .as_array()
            .is_some_and(|r| r.iter().any(|h| h["file"] == "a.rs")),
        "{parsed}"
    );
    assert!(parsed.get("branch_fallback").is_none(), "{parsed}");

    let tool = call_tool(
        &tmp.home(),
        &repo,
        "search",
        serde_json::json!({ "query": "alpha_marker", "limit": 5 }),
    );
    assert!(tool.is_object(), "the tool answered a bare array: {tool}");
    assert!(
        tool["results"]
            .as_array()
            .is_some_and(|r| r.iter().any(|h| h["file"] == "a.rs")),
        "{tool}"
    );
}

/// `search_routes` end to end (MCP -> serve -> store): 25 routes pages as
/// 10 + 10 + 5 with `total`, `next_offset` and `omitted.count`; `limit`
/// defaults to 20; `routes_for_handler` is the same object.
#[test]
fn search_routes_pages_through_the_tool() {
    let _embed = EmbedLock::acquire();
    let mut tmp = Tmp::new("routes_paging");
    let ws = tmp.root.clone();
    let mut body = String::from("from fastapi import FastAPI\napp = FastAPI()\n\n");
    for i in 0..25 {
        body.push_str(&format!(
            "@app.get(\"/items/r{i:02}\")\ndef handler_{i:02}():\n    return {i}\n\n"
        ));
    }
    make_member(&mut tmp, &ws, "solo", "api.py", &body, false);
    let repo = ws.join("solo");
    let call = |args: serde_json::Value| call_tool(&tmp.home(), &repo, "search_routes", args);
    let paths = |v: &serde_json::Value| -> Vec<String> {
        v["routes"]
            .as_array()
            .unwrap_or_else(|| panic!("no routes array: {v}"))
            .iter()
            .map(|r| r["path"].as_str().unwrap().to_string())
            .collect()
    };

    let default = call(serde_json::json!({}));
    assert_eq!(paths(&default).len(), 20, "{default}");
    assert_eq!(default["total"], 25);
    assert_eq!(default["omitted"]["count"], 5);
    assert_eq!(default["next_offset"], 20);

    let p1 = call(serde_json::json!({ "limit": 10 }));
    let p2 = call(serde_json::json!({ "limit": 10, "offset": 10 }));
    let p3 = call(serde_json::json!({ "limit": 10, "offset": 20 }));
    assert_eq!(p1["omitted"]["count"], 15, "{p1}");
    assert_eq!(p2["next_offset"], 20, "{p2}");
    assert_eq!(paths(&p3).len(), 5, "{p3}");
    assert!(
        p3.get("next_offset").is_none() && p3.get("omitted").is_none(),
        "{p3}"
    );
    let mut all = paths(&p1);
    all.extend(paths(&p2));
    all.extend(paths(&p3));
    let unique: std::collections::HashSet<_> = all.iter().collect();
    assert_eq!(all.len(), 25);
    assert_eq!(unique.len(), 25, "a page repeated a route: {all:?}");

    let by_handler = call_tool(
        &tmp.home(),
        &repo,
        "routes_for_handler",
        serde_json::json!({ "handler": "handler_03" }),
    );
    assert_eq!(paths(&by_handler), vec!["/items/r03"], "{by_handler}");
    assert_eq!(by_handler["total"], 1);
}
