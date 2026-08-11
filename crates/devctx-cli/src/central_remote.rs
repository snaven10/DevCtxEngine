//! Client side of the central daemon: discover a running `devctx serve
//! --central`, auto-spawn one when absent, and route registry calls to it.
//!
//! This mirrors [`crate::remote`], with one difference that matters: a project
//! server is per-repository, while the central store is a singleton. Opening it
//! from two processes at once does not degrade — it fails outright, since DuckDB
//! permits a single writing process per file. So unlike the project case, where
//! falling back to a direct open is harmless, here the daemon is what keeps
//! concurrent commands from knocking each other over.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use devctx_central::CentralPaths;
use serde_json::Value;

use crate::remote::ServeInfo;

/// A reachable central daemon.
pub struct CentralRemote {
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
    std::fs::write(&path, serde_json::to_vec_pretty(&info)?)
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Remove the discovery file (best effort).
pub fn remove_serve_file(paths: &CentralPaths) {
    let _ = std::fs::remove_file(serve_file(paths));
}

/// Discover a running central daemon and confirm it answers.
pub fn discover(paths: &CentralPaths) -> Option<CentralRemote> {
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
    Some(CentralRemote {
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
pub fn ensure(paths: &CentralPaths) -> Option<CentralRemote> {
    if let Some(r) = discover(paths) {
        return Some(r);
    }
    if std::env::var_os("DEVCTX_NO_AUTOSERVE").is_some() {
        return None;
    }
    if spawn(paths).is_err() {
        return None;
    }
    // No model to load here, so a healthy daemon appears in well under a second.
    for _ in 0..40 {
        std::thread::sleep(Duration::from_millis(100));
        if let Some(r) = discover(paths) {
            return Some(r);
        }
    }
    None
}

/// Launch `devctx serve --central` detached, with an idle timeout.
fn spawn(paths: &CentralPaths) -> Result<()> {
    let exe = std::env::current_exe().context("locating the devctx binary")?;
    let mut cmd = std::process::Command::new(exe);
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
    .stderr(std::process::Stdio::null());
    // The daemon resolves its own paths from the environment, so pass the home
    // through explicitly rather than relying on the parent's cwd.
    cmd.env(devctx_central::HOME_ENV, &paths.dir);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        cmd.process_group(0);
    }
    cmd.spawn().context("spawning devctx serve --central")?;
    Ok(())
}

/// Stop the advertised central daemon.
pub fn stop(paths: &CentralPaths) -> Result<()> {
    let path = serve_file(paths);
    let Ok(raw) = std::fs::read(&path) else {
        println!("No central daemon is running.");
        return Ok(());
    };
    let info: ServeInfo = serde_json::from_slice(&raw).context("parsing serve.json")?;
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

impl CentralRemote {
    fn agent(&self) -> ureq::Agent {
        ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(60))
            .build()
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
    ) -> Result<Value> {
        self.post(
            "/remember",
            serde_json::json!({
                "content": content, "title": title, "type": memory_type,
                "topic": topic, "tags": tags,
                "project": project, "repo": repo, "branch": branch,
            }),
        )
    }

    pub fn recall(&self, query: &str, limit: usize, repo: Option<&str>) -> Result<Vec<Value>> {
        let v = self.post(
            "/recall",
            serde_json::json!({ "query": query, "limit": limit, "repo": repo }),
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
        Ok(resp) => Ok(resp.into_json()?),
        Err(ureq::Error::Status(_, resp)) => {
            let body: Value = resp.into_json().unwrap_or(Value::Null);
            let msg = body
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("central store request failed");
            Err(anyhow::anyhow!(msg.to_string()))
        }
        Err(e) => Err(anyhow::anyhow!(e.to_string())),
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
