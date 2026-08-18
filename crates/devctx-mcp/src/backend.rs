//! Tool backend: either owns the DB locally (`AppState` + `do_*`) or routes each
//! tool call to a shared server over HTTP. Routing lets many MCP sessions plus
//! the web/CLI/TUI share a single DB owner without DuckDB lock conflicts.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use crate::state::{
    do_backfill_links, do_build_context, do_impact, do_index, do_index_status, do_list_projects,
    do_memories_by_file, do_memories_by_symbol, do_memory_context, do_memory_forget,
    do_memory_move, do_memory_refs, do_memory_stats, do_read_file, do_read_symbol,
    do_recall_scoped, do_references, do_remember, do_remember_shared, do_routes_for_handler,
    do_search, do_search_project, do_search_routes, do_summarize, parse_mode, AppState,
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
    /// Routed to a shared server. The project's identity travels with it:
    /// the server owns the database, but *which* project this session is in —
    /// and the group it belongs to — is what decides where a memory is saved,
    /// and the HTTP client alone cannot answer it.
    Remote(RemoteClient, ProjectIdentity),
}

/// The bound project as the agent needs to see it.
#[derive(Clone, Default)]
pub struct ProjectIdentity {
    pub name: String,
    pub group: String,
}

impl Backend {
    pub fn local(state: Arc<AppState>) -> Self {
        Backend::Local(state)
    }

    pub fn remote(conn: ServerConn, identity: ProjectIdentity) -> Self {
        Backend::Remote(
            RemoteClient {
                base: conn.base,
                token: conn.token,
            },
            identity,
        )
    }

    pub fn search(
        &self,
        query: &str,
        limit: usize,
        language: Option<String>,
        mode: Option<String>,
        rerank: bool,
    ) -> Result<String, String> {
        match self {
            Backend::Local(s) => do_search(
                s,
                query,
                limit,
                language,
                parse_mode(mode.as_deref()),
                rerank,
            ),
            Backend::Remote(r, _) => r.post(
                "/search",
                json!({ "query": query, "limit": limit, "language": language,
                        "mode": mode.unwrap_or_else(|| "vector".into()), "rerank": rerank }),
            ),
        }
    }

    /// Search another registered project. Independent of this session's project,
    /// so both backends take the same path.
    pub fn search_project(
        &self,
        project: &str,
        query: &str,
        limit: usize,
        language: Option<String>,
        mode: Option<String>,
    ) -> Result<String, String> {
        do_search_project(
            project,
            query,
            limit,
            language,
            mode.as_deref().unwrap_or("vector"),
        )
    }

    pub fn read_file(
        &self,
        path: &str,
        start: Option<usize>,
        end: Option<usize>,
    ) -> Result<String, String> {
        match self {
            Backend::Local(s) => do_read_file(s, path, start, end),
            Backend::Remote(r, _) => r.post(
                "/read_file",
                json!({ "path": path, "start_line": start, "end_line": end }),
            ),
        }
    }

    pub fn index(&self, full: bool) -> Result<String, String> {
        match self {
            Backend::Local(s) => do_index(s, full),
            Backend::Remote(r, _) => r.post("/index", json!({ "full": full })),
        }
    }

    pub fn index_status(&self) -> Result<String, String> {
        match self {
            Backend::Local(s) => do_index_status(s),
            Backend::Remote(r, _) => r.get("/status"),
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
        files: String,
    ) -> Result<String, String> {
        // `group` routes to the central store like `global` does, but into the
        // space of the product this repository belongs to.
        let shared = devctx_memory::is_global(&scope) || devctx_memory::is_group(&scope);
        match self {
            Backend::Local(s) if shared => {
                let group = if devctx_memory::is_group(&scope) {
                    s.group_name()
                } else {
                    String::new()
                };
                do_remember_shared(
                    s,
                    &content,
                    &title,
                    &memory_type,
                    &topic,
                    &tags,
                    &group,
                    &files,
                )
            }
            Backend::Local(s) => do_remember(s, content, title, memory_type, topic, tags, files),
            Backend::Remote(r, _) => r.post(
                "/remember",
                json!({ "content": content, "title": title, "type": memory_type,
                        "topic": topic, "tags": tags, "scope": scope, "files": files }),
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
            Backend::Remote(r, _) => r.post(
                "/recall",
                json!({ "query": query, "limit": limit, "scope": scope, "repo": repo }),
            ),
        }
    }

    /// The project registry. Independent of which project this session is in —
    /// both backends ask the central daemon, since neither may open its database.
    pub fn list_projects(&self, include_inactive: bool) -> Result<String, String> {
        // The bound project travels with the answer so the agent can see which
        // group it is in — the fact that decides where a memory belongs.
        match self {
            Backend::Local(s) => {
                do_list_projects(Some(&s.project()), &s.group_name(), include_inactive)
            }
            Backend::Remote(_, id) => do_list_projects(Some(&id.name), &id.group, include_inactive),
        }
    }

    pub fn memory_stats(&self) -> Result<String, String> {
        match self {
            Backend::Local(s) => do_memory_stats(s),
            Backend::Remote(r, _) => r.get("/memory/stats"),
        }
    }

    pub fn impact(&self, symbol: &str, depth: usize) -> Result<String, String> {
        match self {
            Backend::Local(s) => do_impact(s, symbol, depth),
            Backend::Remote(r, _) => r.get(&format!("/impact/{}?depth={depth}", urlencode(symbol))),
        }
    }

    /// Delete one memory for good, wherever it lives.
    pub fn memory_forget(&self, id: &str) -> Result<String, String> {
        match self {
            Backend::Local(s) => do_memory_forget(s, id),
            Backend::Remote(r, _) => r.post("/memory/forget", json!({ "id": id })),
        }
    }

    /// Move a memory to another tier, or another repository.
    pub fn memory_move(&self, id: &str, to: &str) -> Result<String, String> {
        match self {
            Backend::Local(s) => do_memory_move(s, id, to),
            Backend::Remote(r, _) => r.post("/memory/move", json!({ "id": id, "to": to })),
        }
    }

    /// Link memories saved before the junction existed.
    pub fn backfill_links(&self, dry_run: bool, from_text: bool) -> Result<String, String> {
        match self {
            Backend::Local(s) => do_backfill_links(s, dry_run, from_text),
            Backend::Remote(r, _) => r.post(
                "/memories/backfill-links",
                json!({ "dry_run": dry_run, "from_text": from_text }),
            ),
        }
    }

    /// The most recent memories, with no query.
    pub fn memory_context(&self, scope: &str, limit: usize) -> Result<String, String> {
        match self {
            Backend::Local(s) => do_memory_context(s, scope, limit),
            Backend::Remote(r, _) => r.get(&format!("/memory/context?scope={scope}&limit={limit}")),
        }
    }

    /// A symbol's definition and code.
    pub fn read_symbol(&self, name: &str, limit: usize) -> Result<String, String> {
        match self {
            Backend::Local(s) => do_read_symbol(s, name, limit),
            Backend::Remote(r, _) => r.get(&format!("/symbol/{}?limit={limit}", urlencode(name))),
        }
    }

    /// One budgeted brief assembled for a question.
    pub fn build_context(
        &self,
        query: &str,
        max_tokens: usize,
        include_memories: bool,
    ) -> Result<String, String> {
        match self {
            Backend::Local(s) => do_build_context(s, query, max_tokens, include_memories),
            Backend::Remote(r, _) => r.post(
                "/context",
                json!({ "query": query, "max_tokens": max_tokens,
                        "include_memories": include_memories }),
            ),
        }
    }

    /// Memories recorded about a symbol — the memory↔graph join.
    pub fn memories_by_symbol(&self, symbol: &str, limit: usize) -> Result<String, String> {
        match self {
            Backend::Local(s) => do_memories_by_symbol(s, symbol, limit),
            Backend::Remote(r, _) => r.get(&format!(
                "/memories/by-symbol/{}?limit={limit}",
                urlencode(symbol)
            )),
        }
    }

    /// Memories recorded about a file.
    pub fn memories_by_file(&self, file: &str, limit: usize) -> Result<String, String> {
        match self {
            Backend::Local(s) => do_memories_by_file(s, file, limit),
            Backend::Remote(r, _) => r.get(&format!(
                "/memories/by-file/{}?limit={limit}",
                urlencode(file)
            )),
        }
    }

    /// The inverse: what code one memory concerns.
    pub fn memory_refs(&self, memory_id: &str) -> Result<String, String> {
        match self {
            Backend::Local(s) => do_memory_refs(s, memory_id),
            Backend::Remote(r, _) => r.get(&format!("/memory/{}/refs", urlencode(memory_id))),
        }
    }

    pub fn references(&self, symbol: &str) -> Result<String, String> {
        match self {
            Backend::Local(s) => do_references(s, symbol),
            Backend::Remote(r, _) => r.get(&format!("/references/{}", urlencode(symbol))),
        }
    }

    pub fn search_routes(
        &self,
        method: Option<String>,
        path: Option<String>,
    ) -> Result<String, String> {
        match self {
            Backend::Local(s) => do_search_routes(s, method, path),
            Backend::Remote(r, _) => {
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
            Backend::Remote(r, _) => r.get(&format!("/routes/handler/{}", urlencode(handler))),
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
            Backend::Remote(r, _) => r.post(
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
