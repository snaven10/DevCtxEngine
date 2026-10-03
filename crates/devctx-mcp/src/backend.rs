//! Tool backend: either owns the DB locally (`AppState` + `do_*`) or routes each
//! tool call to a shared server over HTTP. Routing lets many MCP sessions plus
//! the web/CLI/TUI share a single DB owner without DuckDB lock conflicts.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use crate::state::{
    do_backfill_links, do_build_context, do_impact, do_index, do_index_status, do_list_projects,
    do_memories_by_file, do_memories_by_symbol, do_memory_context, do_memory_forget,
    do_memory_move, do_memory_refs, do_memory_stats, do_plan_status, do_read_file, do_read_symbol,
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

/// Finds (spawning if needed) the project's server; the error is what the agent
/// reads when there is none.
pub type Connector = Arc<dyn Fn() -> Result<ServerConn, String> + Send + Sync>;

/// A thin blocking HTTP client for the shared server's endpoints.
///
/// The connection may not exist yet: an MCP never opens the project's database
/// itself, and it does not need the server at startup either. The first call
/// connects, and a call that finds no server fails with the reason and tries
/// again next time.
pub struct RemoteClient {
    conn: std::sync::Mutex<Option<(String, Option<String>)>>,
    connect: Option<Connector>,
}

/// Default per-call timeout for a routed request, in seconds. This used to be a
/// flat 3600s applied to every call — search, read_file, recall, all of it —
/// so a genuinely hung daemon or a stalled connection left a caller waiting an
/// hour to find out. Overridable because a slow network is a real thing an
/// operator may need to widen this for, without a rebuild.
const DEFAULT_TIMEOUT_SECS: u64 = 120;

/// Timeout for the one route that can legitimately run for minutes: indexing a
/// large repository. Not read from the environment — an operator who wants
/// `index_repo` to time out sooner than 30 minutes can call it with `full:
/// false` and let incremental indexing do less work per call.
const LONG_TIMEOUT_SECS: u64 = 1800;

/// How long a routed call waits before giving up, from `DEVCTX_DAEMON_TIMEOUT_SECS`.
fn default_timeout() -> Duration {
    Duration::from_secs(
        std::env::var("DEVCTX_DAEMON_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_TIMEOUT_SECS),
    )
}

fn long_timeout() -> Duration {
    Duration::from_secs(LONG_TIMEOUT_SECS)
}

impl RemoteClient {
    /// `(base, token)` of the server, connecting on first use. Held under the
    /// lock so concurrent first calls spawn one server, not several.
    fn target(&self) -> Result<(String, Option<String>), String> {
        let mut slot = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(t) = slot.as_ref() {
            return Ok(t.clone());
        }
        let connect = self
            .connect
            .as_ref()
            .ok_or_else(|| "no devctx server connection".to_string())?;
        let c = connect()?;
        let t = (c.base, c.token);
        *slot = Some(t.clone());
        Ok(t)
    }

    fn agent(&self, timeout: Duration) -> ureq::Agent {
        ureq::AgentBuilder::new().timeout(timeout).build()
    }

    fn auth(req: ureq::Request, token: &Option<String>) -> ureq::Request {
        match token {
            Some(t) => req.set("Authorization", &format!("Bearer {t}")),
            None => req,
        }
    }

    fn get(&self, path: &str) -> Result<String, String> {
        self.get_timed(path, default_timeout())
    }

    fn get_timed(&self, path: &str, timeout: Duration) -> Result<String, String> {
        let (base, token) = self.target()?;
        read(Self::auth(self.agent(timeout).get(&format!("{base}{path}")), &token).call())
    }

    fn post(&self, path: &str, body: Value) -> Result<String, String> {
        self.post_timed(path, body, default_timeout())
    }

    fn post_timed(&self, path: &str, body: Value, timeout: Duration) -> Result<String, String> {
        let (base, token) = self.target()?;
        read(Self::auth(self.agent(timeout).post(&format!("{base}{path}")), &token).send_json(body))
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
    /// Where this project's `plans/` live. `plan_status` only reads markdown, so
    /// it is answered here, in process, without a server and without a `Store`.
    pub plans_root: Option<devctx_core::plans::PlansRoot>,
}

impl Backend {
    pub fn local(state: Arc<AppState>) -> Self {
        Backend::Local(state)
    }

    pub fn remote(conn: ServerConn, identity: ProjectIdentity) -> Self {
        Backend::Remote(
            RemoteClient {
                conn: std::sync::Mutex::new(Some((conn.base, conn.token))),
                connect: None,
            },
            identity,
        )
    }

    /// A backend whose server is found on first use (see [`RemoteClient`]).
    pub fn lazy(connect: Connector, identity: ProjectIdentity) -> Self {
        Backend::Remote(
            RemoteClient {
                conn: std::sync::Mutex::new(None),
                connect: Some(connect),
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
            // The one call worth a long timeout: indexing a large repository
            // from scratch can run for many minutes on the server.
            Backend::Remote(r, _) => {
                r.post_timed("/index", json!({ "full": full }), long_timeout())
            }
        }
    }

    pub fn index_status(&self) -> Result<String, String> {
        match self {
            Backend::Local(s) => do_index_status(s),
            Backend::Remote(r, _) => r.get("/status"),
        }
    }

    /// Progress on the plans in `plans/` (see `plans/PLAN-005-plan-status/`): no `plan` lists
    /// them, `plan` gives one plan's ready/in-progress/blocked tasks.
    pub fn plan_status(&self, plan: Option<&str>) -> Result<String, String> {
        match self {
            Backend::Local(s) => do_plan_status(s, plan),
            Backend::Remote(_, id) if id.plans_root.is_some() => {
                crate::state::plan_status_budgeted(id.plans_root.as_ref().unwrap(), plan)
            }
            Backend::Remote(r, _) => match plan {
                Some(p) => r.get(&format!("/plans/status?plan={}", urlencode(p))),
                None => r.get("/plans/status"),
            },
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
        provenance: Option<String>,
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
                    provenance.as_deref(),
                )
            }
            Backend::Local(s) => do_remember(s, content, title, memory_type, topic, tags, files),
            Backend::Remote(r, _) => {
                let answer = r.post(
                    "/remember",
                    json!({ "content": content, "title": title, "type": memory_type,
                            "topic": topic, "tags": tags, "scope": scope,
                            "files": files, "provenance": provenance }),
                )?;
                Ok(Self::warn_if_provenance_was_ignored(
                    answer,
                    provenance.as_deref(),
                ))
            }
        }
    }

    /// Say so when the server did not honour `provenance`.
    ///
    /// Serde ignores unknown fields by default, so a server built before this
    /// field existed accepts the request, drops it, and attributes the memory
    /// to whatever git reports — the exact bug this closes, silently, against a
    /// version that looks like it worked. Sending the field is not enough; the
    /// answer has to be checked.
    ///
    /// The reply carries the stored `repo`, so this is a comparison rather than
    /// a guess. A mismatch is a warning and not an error: the memory was saved,
    /// and losing it over a wrong attribution would be the worse trade.
    fn warn_if_provenance_was_ignored(answer: String, wanted: Option<&str>) -> String {
        let Some(wanted) = wanted.filter(|w| !w.is_empty()) else {
            return answer;
        };
        let Ok(mut v) = serde_json::from_str::<Value>(&answer) else {
            return answer;
        };
        // Owned, not borrowed: the warning is written back into the same value.
        let stored = v
            .get("repo")
            .and_then(|r| r.as_str())
            .unwrap_or_default()
            .to_string();
        if stored == wanted {
            return answer;
        }
        if let Value::Object(map) = &mut v {
            map.insert(
                "warning".into(),
                json!(format!(
                    "the server attributed this memory to `{stored}` rather than `{wanted}`, \
                     so it is probably older than the `provenance` field and ignored it. \
                     The memory was saved; its attribution is wrong."
                )),
            );
        }
        serde_json::to_string_pretty(&v).unwrap_or(answer)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The reason sending the field is not enough.
    ///
    /// Serde ignores unknown fields, so a server older than `provenance`
    /// accepts the request, drops it, and answers 200 with the memory
    /// attributed to whatever git said. Without this check the bug closes
    /// against a version that only looks like it works.
    #[test]
    fn an_ignored_provenance_is_reported_not_assumed() {
        let answer = json!({ "id": "mem_1", "repo": "api" }).to_string();
        let out = Backend::warn_if_provenance_was_ignored(answer, Some("REVFA"));
        let v: Value = serde_json::from_str(&out).unwrap();
        let warning = v["warning"]
            .as_str()
            .expect("a mismatch has to be reported");
        assert!(warning.contains("api"), "name what it stored: {warning}");
        assert!(warning.contains("REVFA"), "and what was asked: {warning}");
        assert_eq!(v["id"], "mem_1", "the answer itself survives");
    }

    /// A server that honoured it says nothing extra — the common case must not
    /// grow a warning nobody needs.
    #[test]
    fn an_honoured_provenance_is_left_alone() {
        let answer = json!({ "id": "mem_1", "repo": "REVFA" }).to_string();
        let out = Backend::warn_if_provenance_was_ignored(answer.clone(), Some("REVFA"));
        assert_eq!(out, answer);
    }

    /// No provenance asked for, nothing to check. A project-bound session sends
    /// none, and its `repo` is whatever git reports — which is correct there.
    #[test]
    fn no_provenance_asked_means_no_comparison() {
        let answer = json!({ "id": "mem_1", "repo": "api" }).to_string();
        assert_eq!(
            Backend::warn_if_provenance_was_ignored(answer.clone(), None),
            answer
        );
        assert_eq!(
            Backend::warn_if_provenance_was_ignored(answer.clone(), Some("")),
            answer
        );
    }

    /// An answer that is not JSON comes back untouched rather than being
    /// swallowed: whatever the server said, the caller should see it.
    #[test]
    fn a_non_json_answer_passes_through() {
        let answer = "saved".to_string();
        assert_eq!(
            Backend::warn_if_provenance_was_ignored(answer.clone(), Some("REVFA")),
            answer
        );
    }
}
