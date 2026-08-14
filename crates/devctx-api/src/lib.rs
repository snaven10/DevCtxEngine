//! `devctx-api` — an HTTP REST API for DevCtxEngine (axum), reusing the MCP engine.
//!
//! Every route delegates to the same `do_*` handlers the MCP server uses, which
//! return JSON strings. Optional Bearer-token auth guards all routes except
//! `/health`. See `docs/rust-rewrite-plan.md`.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Path, Query, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use devctx_core::config::ProjectConfig;
use devctx_mcp::state::{
    do_graph, do_impact, do_index, do_index_paths, do_index_progress, do_index_status,
    do_list_projects, do_memory_context, do_memory_stats, do_read_file, do_recall_scoped,
    do_references, do_remember, do_remember_shared, do_routes_for_handler, do_search,
    do_search_routes, do_summarize, parse_mode, AppState,
};
use serde::Deserialize;

pub mod central;

/// Vendored web dashboard (served at `/`) and its cytoscape bundle.
const DASHBOARD_HTML: &str = include_str!("../assets/index.html");
const CYTOSCAPE_JS: &str = include_str!("../assets/cytoscape.min.js");

/// Shared router state: the engine plus an optional auth token.
#[derive(Clone)]
struct Api {
    state: Arc<AppState>,
    token: Option<String>,
}

/// Build the router with all routes and the auth layer.
fn router(api: Api) -> Router {
    Router::new()
        .route("/", get(dashboard))
        .route("/assets/cytoscape.min.js", get(cytoscape_js))
        .route("/health", get(health))
        .route("/search", post(search))
        .route("/index", post(index))
        .route("/index/progress", get(index_progress))
        .route("/status", get(status))
        .route("/remember", post(remember))
        .route("/recall", post(recall))
        .route("/memories", get(memories))
        .route("/memory/stats", get(memory_stats))
        .route("/projects", get(list_projects))
        .route("/graph", get(graph))
        .route("/impact/:symbol", get(impact))
        .route("/references/:symbol", get(references))
        .route("/routes", get(routes))
        .route("/routes/handler/:handler", get(routes_for_handler))
        .route("/read_file", post(read_file))
        .route("/summarize", post(summarize))
        .layer(middleware::from_fn_with_state(api.clone(), auth))
        .with_state(api)
}

/// How long a loaded model may go unused before it is dropped, when
/// `DEVCTX_MODEL_IDLE_SECS` says nothing. Comfortably shorter than the usual
/// idle-shutdown window, so a server that is only waiting around stops paying
/// for models it is not using.
const DEFAULT_MODEL_IDLE_SECS: u64 = 300;

/// How long an unused model is kept. `DEVCTX_MODEL_IDLE_SECS=0` keeps models
/// for the life of the process, which is what this used to do unconditionally.
fn model_idle() -> Option<Duration> {
    let secs: u64 = std::env::var("DEVCTX_MODEL_IDLE_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_MODEL_IDLE_SECS);
    (secs > 0).then(|| Duration::from_secs(secs))
}

/// Serve the API until the process is stopped. When `idle` is `Some`, the
/// process exits after that long with no non-health request — used by
/// auto-spawned servers so they don't linger forever.
pub async fn serve(
    cfg: ProjectConfig,
    addr: SocketAddr,
    token: Option<String>,
    idle: Option<Duration>,
) -> anyhow::Result<()> {
    let state = Arc::new(AppState::build(cfg)?);
    let activity = Arc::new(Mutex::new(Instant::now()));
    let app = router(Api {
        state: state.clone(),
        token,
    })
    .layer(middleware::from_fn_with_state(activity.clone(), track));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    eprintln!("DevCtxEngine API listening on http://{addr}");

    if let Some(timeout) = idle {
        let act = activity.clone();
        let closing = state.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(30)).await;
                // A poisoned lock means a thread panicked holding it, not that a
                // request just arrived. Reading that as `unwrap_or_default()` —
                // `Duration::ZERO`, i.e. "busy right now" — silenced this timer
                // for the rest of the process's life, which is the exact runaway
                // it exists to prevent: the server then lingers forever, holding
                // an embedding model and a reranker in memory. A panic cannot
                // leave an `Instant` half-written, so the value behind the lock
                // is still sound. Take it and carry on.
                let idle_for = match act.lock() {
                    Ok(t) => t.elapsed(),
                    Err(poisoned) => poisoned.into_inner().elapsed(),
                };
                if idle_for >= timeout {
                    // "No requests lately" is not the same as "nothing to do".
                    // Indexing runs inside this process and can take far longer
                    // than the idle window; the client that asked for it stops
                    // polling as soon as its own read times out, and exiting
                    // here discarded a nearly complete run.
                    if closing.is_indexing() {
                        continue;
                    }
                    eprintln!("DevCtxEngine server idle for {idle_for:?}; shutting down.");
                    // `exit` runs no destructors, so the database would keep a
                    // write-ahead log no process is ever going to fold in.
                    closing.checkpoint();
                    std::process::exit(0);
                }
            }
        });
    }

    // Staying warm for the next command is the point of the server; holding a
    // cross-encoder the whole time is not. See `AppState::release_idle_models`.
    if let Some(max_idle) = model_idle() {
        let sweeping = state.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(60)).await;
                let s = sweeping.clone();
                // Dropping a model frees hundreds of megabytes and is not
                // instant, so it does not belong on an async worker thread.
                let released = tokio::task::spawn_blocking(move || s.release_idle_models(max_idle))
                    .await
                    .unwrap_or_default();
                if !released.is_empty() {
                    eprintln!(
                        "Released the {} after {}s unused.",
                        released.join(" and the "),
                        max_idle.as_secs()
                    );
                }
            }
        });
    }

    // `devctx serve --stop` sends a plain TERM, whose default action is just as
    // abrupt as `exit`. Catching it buys the one thing that matters: a
    // checkpoint before the connection disappears.
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = terminate().await;
            eprintln!("DevCtxEngine server terminating; checkpointing.");
        })
        .await?;
    state.checkpoint();
    Ok(())
}

/// Resolves when the process is asked to stop (SIGTERM, or Ctrl-C).
async fn terminate() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            _ = term.recv() => {}
            _ = tokio::signal::ctrl_c() => {}
        }
        Ok(())
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await
}

/// Middleware: record the time of the last non-health request (for idle-shutdown).
pub(crate) async fn track(
    State(act): State<Arc<Mutex<Instant>>>,
    req: Request,
    next: Next,
) -> Response {
    if req.uri().path() != "/health" {
        // Recover from poisoning rather than skip the write: dropping it would
        // freeze the clock at the last successful request, and a busy server
        // would then look idle and shut down under load.
        let mut t = act.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        *t = Instant::now();
    }
    next.run(req).await
}

/// Blocking entry point: build a Tokio runtime and serve.
pub fn run_blocking(
    cfg: ProjectConfig,
    addr: SocketAddr,
    token: Option<String>,
    idle: Option<Duration>,
) -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(serve(cfg, addr, token, idle))
}

// --- request bodies / query params ---

#[derive(Deserialize)]
struct SearchBody {
    query: String,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    mode: Option<String>,
    /// Reorder with the cross-encoder. Defaults to on, matching the config;
    /// interactive callers turn it off because it dominates latency.
    #[serde(default)]
    rerank: Option<bool>,
}

#[derive(Deserialize)]
struct IndexBody {
    #[serde(default)]
    full: Option<bool>,
    /// Index exactly these repo-relative paths instead of a commit diff.
    #[serde(default)]
    paths: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct RememberBody {
    content: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default, rename = "type")]
    memory_type: Option<String>,
    #[serde(default)]
    topic: Option<String>,
    #[serde(default)]
    tags: Option<String>,
    /// `local` (default) or `global` — global goes to the central store.
    #[serde(default)]
    scope: Option<String>,
}

#[derive(Deserialize)]
struct RecallBody {
    query: String,
    #[serde(default)]
    limit: Option<usize>,
    /// `local`, `global`, or `all` (default).
    #[serde(default)]
    scope: Option<String>,
    /// Narrow global results to one contributing repository.
    #[serde(default)]
    repo: Option<String>,
}

#[derive(Deserialize)]
struct ListProjectsQuery {
    #[serde(default)]
    all: bool,
}

#[derive(Deserialize)]
struct ReadFileBody {
    path: String,
    #[serde(default)]
    start_line: Option<usize>,
    #[serde(default)]
    end_line: Option<usize>,
}

#[derive(Deserialize)]
struct SummarizeBody {
    content: String,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    max_tokens: Option<usize>,
}

#[derive(Deserialize)]
struct RoutesQuery {
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    path: Option<String>,
}

#[derive(Deserialize)]
struct ImpactQuery {
    #[serde(default)]
    depth: Option<usize>,
}

#[derive(Deserialize)]
struct GraphQuery {
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    hide_external: Option<bool>,
    #[serde(default)]
    hide_synthetic: Option<bool>,
}

#[derive(Deserialize)]
struct MemoriesQuery {
    #[serde(default)]
    limit: Option<usize>,
}

// --- handlers ---

async fn health() -> Response {
    json_ok(r#"{"status":"ok"}"#.to_string())
}

/// The web dashboard shell (call-graph + memories).
async fn dashboard() -> Response {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        DASHBOARD_HTML,
    )
        .into_response()
}

/// The vendored cytoscape bundle (served locally, no CDN).
async fn cytoscape_js() -> Response {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        CYTOSCAPE_JS,
    )
        .into_response()
}

async fn graph(State(api): State<Api>, Query(q): Query<GraphQuery>) -> Response {
    run(api.state, move |s| {
        do_graph(
            s,
            q.kind,
            None,
            q.limit.unwrap_or(0),
            q.hide_external.unwrap_or(false),
            q.hide_synthetic.unwrap_or(true),
        )
    })
    .await
}

async fn memories(State(api): State<Api>, Query(q): Query<MemoriesQuery>) -> Response {
    run(api.state, move |s| {
        do_memory_context(s, q.limit.unwrap_or(25))
    })
    .await
}

async fn search(State(api): State<Api>, Json(b): Json<SearchBody>) -> Response {
    run(api.state, move |s| {
        do_search(
            s,
            &b.query,
            b.limit.unwrap_or(10),
            b.language,
            parse_mode(b.mode.as_deref()),
            b.rerank.unwrap_or(true),
        )
    })
    .await
}

async fn index(State(api): State<Api>, Json(b): Json<IndexBody>) -> Response {
    if let Some(paths) = b.paths.filter(|p| !p.is_empty()) {
        return run(api.state, move |s| do_index_paths(s, &paths)).await;
    }
    run(api.state, move |s| do_index(s, b.full.unwrap_or(false))).await
}

/// How far the current indexing run has got.
///
/// Deliberately **not** routed through [`run`]. That helper hands work to
/// `spawn_blocking`, and the blocking pool is exactly where a long index is
/// already sitting — a progress request queued behind it would answer only once
/// the run it was reporting on had finished. Reading the counters takes a short
/// lock and no database, so it belongs on the async executor.
async fn index_progress(State(api): State<Api>) -> Response {
    match do_index_progress(&api.state) {
        Ok(body) => json_ok(body),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn status(State(api): State<Api>) -> Response {
    run(api.state, do_index_status).await
}

async fn remember(State(api): State<Api>, Json(b): Json<RememberBody>) -> Response {
    run(api.state, move |s| {
        let title = b.title.unwrap_or_default();
        let memory_type = b.memory_type.unwrap_or_else(|| "note".to_string());
        let topic = b.topic.unwrap_or_default();
        let tags = b.tags.unwrap_or_default();
        let scope = b.scope.as_deref().unwrap_or("local");
        // `group` and `global` both live in the central store; only the space
        // they land in differs.
        if devctx_memory::is_global(scope) || devctx_memory::is_group(scope) {
            let group = if devctx_memory::is_group(scope) {
                s.group_name()
            } else {
                String::new()
            };
            return do_remember_shared(s, &b.content, &title, &memory_type, &topic, &tags, &group);
        }
        do_remember(s, b.content, title, memory_type, topic, tags)
    })
    .await
}

async fn recall(State(api): State<Api>, Json(b): Json<RecallBody>) -> Response {
    run(api.state, move |s| {
        do_recall_scoped(
            s,
            &b.query,
            b.limit.unwrap_or(5),
            b.scope.as_deref().unwrap_or("all"),
            b.repo.as_deref(),
        )
    })
    .await
}

/// The registry, proxied so a routed MCP session can reach it without opening
/// the central database itself.
async fn list_projects(Query(q): Query<ListProjectsQuery>) -> Response {
    match tokio::task::spawn_blocking(move || do_list_projects(None, "", q.all)).await {
        Ok(Ok(body)) => json_ok(body),
        Ok(Err(e)) => json_err(StatusCode::BAD_REQUEST, e),
        Err(e) => json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("task failed: {e}"),
        ),
    }
}

async fn memory_stats(State(api): State<Api>) -> Response {
    run(api.state, do_memory_stats).await
}

async fn impact(
    State(api): State<Api>,
    Path(symbol): Path<String>,
    Query(q): Query<ImpactQuery>,
) -> Response {
    run(api.state, move |s| {
        do_impact(s, &symbol, q.depth.unwrap_or(3))
    })
    .await
}

async fn references(State(api): State<Api>, Path(symbol): Path<String>) -> Response {
    run(api.state, move |s| do_references(s, &symbol)).await
}

async fn routes(State(api): State<Api>, Query(q): Query<RoutesQuery>) -> Response {
    run(api.state, move |s| do_search_routes(s, q.method, q.path)).await
}

async fn routes_for_handler(State(api): State<Api>, Path(handler): Path<String>) -> Response {
    run(api.state, move |s| do_routes_for_handler(s, &handler)).await
}

async fn read_file(State(api): State<Api>, Json(b): Json<ReadFileBody>) -> Response {
    run(api.state, move |s| {
        do_read_file(s, &b.path, b.start_line, b.end_line)
    })
    .await
}

async fn summarize(State(api): State<Api>, Json(b): Json<SummarizeBody>) -> Response {
    run(api.state, move |s| {
        do_summarize(s, &b.content, b.query, b.max_tokens.unwrap_or(200))
    })
    .await
}

// --- helpers ---

/// Bearer-token auth for all routes except public ones (health + the dashboard
/// shell and its static assets, so the page always loads).
async fn auth(State(api): State<Api>, req: Request, next: Next) -> Response {
    let path = req.uri().path();
    if path == "/health" || path == "/" || path.starts_with("/assets/") {
        return next.run(req).await;
    }
    if let Some(expected) = &api.token {
        let ok = req
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(|t| t == expected)
            .unwrap_or(false);
        if !ok {
            return json_err(StatusCode::UNAUTHORIZED, "unauthorized".to_string());
        }
    }
    next.run(req).await
}

/// Run a blocking engine call on the blocking pool and render its JSON result.
async fn run<F>(state: Arc<AppState>, f: F) -> Response
where
    F: FnOnce(&AppState) -> std::result::Result<String, String> + Send + 'static,
{
    match tokio::task::spawn_blocking(move || f(&state)).await {
        Ok(Ok(body)) => json_ok(body),
        Ok(Err(e)) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e),
        Err(e) => json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("task failed: {e}"),
        ),
    }
}

pub(crate) fn json_ok(body: String) -> Response {
    ([(header::CONTENT_TYPE, "application/json")], body).into_response()
}

pub(crate) fn json_err(code: StatusCode, msg: String) -> Response {
    let body = serde_json::json!({ "error": msg }).to_string();
    (code, [(header::CONTENT_TYPE, "application/json")], body).into_response()
}
