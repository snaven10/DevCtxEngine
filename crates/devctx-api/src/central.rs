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
