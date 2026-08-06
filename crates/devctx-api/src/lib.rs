//! `devctx-api` — an HTTP REST API for DevCtxEngine (axum), reusing the MCP engine.
//!
//! Every route delegates to the same `do_*` handlers the MCP server uses, which
//! return JSON strings. Optional Bearer-token auth guards all routes except
//! `/health`. See `docs/rust-rewrite-plan.md`.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{Path, Query, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use devctx_core::config::ProjectConfig;
use devctx_mcp::state::{
    do_graph, do_impact, do_index, do_index_status, do_memory_context, do_memory_stats,
    do_read_file, do_recall, do_references, do_remember, do_routes_for_handler, do_search,
    do_search_routes, do_summarize, parse_mode, AppState,
};
use serde::Deserialize;

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
        .route("/status", get(status))
        .route("/remember", post(remember))
        .route("/recall", post(recall))
        .route("/memories", get(memories))
        .route("/memory/stats", get(memory_stats))
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

/// Serve the API until the process is stopped.
pub async fn serve(
    cfg: ProjectConfig,
    addr: SocketAddr,
    token: Option<String>,
) -> anyhow::Result<()> {
    let state = Arc::new(AppState::build(cfg)?);
    let app = router(Api { state, token });
    let listener = tokio::net::TcpListener::bind(addr).await?;
    eprintln!("DevCtxEngine API listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

/// Blocking entry point: build a Tokio runtime and serve.
pub fn run_blocking(
    cfg: ProjectConfig,
    addr: SocketAddr,
    token: Option<String>,
) -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(serve(cfg, addr, token))
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
}

#[derive(Deserialize)]
struct IndexBody {
    #[serde(default)]
    full: Option<bool>,
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
}

#[derive(Deserialize)]
struct RecallBody {
    query: String,
    #[serde(default)]
    limit: Option<usize>,
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
        )
    })
    .await
}

async fn index(State(api): State<Api>, Json(b): Json<IndexBody>) -> Response {
    run(api.state, move |s| do_index(s, b.full.unwrap_or(false))).await
}

async fn status(State(api): State<Api>) -> Response {
    run(api.state, do_index_status).await
}

async fn remember(State(api): State<Api>, Json(b): Json<RememberBody>) -> Response {
    run(api.state, move |s| {
        do_remember(
            s,
            b.content,
            b.title.unwrap_or_default(),
            b.memory_type.unwrap_or_else(|| "note".to_string()),
            b.topic.unwrap_or_default(),
            b.tags.unwrap_or_default(),
        )
    })
    .await
}

async fn recall(State(api): State<Api>, Json(b): Json<RecallBody>) -> Response {
    run(api.state, move |s| {
        do_recall(s, &b.query, b.limit.unwrap_or(5))
    })
    .await
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

fn json_ok(body: String) -> Response {
    ([(header::CONTENT_TYPE, "application/json")], body).into_response()
}

fn json_err(code: StatusCode, msg: String) -> Response {
    let body = serde_json::json!({ "error": msg }).to_string();
    (code, [(header::CONTENT_TYPE, "application/json")], body).into_response()
}
