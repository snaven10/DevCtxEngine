//! The central daemon: the single writer of the central database.
//!
//! DuckDB allows one writing process per file. Per-project servers each own
//! their own database, so they never contend — but the central store is shared
//! by all of them, and two processes opening it concurrently means one simply
//! fails. This daemon is the answer: it owns the file, and everything else
//! (project servers, the CLI, the TUI) reaches the registry and the global
//! memories through it.
//!
//! It deliberately loads no model. Registry work is pure SQL, so startup is a
//! DuckDB open and nothing more — which is what makes auto-spawning it on a
//! plain `devctx projects list` acceptable.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Path, Query, Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use devctx_central::{project_json, Central, RegisterRequest};
use devctx_memory::RememberRequest;
use serde::Deserialize;
use serde_json::json;

use crate::{json_err, json_ok, track};

/// Router state. The store holds a DuckDB connection, which is `Send` but not
/// `Sync`, so access is serialized behind a mutex — registry operations are
/// small and infrequent, so there is nothing to gain from finer granularity.
#[derive(Clone)]
struct CentralApi {
    central: Arc<Mutex<Central>>,
    token: Option<String>,
}

fn router(api: CentralApi) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/projects", get(list_projects).post(add_project))
        .route("/projects/:name", get(show_project))
        .route("/projects/:name", delete(remove_project))
        .route("/projects/:name/refresh", post(refresh_project))
        .route("/remember", post(remember))
        .route("/recall", post(recall))
        .route("/memories", get(memories))
        .route("/memory/stats", get(memory_stats))
        .layer(middleware::from_fn_with_state(api.clone(), auth))
        .with_state(api)
}

/// Serve the central API until stopped, exiting after `idle` with no non-health
/// request when set.
pub async fn serve(
    central: Central,
    addr: SocketAddr,
    token: Option<String>,
    idle: Option<Duration>,
) -> anyhow::Result<()> {
    let api = CentralApi {
        central: Arc::new(Mutex::new(central)),
        token,
    };
    let activity = Arc::new(Mutex::new(Instant::now()));
    let app = router(api).layer(middleware::from_fn_with_state(activity.clone(), track));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    eprintln!("DevCtxEngine central store listening on http://{addr}");

    if let Some(timeout) = idle {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(30)).await;
                let idle_for = activity.lock().map(|t| t.elapsed()).unwrap_or_default();
                if idle_for >= timeout {
                    eprintln!("Central store idle for {idle_for:?}; shutting down.");
                    std::process::exit(0);
                }
            }
        });
    }

    axum::serve(listener, app).await?;
    Ok(())
}

/// Blocking entry point: build a Tokio runtime and serve.
pub fn run_blocking(
    central: Central,
    addr: SocketAddr,
    token: Option<String>,
    idle: Option<Duration>,
) -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(serve(central, addr, token, idle))
}

// --- request bodies / query params ---

#[derive(Deserialize)]
struct AddBody {
    path: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: String,
    #[serde(default)]
    tags: String,
    #[serde(default)]
    init: bool,
}

#[derive(Deserialize)]
struct ListQuery {
    #[serde(default)]
    all: bool,
}

#[derive(Deserialize)]
struct RemoveQuery {
    #[serde(default)]
    deactivate: bool,
}

#[derive(Deserialize)]
struct RememberBody {
    content: String,
    #[serde(default)]
    title: String,
    #[serde(rename = "type", default)]
    memory_type: String,
    #[serde(default)]
    topic: String,
    #[serde(default)]
    tags: String,
    /// Contributing project and repository, kept as provenance.
    #[serde(default)]
    project: String,
    #[serde(default)]
    repo: String,
    #[serde(default)]
    branch: String,
}

#[derive(Deserialize)]
struct RecallBody {
    query: String,
    #[serde(default)]
    limit: Option<usize>,
    /// Narrow to memories contributed by one repository.
    #[serde(default)]
    repo: Option<String>,
}

#[derive(Deserialize)]
struct MemoriesQuery {
    #[serde(default)]
    limit: Option<usize>,
}

// --- handlers ---

async fn health() -> Response {
    json_ok(json!({ "status": "ok", "role": "central" }).to_string())
}

async fn list_projects(State(api): State<CentralApi>, Query(q): Query<ListQuery>) -> Response {
    run(api, move |c| {
        let projects = c.list(q.all).map_err(|e| e.to_string())?;
        Ok(
            json!({ "projects": projects.iter().map(project_json).collect::<Vec<_>>() })
                .to_string(),
        )
    })
    .await
}

async fn add_project(State(api): State<CentralApi>, Json(b): Json<AddBody>) -> Response {
    run(api, move |c| {
        let rec = c
            .register(&RegisterRequest {
                root: b.path.into(),
                name: b.name,
                description: b.description,
                tags: b.tags,
                create_config: b.init,
                now: devctx_central::now_stamp(),
            })
            .map_err(|e| e.to_string())?;
        Ok(project_json(&rec).to_string())
    })
    .await
}

async fn show_project(State(api): State<CentralApi>, Path(name): Path<String>) -> Response {
    run(api, move |c| {
        match c.get(&name).map_err(|e| e.to_string())? {
            Some(rec) => Ok(project_json(&rec).to_string()),
            None => Err(format!("no registered project named `{name}`")),
        }
    })
    .await
}

async fn refresh_project(State(api): State<CentralApi>, Path(name): Path<String>) -> Response {
    run(api, move |c| {
        let rec = c
            .refresh(&name, &devctx_central::now_stamp())
            .map_err(|e| e.to_string())?;
        Ok(project_json(&rec).to_string())
    })
    .await
}

async fn remove_project(
    State(api): State<CentralApi>,
    Path(name): Path<String>,
    Query(q): Query<RemoveQuery>,
) -> Response {
    run(api, move |c| {
        let done = if q.deactivate {
            c.deactivate(&name, &devctx_central::now_stamp())
        } else {
            c.remove(&name)
        }
        .map_err(|e| e.to_string())?;
        if !done {
            return Err(format!("no registered project named `{name}`"));
        }
        Ok(json!({ "removed": name, "deactivated": q.deactivate }).to_string())
    })
    .await
}

async fn remember(State(api): State<CentralApi>, Json(b): Json<RememberBody>) -> Response {
    run(api, move |c| {
        let res = c
            .remember(&RememberRequest {
                title: b.title,
                content: b.content,
                memory_type: if b.memory_type.is_empty() {
                    "insight".to_string()
                } else {
                    b.memory_type
                },
                project: b.project,
                topic_key: b.topic,
                tags: b.tags,
                repo: b.repo,
                branch: b.branch,
                now: devctx_central::now_stamp(),
                ..Default::default()
            })
            .map_err(|e| e.to_string())?;
        Ok(json!({
            "id": res.memory.id,
            "title": res.memory.title,
            "status": format!("{:?}", res.status).to_lowercase(),
            "scope": res.memory.scope,
            "repo": res.memory.repo,
            "revision_count": res.memory.revision_count,
            "duplicate_count": res.memory.duplicate_count,
        })
        .to_string())
    })
    .await
}

async fn recall(State(api): State<CentralApi>, Json(b): Json<RecallBody>) -> Response {
    run(api, move |c| {
        let hits = c
            .recall(&b.query, b.repo.as_deref(), b.limit.unwrap_or(5))
            .map_err(|e| e.to_string())?;
        Ok(json!({
            "memories": hits.iter().map(|h| json!({
                "id": h.memory.id,
                "title": h.memory.title,
                "content": h.memory.content,
                "type": h.memory.memory_type,
                "tags": h.memory.tags,
                "repo": h.memory.repo,
                "score": h.score,
                "updated_at": h.memory.updated_at,
            })).collect::<Vec<_>>()
        })
        .to_string())
    })
    .await
}

async fn memories(State(api): State<CentralApi>, Query(q): Query<MemoriesQuery>) -> Response {
    run(api, move |c| {
        let mems = c
            .recent_memories(q.limit.unwrap_or(20))
            .map_err(|e| e.to_string())?;
        Ok(json!({
            "memories": mems.iter().map(|m| json!({
                "id": m.id,
                "title": m.title,
                "content": m.content,
                "type": m.memory_type,
                "tags": m.tags,
                "repo": m.repo,
                "updated_at": m.updated_at,
            })).collect::<Vec<_>>()
        })
        .to_string())
    })
    .await
}

async fn memory_stats(State(api): State<CentralApi>) -> Response {
    run(api, move |c| {
        let stats = c.memory_stats().map_err(|e| e.to_string())?;
        Ok(json!({ "total": stats.total, "by_type": stats.by_type }).to_string())
    })
    .await
}

/// Bearer-token auth for everything except `/health`.
async fn auth(State(api): State<CentralApi>, req: Request, next: Next) -> Response {
    if req.uri().path() == "/health" {
        return next.run(req).await;
    }
    if let Some(expected) = &api.token {
        let ok = req
            .headers()
            .get(axum::http::header::AUTHORIZATION)
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

/// Run a blocking registry call on the blocking pool and render its JSON result.
async fn run<F>(api: CentralApi, f: F) -> Response
where
    F: FnOnce(&Central) -> Result<String, String> + Send + 'static,
{
    let central = api.central.clone();
    let task = tokio::task::spawn_blocking(move || match central.lock() {
        Ok(guard) => f(&guard),
        Err(e) => Err(format!("central store lock poisoned: {e}")),
    });
    match task.await {
        Ok(Ok(body)) => json_ok(body),
        Ok(Err(e)) => json_err(StatusCode::BAD_REQUEST, e),
        Err(e) => json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("task failed: {e}"),
        ),
    }
}
