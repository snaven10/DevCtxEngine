//! Tool backend: either owns the DB locally (`AppState` + `do_*`) or routes each
//! tool call to a shared server over HTTP. Routing lets many MCP sessions plus
//! the web/CLI/TUI share a single DB owner without DuckDB lock conflicts.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use crate::state::{
    do_impact, do_index, do_index_status, do_list_projects, do_memory_stats, do_read_file,
    do_recall_scoped, do_references, do_remember, do_remember_global, do_routes_for_handler,
    do_search, do_search_routes, do_summarize, parse_mode, AppState,
};

/// Connection to a shared server the MCP routes through.
pub struct ServerConn {
    /// Base URL, e.g. `http://127.0.0.1:20111`.
    pub base: String,
    /// Bearer token, if the server requires one.
    pub token: Option<String>,
}

/// A thin blocking HTTP client for the shared server's endpoints.
pub struct RemoteClient {
    base: String,
    token: Option<String>,
}

impl RemoteClient {
    fn agent(&self) -> ureq::Agent {
        // Generous: a routed `index` can run for many minutes on the server.
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

    fn get(&self, path: &str) -> Result<String, String> {
        read(
            self.auth(self.agent().get(&format!("{}{path}", self.base)))
                .call(),
        )
    }

    fn post(&self, path: &str, body: Value) -> Result<String, String> {
        read(
            self.auth(self.agent().post(&format!("{}{path}", self.base)))
                .send_json(body),
        )
    }
}

/// Read a response body, surfacing the server's own `error` message on a
/// failure status. Without this a routed tool call reports only "status code
/// 500" and the actual cause — which the server did explain — is thrown away.
fn read(r: Result<ureq::Response, ureq::Error>) -> Result<String, String> {
    match r {
        Ok(resp) => resp.into_string().map_err(|e| e.to_string()),
        Err(ureq::Error::Status(code, resp)) => {
            let body = resp.into_string().unwrap_or_default();
            let msg = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
                .unwrap_or(body);
            Err(if msg.is_empty() {
                format!("server returned status {code}")
            } else {
                msg
            })
        }
        Err(e) => Err(e.to_string()),
    }
}

/// Where the MCP tools run: locally (owns the DB) or via a shared server.
pub enum Backend {
    Local(Arc<AppState>),
    Remote(RemoteClient),
}

impl Backend {
    pub fn local(state: Arc<AppState>) -> Self {
        Backend::Local(state)
    }

    pub fn remote(conn: ServerConn) -> Self {
        Backend::Remote(RemoteClient {
            base: conn.base,
            token: conn.token,
        })
    }

    pub fn search(
        &self,
        query: &str,
        limit: usize,
        language: Option<String>,
        mode: Option<String>,
    ) -> Result<String, String> {
        match self {
            Backend::Local(s) => do_search(s, query, limit, language, parse_mode(mode.as_deref())),
            Backend::Remote(r) => r.post(
                "/search",
                json!({ "query": query, "limit": limit, "language": language,
                        "mode": mode.unwrap_or_else(|| "vector".into()) }),
            ),
        }
    }

    pub fn read_file(
        &self,
        path: &str,
        start: Option<usize>,
        end: Option<usize>,
    ) -> Result<String, String> {
        match self {
            Backend::Local(s) => do_read_file(s, path, start, end),
            Backend::Remote(r) => r.post(
                "/read_file",
                json!({ "path": path, "start_line": start, "end_line": end }),
            ),
        }
    }

    pub fn index(&self, full: bool) -> Result<String, String> {
        match self {
            Backend::Local(s) => do_index(s, full),
            Backend::Remote(r) => r.post("/index", json!({ "full": full })),
        }
    }

    pub fn index_status(&self) -> Result<String, String> {
        match self {
            Backend::Local(s) => do_index_status(s),
            Backend::Remote(r) => r.get("/status"),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn remember(
        &self,
        content: String,
        title: String,
        memory_type: String,
        topic: String,
        tags: String,
        scope: String,
    ) -> Result<String, String> {
        let global = devctx_memory::is_global(&scope);
        match self {
            Backend::Local(s) if global => {
                do_remember_global(s, &content, &title, &memory_type, &topic, &tags)
            }
            Backend::Local(s) => do_remember(s, content, title, memory_type, topic, tags),
            Backend::Remote(r) => r.post(
                "/remember",
                json!({ "content": content, "title": title, "type": memory_type,
                        "topic": topic, "tags": tags, "scope": scope }),
            ),
        }
    }

    pub fn recall(
        &self,
        query: &str,
        limit: usize,
        scope: &str,
        repo: Option<&str>,
    ) -> Result<String, String> {
        match self {
            Backend::Local(s) => do_recall_scoped(s, query, limit, scope, repo),
            Backend::Remote(r) => r.post(
                "/recall",
                json!({ "query": query, "limit": limit, "scope": scope, "repo": repo }),
            ),
        }
    }

    /// The project registry. Independent of which project this session is in —
    /// both backends ask the central daemon, since neither may open its database.
    pub fn list_projects(&self, include_inactive: bool) -> Result<String, String> {
        do_list_projects(include_inactive)
    }

    pub fn memory_stats(&self) -> Result<String, String> {
        match self {
            Backend::Local(s) => do_memory_stats(s),
            Backend::Remote(r) => r.get("/memory/stats"),
        }
    }

    pub fn impact(&self, symbol: &str, depth: usize) -> Result<String, String> {
        match self {
            Backend::Local(s) => do_impact(s, symbol, depth),
            Backend::Remote(r) => r.get(&format!("/impact/{}?depth={depth}", urlencode(symbol))),
        }
    }

    pub fn references(&self, symbol: &str) -> Result<String, String> {
        match self {
            Backend::Local(s) => do_references(s, symbol),
            Backend::Remote(r) => r.get(&format!("/references/{}", urlencode(symbol))),
        }
    }

    pub fn search_routes(
        &self,
        method: Option<String>,
        path: Option<String>,
    ) -> Result<String, String> {
        match self {
            Backend::Local(s) => do_search_routes(s, method, path),
            Backend::Remote(r) => {
                let mut q = Vec::new();
                if let Some(m) = &method {
                    q.push(format!("method={}", urlencode(m)));
                }
                if let Some(p) = &path {
                    q.push(format!("path={}", urlencode(p)));
                }
                let qs = if q.is_empty() {
                    String::new()
                } else {
                    format!("?{}", q.join("&"))
                };
                r.get(&format!("/routes{qs}"))
            }
        }
    }

    pub fn routes_for_handler(&self, handler: &str) -> Result<String, String> {
        match self {
            Backend::Local(s) => do_routes_for_handler(s, handler),
            Backend::Remote(r) => r.get(&format!("/routes/handler/{}", urlencode(handler))),
        }
    }

    pub fn summarize(
        &self,
        content: &str,
        query: Option<String>,
        max_tokens: usize,
    ) -> Result<String, String> {
        match self {
            Backend::Local(s) => do_summarize(s, content, query, max_tokens),
            Backend::Remote(r) => r.post(
                "/summarize",
                json!({ "content": content, "query": query, "max_tokens": max_tokens }),
            ),
        }
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
