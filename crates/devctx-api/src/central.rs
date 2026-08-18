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
        .route("/projects/indexed", post(record_index))
        .route("/remember", post(remember))
        .route("/recall", post(recall))
        .route("/memories", get(memories))
        .route("/memories/by-id", post(memories_by_id))
        .route("/memories/mentioning", post(memories_mentioning))
        .route("/memory/stats", get(memory_stats))
        .route("/memory/:id", delete(forget_memory))
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
    let sweep_every = {
        let guard = api.central.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        guard.config().reindex.every_seconds
    };
    let registry = api.central.clone();
    let activity = Arc::new(Mutex::new(Instant::now()));
    let app = router(api).layer(middleware::from_fn_with_state(activity.clone(), track));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    eprintln!("DevCtxEngine central store listening on http://{addr}");

    if let Some(timeout) = idle {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(30)).await;
                // See the same loop in `lib.rs::serve`: a poisoned lock read as
                // `Duration::ZERO` makes this server immortal instead of idle.
                let idle_for = match activity.lock() {
                    Ok(t) => t.elapsed(),
                    Err(poisoned) => poisoned.into_inner().elapsed(),
                };
                if idle_for >= timeout {
                    eprintln!("Central store idle for {idle_for:?}; shutting down.");
                    std::process::exit(0);
                }
            }
        });
    }

    if sweep_every > 0 {
        eprintln!("Background reindex: every {sweep_every}s");
        tokio::spawn(async move {
            let period = Duration::from_secs(sweep_every);
            loop {
                tokio::time::sleep(period).await;
                let stale = tokio::task::spawn_blocking({
                    let registry = registry.clone();
                    move || stale_projects(&registry)
                })
                .await
                .unwrap_or_default();
                for (name, path) in stale {
                    eprintln!("Reindexing {name} (HEAD moved)");
                    let _ = tokio::task::spawn_blocking(move || index_project(&path)).await;
                }
            }
        });
    }

    axum::serve(listener, app).await?;
    Ok(())
}

/// Registered projects whose HEAD has moved since they were last indexed.
///
/// Deliberately compares `git rev-parse HEAD` against the recorded commit
/// rather than opening any database: the sweep must be cheap enough to run on a
/// timer without touching projects that have nothing to do.
fn stale_projects(registry: &Arc<Mutex<Central>>) -> Vec<(String, String)> {
    let Ok(guard) = registry.lock() else {
        return Vec::new();
    };
    let Ok(projects) = guard.list(false) else {
        return Vec::new();
    };
    projects
        .into_iter()
        .filter(|p| match head_commit(&p.path) {
            Some(head) => head != p.last_commit,
            None => false,
        })
        .map(|p| (p.name, p.path))
        .collect()
}

fn head_commit(path: &str) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|c| !c.is_empty())
}

/// Index a project by running `devctx index` inside it, so the work goes
/// through that project's own server rather than this process.
fn index_project(path: &str) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let _ = std::process::Command::new(exe)
        .arg("index")
        .current_dir(path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
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
struct IndexedBody {
    /// Absolute repository path — the key the registry is looked up by.
    path: String,
    #[serde(default)]
    commit: String,
    #[serde(default)]
    branch: String,
    #[serde(default)]
    files: i64,
    #[serde(default)]
    symbols: i64,
    #[serde(default)]
    chunks: i64,
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
    /// Group to share this memory with. Empty means the global space.
    #[serde(default)]
    group: String,
    /// Comma-separated files the memory concerns. Carried so the contributing
    /// project can link the memory to the code it is about.
    #[serde(default)]
    files: String,
}

#[derive(Deserialize)]
struct RecallBody {
    query: String,
    #[serde(default)]
    limit: Option<usize>,
    /// Narrow to memories contributed by one repository.
    #[serde(default)]
    repo: Option<String>,
    /// Recall from one group's space instead of the global one.
    #[serde(default)]
    group: Option<String>,
}

#[derive(Deserialize)]
struct MemoriesQuery {
    #[serde(default)]
    limit: Option<usize>,
}

/// Bodies for `POST /memories/by-id`.
#[derive(Deserialize)]
struct ByIdBody {
    #[serde(default)]
    ids: Vec<String>,
}

/// Body for `POST /memories/mentioning`.
#[derive(Deserialize)]
struct MentioningBody {
    label: String,
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

async fn record_index(State(api): State<CentralApi>, Json(b): Json<IndexedBody>) -> Response {
    run(api, move |c| {
        let recorded = c
            .record_index(
                &b.path,
                &devctx_store::ProjectIndexStats {
                    commit: b.commit,
                    branch: b.branch,
                    files: b.files,
                    symbols: b.symbols,
                    chunks: b.chunks,
                },
                &devctx_central::now_stamp(),
            )
            .map_err(|e| e.to_string())?;
        Ok(json!({ "recorded": recorded }).to_string())
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
                files: b.files,
                now: devctx_central::now_stamp(),
                scope: if b.group.is_empty() {
                    String::new()
                } else {
                    devctx_memory::SCOPE_GROUP.to_string()
                },
                group: b.group,
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
        let limit = b.limit.unwrap_or(5);
        let hits = match b.group.as_deref().filter(|g| !g.is_empty()) {
            Some(g) => c
                .recall_in(
                    &devctx_memory::group_project(g),
                    &b.query,
                    b.repo.as_deref(),
                    limit,
                )
                .map_err(|e| e.to_string())?,
            None => c
                .recall(&b.query, b.repo.as_deref(), limit)
                .map_err(|e| e.to_string())?,
        };
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

/// Fetch specific memories by id.
///
/// The memory↔graph junction lives in the project store while a global or group
/// memory lives here, so resolving a link means asking this daemon for a
/// handful of ids. By id rather than by scan because the caller already knows
/// exactly which ones it wants, and because opening this database directly to
/// read them is not available — the daemon exists precisely because DuckDB
/// permits one writer and this is it.
///
/// Ids that no longer exist are simply absent from the answer: a junction row
/// outliving the memory it points at is expected, not an error.
/// Permanently delete one shared memory and its vectors.
async fn forget_memory(State(api): State<CentralApi>, Path(id): Path<String>) -> Response {
    run(api, move |c| {
        let gone = c.store().forget_memory(&id).map_err(|e| e.to_string())?;
        Ok(json!({ "id": id, "forgotten": gone }).to_string())
    })
    .await
}

async fn memories_by_id(State(api): State<CentralApi>, Json(body): Json<ByIdBody>) -> Response {
    run(api, move |c| {
        let mut out = Vec::new();
        for id in &body.ids {
            match c.store().get_memory(id) {
                Ok(Some(m)) if m.deleted_at.is_none() => {
                    out.push(serde_json::to_value(&m).map_err(|e| e.to_string())?)
                }
                Ok(_) => {}
                Err(e) => return Err(e.to_string()),
            }
        }
        Ok(json!({ "memories": out }).to_string())
    })
    .await
}

/// Shared memories whose text or `files` field mentions `label`.
///
/// The fallback half of a symbol lookup. Literal rather than semantic on
/// purpose: the caller has an exact identifier and wants the memories that
/// contain it, not the ones that are vaguely about the same area — semantic
/// recall on a bare name like `charge` returns everything about payments.
async fn memories_mentioning(
    State(api): State<CentralApi>,
    Json(body): Json<MentioningBody>,
) -> Response {
    run(api, move |c| {
        let found = c
            .store()
            .memories_mentioning(&body.label, body.limit.unwrap_or(20))
            .map_err(|e| e.to_string())?;
        let out: Vec<_> = found
            .iter()
            .map(serde_json::to_value)
            .collect::<std::result::Result<_, _>>()
            .map_err(|e| e.to_string())?;
        Ok(json!({ "memories": out }).to_string())
    })
    .await
}

async fn memories(State(api): State<CentralApi>, Query(q): Query<MemoriesQuery>) -> Response {
    run(api, move |c| {
        let mems = c
            // limit=0 means every shared memory, for a backfill sweep that
            // must not stop at the newest page.
            .shared_memories(q.limit.unwrap_or(20))
            .map_err(|e| e.to_string())?;
        Ok(json!({
            "memories": mems.iter().map(|m| json!({
                "id": m.id,
                "title": m.title,
                "content": m.content,
                "type": m.memory_type,
                "tags": m.tags,
                "repo": m.repo,
                // Carried for the linker: without `files` a backfill has
                // nothing to resolve, and without `branch` the junction rows it
                // writes cannot be narrowed by branch afterwards.
                "files": m.files,
                "branch": m.branch,
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
