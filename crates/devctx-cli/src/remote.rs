//! Client side of "server mode": discover a running `devctx serve` that owns
//! the DuckDB file and route commands to it over HTTP, so no second process
//! ever takes a conflicting file lock. When no server is advertised (or it is
//! unreachable) the caller falls back to opening the store directly.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use devctx_core::config::ProjectConfig;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Advertised by `devctx serve`, next to the database file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServeInfo {
    /// `host:port` the server is bound to.
    pub addr: String,
    /// Bearer token required by the server (if any).
    #[serde(default)]
    pub token: Option<String>,
    /// Owner process id (informational).
    #[serde(default)]
    pub pid: Option<u32>,
}

/// A snapshot of the indexing run happening inside the server.
#[derive(Debug, Default, Deserialize)]
pub struct IndexProgress {
    /// Whether a run is in flight right now.
    pub running: bool,
    /// Which run these counts belong to. Defaults to zero against a server
    /// built before the field existed, which reads as "always the same run" —
    /// the same behaviour those servers already had.
    #[serde(default)]
    pub run: u64,
    /// Changes the run expects to process.
    pub total: usize,
    /// Changes it has started on.
    pub done: usize,
    /// The file it reached last.
    pub file: String,
}

/// Path of the discovery file (`serve.json`) next to the DB.
pub fn serve_file(cfg: &ProjectConfig) -> PathBuf {
    let db = cfg.db_path();
    let dir = db.parent().unwrap_or_else(|| Path::new("."));
    dir.join("serve.json")
}

/// Write the discovery file so clients can find this server.
pub fn write_serve_file(cfg: &ProjectConfig, addr: SocketAddr, token: Option<&str>) -> Result<()> {
    let info = ServeInfo {
        addr: addr.to_string(),
        token: token.map(str::to_string),
        pid: Some(std::process::id()),
    };
    let path = serve_file(cfg);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&path, serde_json::to_vec_pretty(&info)?)
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Remove the discovery file (best effort).
///
/// Unconditional, so only for callers that have established the advertised
/// server is gone — [`reclaim_db`] and [`stop_server`], which kill it first and
/// wait for it to exit. A server tidying up after *itself* must use
/// [`remove_own_serve_file`].
pub fn remove_serve_file(cfg: &ProjectConfig) {
    let _ = std::fs::remove_file(serve_file(cfg));
}

/// Remove the discovery file only while it still advertises this process.
///
/// A server that fails to start — the port taken, the database held by the
/// server already running — used to delete the file on its way out, and the
/// file it deleted belonged to that healthy server. The healthy one kept
/// running, advertised nowhere, so every later command failed to discover it,
/// fell back to opening the database directly, and hit the lock it was holding.
/// The CLI was then unusable until someone found the process and killed it by
/// pid, with nothing on screen connecting the two.
///
/// Checking the pid makes the tidy-up idempotent under a race: whoever owns the
/// file removes it, and a loser removes nothing.
pub fn remove_own_serve_file(cfg: &ProjectConfig) {
    let path = serve_file(cfg);
    let ours = std::fs::read(&path)
        .ok()
        .and_then(|raw| serde_json::from_slice::<ServeInfo>(&raw).ok())
        .and_then(|info| info.pid)
        .is_some_and(|pid| pid == std::process::id());
    if ours {
        let _ = std::fs::remove_file(&path);
    }
}

/// A reachable server we can route requests to.
#[derive(Clone)]
pub struct Remote {
    base: String,
    token: Option<String>,
}

/// A deterministic loopback address per project, so auto-spawned servers for
/// different projects don't collide on one port.
fn auto_addr(cfg: &ProjectConfig) -> String {
    // FNV-1a over the project path → a stable high port.
    let mut h: u64 = 0xcbf29ce484222325;
    for b in cfg.project.path.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    let port = 20000 + (h % 40000) as u16;
    format!("127.0.0.1:{port}")
}

/// Ensure a server is running for this project, auto-spawning one in the
/// background if needed, and return a client to it. Falls back to `None` (run
/// locally) when auto-spawn is disabled (`DEVCTX_NO_AUTOSERVE`) or the server
/// does not come up in time. Every DB command routes through this so the server
/// is the single owner of the DuckDB file — no command ever fights the lock
/// (e.g. querying while an `index` runs), and the embedding model stays warm.
pub fn ensure(cfg: &ProjectConfig) -> Option<Remote> {
    if let Some(r) = discover(cfg) {
        return Some(r);
    }
    if std::env::var_os("DEVCTX_NO_AUTOSERVE").is_some() {
        return None;
    }
    if spawn_server(cfg).is_err() {
        return None;
    }
    eprintln!("· started background server (devctx serve); it stays warm for later commands");
    // Poll until healthy (model load can take a few seconds on a cold start).
    for _ in 0..200 {
        std::thread::sleep(Duration::from_millis(300));
        if let Some(r) = discover(cfg) {
            return Some(r);
        }
    }
    None
}

/// Launch `devctx serve` detached in the background, with an idle timeout so it
/// eventually exits on its own.
fn spawn_server(cfg: &ProjectConfig) -> Result<()> {
    let exe = std::env::current_exe().context("locating the devctx binary")?;
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["serve", "--addr", &auto_addr(cfg), "--idle", "900"])
        .current_dir(&cfg.project.path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // Detach from the parent's process group so it survives the CLI exiting.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        cmd.process_group(0);
    }
    cmd.spawn().context("spawning devctx serve")?;
    Ok(())
}

/// The base URL of a reachable running server for this project, if any.
pub fn running_server_url(cfg: &ProjectConfig) -> Option<String> {
    discover(cfg).map(|r| r.base)
}

/// Whether a process with `pid` is alive (`kill -0`).
fn pid_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Send SIGTERM to `pid`, quietly (no "No such process" noise on a race).
fn kill_pid(pid: u32) {
    let _ = std::process::Command::new("kill")
        .arg(pid.to_string())
        .stderr(std::process::Stdio::null())
        .status();
}

/// Stop any background server holding this project's DB so the caller can take
/// exclusive ownership (used by interactive owners like the TUI). Returns
/// whether a server was stopped. Best-effort and quiet.
pub fn reclaim_db(cfg: &ProjectConfig) -> bool {
    let Ok(raw) = std::fs::read(serve_file(cfg)) else {
        return false;
    };
    let Ok(info) = serde_json::from_slice::<ServeInfo>(&raw) else {
        return false;
    };
    let Some(pid) = info.pid else {
        return false;
    };
    if !pid_alive(pid) {
        remove_serve_file(cfg);
        return false;
    }
    kill_pid(pid);
    // Wait for it to exit and release the file lock.
    for _ in 0..40 {
        if !pid_alive(pid) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    remove_serve_file(cfg);
    true
}

/// Stop the server advertised for this project (SIGTERM its pid, drop the file).
pub fn stop_server(cfg: &ProjectConfig) -> Result<()> {
    let path = serve_file(cfg);
    let Ok(raw) = std::fs::read(&path) else {
        println!("No server is registered for this project.");
        return Ok(());
    };
    let info: ServeInfo = serde_json::from_slice(&raw).context("parsing serve.json")?;
    if let Some(pid) = info.pid {
        kill_pid(pid);
        // Wait for it to actually exit. Returning while it still holds the DuckDB
        // file means the next command spawns a server that cannot open the
        // database, and then waits out the full auto-spawn poll — a measured 61
        // seconds — before giving up and falling back to a local open.
        if !wait_for_exit(pid) {
            eprintln!("· server {pid} did not exit promptly; the next command may be slow");
        }
        println!("Stopped server (pid {pid}).");
    }
    remove_serve_file(cfg);
    Ok(())
}

/// Wait for a process to disappear, up to a few seconds. Returns whether it did.
fn wait_for_exit(pid: u32) -> bool {
    for _ in 0..50 {
        if !pid_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// Discover a running server for this project and confirm it is reachable.
/// Returns `None` when there is no server (so the caller runs locally).
pub fn discover(cfg: &ProjectConfig) -> Option<Remote> {
    let raw = std::fs::read(serve_file(cfg)).ok()?;
    let info: ServeInfo = serde_json::from_slice(&raw).ok()?;
    let base = format!("http://{}", info.addr);
    // Health-check with a short timeout so a stale file doesn't hang the CLI.
    let ok = ureq::AgentBuilder::new()
        .timeout(Duration::from_millis(400))
        .build()
        .get(&format!("{base}/health"))
        .call()
        .is_ok();
    if !ok {
        return None;
    }
    Some(Remote {
        base,
        token: info.token,
    })
}

impl Remote {
    /// Consume into `(base_url, token)` — for handing the connection to the TUI.
    pub fn into_parts(self) -> (String, Option<String>) {
        (self.base, self.token)
    }

    fn agent(&self) -> ureq::Agent {
        // Generous overall timeout: a routed `index` of a large repo (or a slow
        // model) can run for many minutes on the server before responding.
        ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(3600))
            .build()
    }

    fn auth(&self, req: ureq::Request) -> ureq::Request {
        match &self.token {
            Some(t) => req.set("Authorization", &format!("Bearer {t}")),
            None => req,
        }
    }

    fn get(&self, path: &str) -> Result<String> {
        let req = self.auth(self.agent().get(&format!("{}{path}", self.base)));
        read(req.call())
    }

    fn post(&self, path: &str, body: Value) -> Result<String> {
        let req = self.auth(self.agent().post(&format!("{}{path}", self.base)));
        read(req.send_json(body))
    }

    // --- typed endpoints (return the server's JSON string) ---

    pub fn status(&self) -> Result<String> {
        self.get("/status")
    }

    pub fn index(&self, full: bool, branch: Option<&str>) -> Result<String> {
        self.post(
            "/index",
            serde_json::json!({ "full": full, "branch": branch }),
        )
    }

    /// Index an explicit path list (what the watcher sends).
    pub fn index_paths(&self, paths: &[String]) -> Result<String> {
        self.post("/index", serde_json::json!({ "paths": paths }))
    }

    /// How far the server's current indexing run has got.
    ///
    /// Builds its own short-timeout agent rather than reusing [`Self::agent`]:
    /// that one waits an hour, which is right for the index request itself and
    /// wrong for a poll behind a progress bar. A progress call that hangs
    /// should give up quickly and let the bar fall back, not freeze with no
    /// explanation.
    pub fn index_progress(&self) -> Result<IndexProgress> {
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(2))
            .build();
        let req = self.auth(agent.get(&format!("{}/index/progress", self.base)));
        let body = read(req.call())?;
        serde_json::from_str(&body).context("parsing the server's index progress")
    }

    pub fn search(
        &self,
        query: &str,
        limit: usize,
        language: Option<&str>,
        mode: &str,
        rerank: bool,
    ) -> Result<String> {
        self.post(
            "/search",
            serde_json::json!({
                "query": query, "limit": limit, "language": language,
                "mode": mode, "rerank": rerank,
            }),
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
        files: &str,
        scope: &str,
    ) -> Result<String> {
        self.post(
            "/remember",
            serde_json::json!({
                "content": content, "title": title, "type": memory_type,
                "topic": topic, "tags": tags, "files": files, "scope": scope,
            }),
        )
    }

    /// Recall this project's OWN memories.
    ///
    /// The scope is sent explicitly: the endpoint defaults to `all`, and the
    /// caller already queries the group and global tiers itself. Leaving it out
    /// asked the server for everything and then fused it with those tiers a
    /// second time.
    pub fn recall(&self, query: &str, limit: usize) -> Result<String> {
        self.post(
            "/recall",
            serde_json::json!({ "query": query, "limit": limit, "scope": "local" }),
        )
    }

    pub fn memory_stats(&self) -> Result<String> {
        self.get("/memory/stats")
    }

    pub fn impact(&self, symbol: &str, depth: usize) -> Result<String> {
        self.get(&format!("/impact/{}?depth={depth}", urlencode(symbol)))
    }

    pub fn backfill_links(&self, dry_run: bool, from_text: bool) -> Result<String> {
        self.post(
            "/memories/backfill-links",
            serde_json::json!({ "dry_run": dry_run, "from_text": from_text }),
        )
    }

    pub fn read_symbol(&self, name: &str, limit: usize) -> Result<String> {
        self.get(&format!("/symbol/{}?limit={limit}", urlencode(name)))
    }

    pub fn build_context(
        &self,
        query: &str,
        max_tokens: usize,
        include_memories: bool,
    ) -> Result<String> {
        self.post(
            "/context",
            serde_json::json!({ "query": query, "max_tokens": max_tokens,
                                "include_memories": include_memories }),
        )
    }

    pub fn routes(&self, method: Option<&str>, path: Option<&str>) -> Result<String> {
        let mut q = Vec::new();
        if let Some(m) = method {
            q.push(format!("method={}", urlencode(m)));
        }
        if let Some(p) = path {
            q.push(format!("path={}", urlencode(p)));
        }
        let qs = if q.is_empty() {
            String::new()
        } else {
            format!("?{}", q.join("&"))
        };
        self.get(&format!("/routes{qs}"))
    }

    pub fn summarize(
        &self,
        content: &str,
        query: Option<&str>,
        max_tokens: usize,
    ) -> Result<String> {
        self.post(
            "/summarize",
            serde_json::json!({ "content": content, "query": query, "max_tokens": max_tokens }),
        )
    }
}

/// Read a response body, surfacing the server's own message on a failure status.
///
/// Without this a routed command reports only `status code 500` and throws away
/// the explanation the server already wrote — which is exactly the information
/// needed to act on it.
fn read(r: std::result::Result<ureq::Response, ureq::Error>) -> Result<String> {
    match r {
        Ok(resp) => Ok(resp.into_string()?),
        Err(ureq::Error::Status(code, resp)) => {
            let body = resp.into_string().unwrap_or_default();
            let msg = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
                .unwrap_or(body);
            if msg.trim().is_empty() {
                anyhow::bail!("the server returned status {code}");
            }
            anyhow::bail!("{}", msg.trim())
        }
        Err(e) => Err(anyhow::anyhow!(e.to_string())),
    }
}

/// Minimal percent-encoding for a path/query segment.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_at(dir: &Path) -> ProjectConfig {
        let mut c = ProjectConfig::default();
        c.project.path = dir.to_string_lossy().to_string();
        c.storage.db_path = dir.join("index.duckdb").to_string_lossy().to_string();
        c
    }

    fn advertise(cfg: &ProjectConfig, pid: u32) {
        let info = ServeInfo {
            addr: "127.0.0.1:1".into(),
            token: None,
            pid: Some(pid),
        };
        let path = serve_file(cfg);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, serde_json::to_vec(&info).unwrap()).unwrap();
    }

    /// The failure this exists to prevent: a server that could not start used to
    /// delete the discovery file of the healthy one that beat it to the port.
    /// The healthy server kept running, advertised nowhere, and every later
    /// command fell back to opening the database it was holding.
    #[test]
    fn a_losing_server_does_not_delete_the_winners_advertisement() {
        let dir = std::env::temp_dir().join(format!("devctx_serve_race_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = cfg_at(&dir);

        // Someone else owns the file: a pid that is not ours.
        advertise(&cfg, std::process::id() + 1);
        remove_own_serve_file(&cfg);
        assert!(
            serve_file(&cfg).exists(),
            "another process's advertisement must survive"
        );

        // Our own, on the other hand, is ours to clean up.
        advertise(&cfg, std::process::id());
        remove_own_serve_file(&cfg);
        assert!(!serve_file(&cfg).exists(), "our own must go");

        // And with no file at all it is a no-op rather than an error.
        remove_own_serve_file(&cfg);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
