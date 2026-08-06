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
pub fn remove_serve_file(cfg: &ProjectConfig) {
    let _ = std::fs::remove_file(serve_file(cfg));
}

/// A reachable server we can route requests to.
pub struct Remote {
    base: String,
    token: Option<String>,
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
    fn agent(&self) -> ureq::Agent {
        ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(600))
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
        Ok(req.call().map_err(box_err)?.into_string()?)
    }

    fn post(&self, path: &str, body: Value) -> Result<String> {
        let req = self.auth(self.agent().post(&format!("{}{path}", self.base)));
        Ok(req.send_json(body).map_err(box_err)?.into_string()?)
    }

    // --- typed endpoints (return the server's JSON string) ---

    pub fn status(&self) -> Result<String> {
        self.get("/status")
    }

    pub fn index(&self, full: bool) -> Result<String> {
        self.post("/index", serde_json::json!({ "full": full }))
    }

    pub fn search(
        &self,
        query: &str,
        limit: usize,
        language: Option<&str>,
        mode: &str,
    ) -> Result<String> {
        self.post(
            "/search",
            serde_json::json!({
                "query": query, "limit": limit, "language": language, "mode": mode,
            }),
        )
    }

    pub fn remember(
        &self,
        content: &str,
        title: &str,
        memory_type: &str,
        topic: &str,
        tags: &str,
    ) -> Result<String> {
        self.post(
            "/remember",
            serde_json::json!({
                "content": content, "title": title, "type": memory_type,
                "topic": topic, "tags": tags,
            }),
        )
    }

    pub fn recall(&self, query: &str, limit: usize) -> Result<String> {
        self.post(
            "/recall",
            serde_json::json!({ "query": query, "limit": limit }),
        )
    }

    pub fn memory_stats(&self) -> Result<String> {
        self.get("/memory/stats")
    }

    pub fn impact(&self, symbol: &str, depth: usize) -> Result<String> {
        self.get(&format!("/impact/{}?depth={depth}", urlencode(symbol)))
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

/// ureq errors aren't `Send + Sync + 'static` friendly for anyhow directly; flatten.
fn box_err(e: ureq::Error) -> anyhow::Error {
    anyhow::anyhow!(e.to_string())
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
