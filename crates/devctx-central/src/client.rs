//! Client side of the central daemon: discover a running `devctx serve
//! --central`, auto-spawn one when absent, and route calls to it.
//!
//! Everything that is not the daemon itself reaches the central store through
//! here — the CLI, the MCP server, and later the TUI. That matters because the
//! central database is a singleton: opening it from two processes at once does
//! not degrade, it fails outright, since DuckDB permits a single writing process
//! per file.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{CentralError, Result};
use crate::paths::CentralPaths;

/// What `devctx serve --central` advertises so clients can find it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServeInfo {
    /// `host:port` the daemon is bound to.
    pub addr: String,
    /// Bearer token it requires (if any).
    #[serde(default)]
    pub token: Option<String>,
    /// Owner process id (informational).
    #[serde(default)]
    pub pid: Option<u32>,
}

/// A reachable central daemon.
pub struct CentralClient {
    base: String,
    token: Option<String>,
}

/// Path of the discovery file advertising the central daemon.
pub fn serve_file(paths: &CentralPaths) -> PathBuf {
    paths.serve_file.clone()
}

/// A deterministic loopback address per central home, so two different
/// `DEVCTX_HOME`s (a test run and a real one, say) never fight over a port.
fn auto_addr(paths: &CentralPaths) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in paths.dir.to_string_lossy().as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    // A band above the per-project range, so the two never collide.
    let port = 60000 + (h % 5000) as u16;
    format!("127.0.0.1:{port}")
}

/// Write the discovery file so clients can find this daemon.
pub fn write_serve_file(paths: &CentralPaths, addr: SocketAddr, token: Option<&str>) -> Result<()> {
    let info = ServeInfo {
        addr: addr.to_string(),
        token: token.map(str::to_string),
        pid: Some(std::process::id()),
    };
    let path = serve_file(paths);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let body =
        serde_json::to_vec_pretty(&info).map_err(|e| CentralError::Request(e.to_string()))?;
    std::fs::write(&path, body).map_err(|e| CentralError::Io(e, path.clone()))?;
    Ok(())
}

/// Remove the discovery file (best effort).
///
/// Unconditional: only for callers that have already established the advertised
/// daemon is gone. A daemon tidying up after itself must use
/// [`remove_own_serve_file`].
pub fn remove_serve_file(paths: &CentralPaths) {
    let _ = std::fs::remove_file(serve_file(paths));
}

/// Remove the discovery file only while it still advertises this process.
///
/// The central store is a singleton, so a second daemon starting always loses —
/// and on its way out it used to delete the file belonging to the winner. The
/// winner then ran on, advertised nowhere, while every client fell back to
/// opening the database it was holding.
pub fn remove_own_serve_file(paths: &CentralPaths) {
    let path = serve_file(paths);
    let ours = std::fs::read(&path)
        .ok()
        .and_then(|raw| serde_json::from_slice::<ServeInfo>(&raw).ok())
        .and_then(|info| info.pid)
        .is_some_and(|pid| pid == std::process::id());
    if ours {
        let _ = std::fs::remove_file(&path);
    }
}

/// Discover a running central daemon and confirm it answers.
pub fn discover(paths: &CentralPaths) -> Option<CentralClient> {
    let raw = std::fs::read(serve_file(paths)).ok()?;
    let info: ServeInfo = serde_json::from_slice(&raw).ok()?;
    let base = format!("http://{}", info.addr);
    let ok = ureq::AgentBuilder::new()
        .timeout(Duration::from_millis(400))
        .build()
        .get(&format!("{base}/health"))
        .call()
        .is_ok();
    if !ok {
        return None;
    }
    Some(CentralClient {
        base,
        token: info.token,
    })
}

/// Ensure a central daemon is running, spawning one if needed.
///
/// Returns `None` when auto-spawn is disabled or the daemon does not come up, in
/// which case the caller opens the store directly — correct for a lone command,
/// and the reason a single `devctx projects list` still works with no daemon at
/// all.
pub fn ensure(paths: &CentralPaths) -> Option<CentralClient> {
    if let Some(r) = discover(paths) {
        return Some(r);
    }
    if std::env::var_os("DEVCTX_NO_AUTOSERVE").is_some() {
        return None;
    }
    let Ok(exited) = spawn(paths) else {
        return None;
    };
    // The budget used to be four seconds, on the reasoning that this daemon
    // loads no model and so "appears in well under a second". True on an idle
    // machine, false on a busy one: a full test run has several processes
    // loading embedding models at once, and under that the daemon lost the race
    // and the caller reported it as "could not be started". That was the whole
    // flaky-test story.
    //
    // Waiting longer is only half of it — a daemon that DIED is not going to
    // arrive no matter how long anyone waits, so the loop watches for that too
    // and gives up immediately instead of burning the budget.
    for _ in 0..WAIT_TICKS {
        std::thread::sleep(TICK);
        if let Some(r) = discover(paths) {
            return Some(r);
        }
        if exited.lock().is_ok_and(|e| e.is_some()) {
            return None;
        }
    }
    None
}

/// How long to wait for a freshly spawned central daemon, and in what steps.
///
/// Twenty seconds is not a guess at how long startup takes — startup is fast.
/// It is the margin for a machine so loaded that a process takes seconds just
/// to be scheduled. The loop leaves as soon as the daemon answers, so on an
/// idle machine this costs the same few hundred milliseconds it always did.
const TICK: Duration = Duration::from_millis(100);
const WAIT_TICKS: usize = 200;

/// Why the last spawned daemon did not come up, as far as its log knows.
///
/// `ensure` returning `None` says only "no daemon". The daemon writes the
/// actual reason — a lock it could not take, a port already held, a store whose
/// config no longer matches — to `serve.log` and then exits, and without this
/// that line is never read by anyone. An error that names no cause is an error
/// somebody has to reproduce before they can start working on it.
pub fn spawn_failure_hint(paths: &CentralPaths) -> Option<String> {
    let log = std::fs::read_to_string(paths.dir.join("serve.log")).ok()?;
    let tail: Vec<&str> = log
        .lines()
        .filter(|l| !l.trim().is_empty())
        .rev()
        .take(3)
        .collect();
    if tail.is_empty() {
        return None;
    }
    Some(
        tail.into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join(" / ")
            .chars()
            .take(400)
            .collect(),
    )
}

/// Where a spawned daemon's stderr goes: `serve.log` beside the database,
/// appended to. Falls back to discarding it if the file cannot be opened —
/// losing the log is not a reason to refuse to start the daemon.
fn log_sink(paths: &CentralPaths) -> std::process::Stdio {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths.dir.join("serve.log"))
        .map(std::process::Stdio::from)
        .unwrap_or_else(|_| std::process::Stdio::null())
}

/// Launch `devctx serve --central` detached, with an idle timeout.
///
/// Returns a handle the caller can poll to tell "not up yet" apart from "it is
/// never coming up".
fn spawn(paths: &CentralPaths) -> Result<Arc<Mutex<Option<std::process::ExitStatus>>>> {
    let exe_path =
        std::env::current_exe().map_err(|e| CentralError::Io(e, PathBuf::from("<current exe>")))?;
    let mut cmd = std::process::Command::new(&exe_path);
    cmd.args([
        "serve",
        "--central",
        "--addr",
        &auto_addr(paths),
        "--idle",
        "900",
    ])
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::null())
    // Not `/dev/null`: when the daemon dies on startup — a lock it could not
    // take, a config that no longer matches the store — the caller sees only
    // "could not be started", and the one line explaining why is discarded with
    // it. Appending to a file next to the database keeps that line.
    .stderr(log_sink(paths));
    // The daemon resolves its own paths from the environment, so pass the home
    // through explicitly rather than relying on the parent's cwd.
    cmd.env(crate::HOME_ENV, &paths.dir);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        cmd.process_group(0);
    }
    let child = cmd
        .spawn()
        .map_err(|e| CentralError::Io(e, exe_path.clone()))?;
    // A `Child` that is dropped is never waited on, so the daemon becomes a
    // zombie the moment it exits — and it does exit: on its idle timeout, or
    // immediately when another daemon already owns the database. In a long-lived
    // parent (an MCP server that outlives many of them) those corpses
    // accumulate, one per attempt. Reap it wherever it ends.
    let exited: Arc<Mutex<Option<std::process::ExitStatus>>> = Arc::new(Mutex::new(None));
    let slot = Arc::clone(&exited);
    std::thread::spawn(move || {
        let mut child = child;
        if let Ok(status) = child.wait() {
            if let Ok(mut g) = slot.lock() {
                *g = Some(status);
            }
        }
    });
    Ok(exited)
}

/// Stop the advertised central daemon.
pub fn stop(paths: &CentralPaths) -> Result<()> {
    let path = serve_file(paths);
    let Ok(raw) = std::fs::read(&path) else {
        println!("No central daemon is running.");
        return Ok(());
    };
    let Ok(info) = serde_json::from_slice::<ServeInfo>(&raw) else {
        remove_serve_file(paths);
        return Ok(());
    };
    if let Some(pid) = info.pid {
        let _ = std::process::Command::new("kill")
            .arg(pid.to_string())
            .stderr(std::process::Stdio::null())
            .status();
        println!("Stopped the central daemon (pid {pid}).");
    }
    remove_serve_file(paths);
    Ok(())
}

/// The address a daemon for this home should bind to.
pub fn default_addr(paths: &CentralPaths) -> String {
    auto_addr(paths)
}

/// How long to wait on the central daemon for one call.
///
/// The expensive endpoints — `/remember` above all — load an embedding model
/// and then embed, and neither step reports progress, so the client has nothing
/// to go on but the clock. Sixty seconds was enough on an idle machine and not
/// enough during a full workspace test run, where several processes load models
/// at once; the call surfaced as `timed out reading response`, which reads like
/// a broken daemon rather than a busy one.
///
/// This is a ceiling for a local process, not a latency target. A daemon that
/// died is already caught by the exit watch in `ensure`, so the only thing this
/// bound still protects against is one that hangs — and for that, waiting five
/// minutes before saying so costs nothing anybody notices.
const CALL_TIMEOUT: Duration = Duration::from_secs(300);

impl CentralClient {
    fn agent(&self) -> ureq::Agent {
        ureq::AgentBuilder::new().timeout(CALL_TIMEOUT).build()
    }

    fn auth(&self, req: ureq::Request) -> ureq::Request {
        match &self.token {
            Some(t) => req.set("Authorization", &format!("Bearer {t}")),
            None => req,
        }
    }

    fn get(&self, path: &str) -> Result<Value> {
        let req = self.auth(self.agent().get(&format!("{}{path}", self.base)));
        parse(req.call())
    }

    fn post(&self, path: &str, body: Value) -> Result<Value> {
        let req = self.auth(self.agent().post(&format!("{}{path}", self.base)));
        parse(req.send_json(body))
    }

    fn delete(&self, path: &str) -> Result<Value> {
        let req = self.auth(self.agent().delete(&format!("{}{path}", self.base)));
        parse(req.call())
    }

    // --- typed endpoints ---

    pub fn list(&self, all: bool) -> Result<Vec<Value>> {
        let v = self.get(&format!("/projects?all={all}"))?;
        Ok(v.get("projects")
            .and_then(|p| p.as_array())
            .cloned()
            .unwrap_or_default())
    }

    pub fn add(
        &self,
        path: &Path,
        name: Option<&str>,
        description: &str,
        tags: &str,
        init: bool,
    ) -> Result<Value> {
        self.post(
            "/projects",
            serde_json::json!({
                "path": path.to_string_lossy(),
                "name": name,
                "description": description,
                "tags": tags,
                "init": init,
            }),
        )
    }

    /// Report an indexing outcome so `projects list` reflects reality.
    pub fn record_index(
        &self,
        path: &str,
        commit: &str,
        branch: &str,
        files: i64,
        symbols: i64,
        chunks: i64,
    ) -> Result<Value> {
        self.post(
            "/projects/indexed",
            serde_json::json!({
                "path": path, "commit": commit, "branch": branch,
                "files": files, "symbols": symbols, "chunks": chunks,
            }),
        )
    }

    pub fn show(&self, name: &str) -> Result<Value> {
        self.get(&format!("/projects/{}", urlencode(name)))
    }

    pub fn refresh(&self, name: &str) -> Result<Value> {
        self.post(
            &format!("/projects/{}/refresh", urlencode(name)),
            Value::Null,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn remember(
        &self,
        content: &str,
        title: &str,
        memory_type: &str,
        topic: &str,
        tags: &str,
        project: &str,
        repo: &str,
        branch: &str,
        group: &str,
        files: &str,
    ) -> Result<Value> {
        self.post(
            "/remember",
            serde_json::json!({
                "content": content, "title": title, "type": memory_type,
                "topic": topic, "tags": tags,
                "project": project, "repo": repo, "branch": branch,
                "group": group, "files": files,
            }),
        )
    }

    pub fn recall(&self, query: &str, limit: usize, repo: Option<&str>) -> Result<Vec<Value>> {
        self.recall_scoped(query, limit, repo, None)
    }

    /// Recall from one group's space when `group` is set, else the global one.
    pub fn recall_scoped(
        &self,
        query: &str,
        limit: usize,
        repo: Option<&str>,
        group: Option<&str>,
    ) -> Result<Vec<Value>> {
        let v = self.post(
            "/recall",
            serde_json::json!({ "query": query, "limit": limit, "repo": repo, "group": group }),
        )?;
        Ok(v.get("memories")
            .and_then(|m| m.as_array())
            .cloned()
            .unwrap_or_default())
    }

    /// The most recently updated shared memories, without a query.
    pub fn recent_memories(&self, limit: usize) -> Result<Vec<Value>> {
        let v = self.get(&format!("/memories?limit={limit}"))?;
        Ok(v.get("memories")
            .and_then(|m| m.as_array())
            .cloned()
            .unwrap_or_default())
    }

    /// Permanently delete a shared memory. `false` means it was not there.
    pub fn forget_memory(&self, id: &str) -> Result<bool> {
        let v = self.delete(&format!("/memory/{}", urlencode(id)))?;
        Ok(v.get("forgotten")
            .and_then(|f| f.as_bool())
            .unwrap_or(false))
    }

    /// Every live shared memory, for a backfill sweep.
    ///
    /// Unlike `recent_memories` this takes no limit: a sweep that silently
    /// stopped at the newest twenty would report success having linked a
    /// fraction of the corpus.
    pub fn all_shared_memories(&self) -> Result<Vec<Value>> {
        let v = self.get("/memories?limit=0")?;
        Ok(v.get("memories")
            .and_then(|m| m.as_array())
            .cloned()
            .unwrap_or_default())
    }

    /// Fetch specific memories by id, skipping any that are gone.
    ///
    /// Used to resolve memory↔graph links: the junction row is written next to
    /// the graph in a project store, and the memory it names may live here.
    pub fn memories_by_id(&self, ids: &[String]) -> Result<Vec<Value>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let v = self.post("/memories/by-id", serde_json::json!({ "ids": ids }))?;
        Ok(v.get("memories")
            .and_then(|m| m.as_array())
            .cloned()
            .unwrap_or_default())
    }

    /// Shared memories mentioning `label` literally — the fallback for a symbol
    /// the junction never linked.
    pub fn memories_mentioning(&self, label: &str, limit: usize) -> Result<Vec<Value>> {
        let v = self.post(
            "/memories/mentioning",
            serde_json::json!({ "label": label, "limit": limit }),
        )?;
        Ok(v.get("memories")
            .and_then(|m| m.as_array())
            .cloned()
            .unwrap_or_default())
    }

    pub fn remove(&self, name: &str, deactivate: bool) -> Result<Value> {
        self.delete(&format!(
            "/projects/{}?deactivate={deactivate}",
            urlencode(name)
        ))
    }
}

/// Turn a ureq result into JSON, surfacing the server's `error` field so a
/// routed failure reads the same as a local one.
fn parse(r: std::result::Result<ureq::Response, ureq::Error>) -> Result<Value> {
    match r {
        Ok(resp) => resp
            .into_json()
            .map_err(|e| CentralError::Request(e.to_string())),
        Err(ureq::Error::Status(_, resp)) => {
            let body: Value = resp.into_json().unwrap_or(Value::Null);
            let msg = body
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("central store request failed");
            Err(CentralError::Request(msg.to_string()))
        }
        Err(e) => Err(CentralError::Request(e.to_string())),
    }
}

/// Minimal percent-encoding for a path segment.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
