//! `devctx-api` — an HTTP REST API for DevCtxEngine (axum), reusing the MCP engine.
//!
//! Every route delegates to the same `do_*` handlers the MCP server uses, which
//! return JSON strings. Optional Bearer-token auth guards all routes except
//! `/health`. See `docs/architecture-spec.md`.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::extract::{Path, Query, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use devctx_core::config::ProjectConfig;
use devctx_mcp::state::{
    do_backfill_links, do_build_context, do_graph, do_impact, do_index_on, do_index_paths,
    do_index_progress, do_index_status, do_list_projects, do_memories_by_file,
    do_memories_by_symbol, do_memory_context, do_memory_forget, do_memory_move, do_memory_refs,
    do_memory_stats, do_plan_graph, do_plan_status, do_read_file, do_read_symbol, do_recall_scoped,
    do_references, do_remember, do_remember_shared, do_routes_for_handler, do_search,
    do_search_routes, do_summarize, parse_mode, AppState, MemoriesOpts, Page, PlanListOpts,
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
        .route("/memory/forget", post(memory_forget))
        .route("/memory/move", post(memory_move))
        .route("/memory/stats", get(memory_stats))
        .route("/projects", get(list_projects))
        .route("/graph", get(graph))
        .route("/plans/status", get(plans_status))
        .route("/plans/graph", get(plans_graph))
        .route("/impact/:symbol", get(impact))
        .route("/references/:symbol", get(references))
        .route("/memories/by-symbol/:symbol", get(memories_by_symbol))
        .route("/memories/by-file/:file", get(memories_by_file))
        .route("/memory/:id/refs", get(memory_refs))
        .route("/symbol/:name", get(read_symbol))
        .route("/memory/context", get(memory_context))
        .route("/memories/backfill-links", post(backfill_links))
        .route("/context", post(build_context))
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
    serve_with(cfg, addr, token, idle, || Ok(()), || {}).await
}

/// [`serve`] with a hook that runs once the store is open and the port is
/// bound — the moment this process really owns the database and can be
/// advertised. A failing hook aborts the start.
///
/// `on_exit` undoes that advertisement. It runs on the paths that end the
/// process from a watchdog thread (idle timeout, a stop request that could not
/// unwind in time), where the caller's own cleanup after `serve` returns never
/// gets to run.
pub async fn serve_with(
    cfg: ProjectConfig,
    addr: SocketAddr,
    token: Option<String>,
    idle: Option<Duration>,
    ready: impl FnOnce() -> anyhow::Result<()>,
    on_exit: impl Fn() + Send + Sync + 'static,
) -> anyhow::Result<()> {
    let on_exit: ExitHook = Arc::new(on_exit);
    let marker = checkpoint_marker(&cfg);
    let state = Arc::new(AppState::build(cfg)?);
    let life = Arc::new(Lifecycle {
        marker: Some(marker),
        ..Default::default()
    });
    let activity = Arc::new(Mutex::new(Instant::now()));
    let app = router(Api {
        state: state.clone(),
        token,
    })
    .layer(middleware::from_fn_with_state(activity.clone(), track))
    .layer(middleware::from_fn_with_state(
        life.clone(),
        count_in_flight,
    ));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| anyhow::anyhow!("binding {addr}: {e}"))?;
    ready()?;
    eprintln!("DevCtxEngine API listening on http://{addr}");

    if let Some(timeout) = idle {
        spawn_idle_watchdog(
            timeout,
            activity.clone(),
            state.clone(),
            life.clone(),
            on_exit.clone(),
        );
    }
    // Independent of `--idle`: a server whose project was deleted serves
    // nothing anyone can reach again, idle window or not.
    spawn_vanish_watchdog(state.clone(), life.clone(), on_exit.clone());

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
    // checkpoint before the connection disappears. See [`Lifecycle`] for the
    // whole timeline.
    let closing = state.clone();
    let hook = on_exit.clone();
    let stopping = life.clone();
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = terminate().await;
            eprintln!("DevCtxEngine server terminating; checkpointing.");
            stopping.mark_stop_requested();
            // An `/index` in flight stops at its next file, commits, folds its
            // WAL and answers — which is what lets graceful shutdown finish.
            closing.cancel_indexing();
            arm_exit_watchdog(closing, stopping, hook);
        })
        .await?;

    // A request whose client went away keeps running on the blocking pool:
    // give a cancelled index the same bounded wait the watchdog gives it, so
    // the final checkpoint does not abort its last file.
    while life.within_hard_cap() && state.index_winding_down(INDEX_CANCEL_QUIET) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // Claimed, not just announced: a watchdog that read "not started" a moment
    // ago may be entering `exit_now` right now, and two checkpoints racing —
    // one of them cut short by the other's `_exit` — is the failure this
    // state exists to prevent.
    if !life.claim_orderly_checkpoint() {
        // `exit_now` owns the exit and ends the process within its budget.
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
    life.begin_checkpoint_marker();
    // Frozen for good: the runtime's wind-down below may still run blocking
    // tasks (a request whose client left, a cancelled index past the cap),
    // and none of them may write after this checkpoint.
    state.checkpoint_for_exit(ORDERLY_FREEZE_WAIT);
    life.end_checkpoint_marker();
    life.final_checkpoint.store(CKPT_DONE, Ordering::SeqCst);
    Ok(())
}

/// The file a stopping server holds while it takes its final checkpoint,
/// containing its pid: `serve.checkpointing`, next to `serve.json`.
///
/// `devctx serve --stop` cannot ask the server over HTTP by then — the
/// listener is the first thing a stop closes — so this is how it learns that
/// the process it is waiting on is folding its WAL, and keeps waiting rather
/// than sending SIGKILL into the middle of it.
pub fn checkpoint_marker(cfg: &ProjectConfig) -> std::path::PathBuf {
    let db = cfg.db_path();
    db.parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("serve.checkpointing")
}

/// What a server undoes about itself before a watchdog ends the process
/// (removing its `serve.json`).
type ExitHook = Arc<dyn Fn() + Send + Sync>;

/// How long a stop request has to complete in an orderly way before the
/// watchdog may end the process. `DEVCTX_SHUTDOWN_GRACE_SECS` overrides it,
/// downwards only: see [`STOP_TIMELINE`].
const DEFAULT_SHUTDOWN_GRACE_SECS: u64 = 3;

/// How long the exit checkpoint may take before the process leaves without it.
const CHECKPOINT_BUDGET: Duration = Duration::from_millis(1500);

/// The longest a server with no index in flight takes to end after SIGTERM:
/// the grace (at most [`DEFAULT_SHUTDOWN_GRACE_SECS`]) plus the exit
/// checkpoint's budget. `devctx serve --stop` waits this plus a margin before
/// SIGKILL. The grace override is clamped to the default so a larger value
/// cannot push the timeline past what the client waits — which used to land
/// SIGKILL in the middle of the checkpoint the stop was waiting for.
pub const STOP_TIMELINE: Duration = Duration::from_millis(
    DEFAULT_SHUTDOWN_GRACE_SECS * 1000 + CHECKPOINT_BUDGET.as_millis() as u64,
);

/// How long the exit path waits for a write already running before it
/// checkpoints regardless: a slice of [`CHECKPOINT_BUDGET`], since the
/// checkpoint itself must fit in the rest.
const EXIT_FREEZE_WAIT: Duration = Duration::from_millis(500);

/// The same wait on the orderly path, which has no budget to fit in.
const ORDERLY_FREEZE_WAIT: Duration = Duration::from_secs(5);

/// The longest a stop request waits for a cancelled indexing run (or the final
/// checkpoint) that is still making progress. `devctx serve --stop` waits this
/// long, plus a margin, before escalating to SIGKILL when the server reports
/// an index in flight.
pub const INDEX_CANCEL_CAP: Duration = Duration::from_secs(60);

/// A cancelled run that has not reported progress for this long is not
/// winding down, it is stuck. Comfortably above the pipeline's heartbeat (5 s).
const INDEX_CANCEL_QUIET: Duration = Duration::from_secs(15);

/// How long the runtime may take to wind down its blocking pool on the way out.
const RUNTIME_WIND_DOWN: Duration = Duration::from_secs(5);

/// How often the vanished-project check runs. `DEVCTX_VANISH_POLL_MS`
/// overrides it (the tests use a fraction of a second).
const DEFAULT_VANISH_POLL: Duration = Duration::from_secs(10);

// `Lifecycle::final_checkpoint`.
/// Not started (the default).
const CKPT_IDLE: u8 = 0;
/// The orderly path is taking the final checkpoint.
const CKPT_RUNNING: u8 = 1;
/// The final checkpoint is taken; nothing is left to save.
const CKPT_DONE: u8 = 2;
/// `exit_now` claimed the checkpoint and is ending the process.
const CKPT_EXITING: u8 = 3;

/// Where the server is in its shutdown, shared by the orderly path and the
/// watchdog threads.
///
/// The timeline of a stop request (`SIGTERM`, `devctx serve --stop`):
///
/// * **t = 0** — stop accepting connections, raise the indexing cancel flag,
///   arm the stop watchdog. An `/index` in flight stops at its next file
///   boundary, commits, checkpoints and answers; graceful shutdown then sees
///   no connection left, and the orderly path takes the final checkpoint and
///   returns.
/// * **t = grace (3 s)** — if the process is still here, the watchdog looks at
///   why. The final checkpoint is running → wait for it. A cancelled index is
///   still reporting progress (within 15 s: finishing a file, folding its
///   WAL) → wait for it. Otherwise (a request stuck on the network, a run that
///   stopped moving) → exit now: checkpoint within 1.5 s, escalating to
///   `FORCE CHECKPOINT` if a transaction blocks it, then `_exit`.
/// * **t = 60 s** ([`INDEX_CANCEL_CAP`]) — exit now regardless.
///
/// `serve --stop` waits [`STOP_TIMELINE`] (grace + checkpoint budget, 4.5 s)
/// plus a margin before SIGKILL, `INDEX_CANCEL_CAP` + 5 s when the server says
/// an index is in flight, and — whichever it chose — keeps waiting while the
/// server holds its [`checkpoint_marker`], so the signal never lands in the
/// middle of the checkpoint it is waiting for.
///
/// The final checkpoint is taken exactly once, by whichever of the orderly
/// path and `exit_now` claims [`Lifecycle::final_checkpoint`] first; the other
/// waits for it.
#[derive(Default)]
struct Lifecycle {
    /// Non-health requests being served right now.
    in_flight: AtomicUsize,
    /// When the stop request arrived.
    stop_requested: OnceLock<Instant>,
    /// `CKPT_*`: the orderly path's final checkpoint.
    final_checkpoint: AtomicU8,
    /// Set by the first thread to start ending the process.
    exiting: AtomicBool,
    /// [`checkpoint_marker`] for this server; `None` in tests without one.
    marker: Option<std::path::PathBuf>,
}

impl Lifecycle {
    fn mark_stop_requested(&self) {
        let _ = self.stop_requested.set(Instant::now());
    }

    fn stopping(&self) -> bool {
        self.stop_requested.get().is_some()
    }

    fn since_stop(&self) -> Duration {
        self.stop_requested
            .get()
            .map(Instant::elapsed)
            .unwrap_or_default()
    }

    fn within_hard_cap(&self) -> bool {
        self.since_stop() < INDEX_CANCEL_CAP
    }

    /// The orderly path's claim on the final checkpoint; `false` when
    /// `exit_now` got there first (and is ending the process).
    fn claim_orderly_checkpoint(&self) -> bool {
        self.final_checkpoint
            .compare_exchange(CKPT_IDLE, CKPT_RUNNING, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    /// `exit_now`'s claim on the final checkpoint.
    fn claim_exit_checkpoint(&self) -> ExitClaim {
        match self.final_checkpoint.compare_exchange(
            CKPT_IDLE,
            CKPT_EXITING,
            Ordering::SeqCst,
            Ordering::SeqCst,
        ) {
            Ok(_) => ExitClaim::Ours,
            Err(CKPT_RUNNING) => ExitClaim::OrderlyRunning,
            Err(_) => ExitClaim::Done,
        }
    }

    /// Wait (at most `cap`) for the orderly path's checkpoint to finish.
    fn wait_orderly_checkpoint(&self, cap: Duration) {
        let deadline = Instant::now() + cap;
        while self.final_checkpoint.load(Ordering::SeqCst) == CKPT_RUNNING
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Say "checkpointing" to a `devctx serve --stop` waiting on this process.
    fn begin_checkpoint_marker(&self) {
        if let Some(m) = &self.marker {
            let _ = std::fs::write(m, std::process::id().to_string());
        }
    }

    fn end_checkpoint_marker(&self) {
        if let Some(m) = &self.marker {
            let _ = std::fs::remove_file(m);
        }
    }
}

/// What `exit_now` found when it went to take the final checkpoint.
#[derive(Debug, PartialEq, Eq)]
enum ExitClaim {
    /// Nobody had started it: `exit_now` takes it.
    Ours,
    /// The orderly path is taking it: wait, do not `_exit` over it.
    OrderlyRunning,
    /// Already taken (or being taken by another exit): just leave.
    Done,
}

/// Middleware: count the non-health requests in flight (the vanished-project
/// check does not end a server that is answering someone).
async fn count_in_flight(State(life): State<Arc<Lifecycle>>, req: Request, next: Next) -> Response {
    if req.uri().path() == "/health" {
        return next.run(req).await;
    }
    struct Guard(Arc<Lifecycle>);
    impl Drop for Guard {
        fn drop(&mut self) {
            self.0.in_flight.fetch_sub(1, Ordering::SeqCst);
        }
    }
    life.in_flight.fetch_add(1, Ordering::SeqCst);
    let _guard = Guard(life.clone());
    next.run(req).await
}

fn shutdown_grace() -> Duration {
    grace_from(std::env::var("DEVCTX_SHUTDOWN_GRACE_SECS").ok().as_deref())
}

/// The grace for an override value; never above the default, which is what
/// [`STOP_TIMELINE`] — and so the client's SIGKILL — is built on.
fn grace_from(value: Option<&str>) -> Duration {
    let secs: u64 = value
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_SHUTDOWN_GRACE_SECS)
        .min(DEFAULT_SHUTDOWN_GRACE_SECS);
    Duration::from_secs(secs)
}

fn vanish_poll() -> Duration {
    std::env::var("DEVCTX_VANISH_POLL_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_VANISH_POLL)
}

/// End the process without running C++ static destructors.
///
/// `exit` runs `atexit` handlers and static destructors, DuckDB's among them,
/// while other threads (an indexing run, the blocking pool) may still be inside
/// DuckDB — the suspected source of the exit-time segfaults (status 139). Every
/// caller has already checkpointed, and nothing else needs flushing but stdout.
fn hard_exit(code: i32) -> ! {
    use std::io::Write;
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    #[cfg(unix)]
    // SAFETY: `_exit` takes an integer and never returns; it touches no Rust
    // state, which is the point.
    unsafe {
        libc::_exit(code)
    }
    #[cfg(not(unix))]
    std::process::exit(code)
}

/// Freeze and fold the write-ahead log, undo the advertisement and end the
/// process.
///
/// The final checkpoint happens once: if the orderly path already claimed it
/// ([`CKPT_RUNNING`]) this waits for it to finish — up to [`INDEX_CANCEL_CAP`]
/// — instead of `_exit`ing in the middle of it; if it is done, this just
/// leaves. Otherwise this claims it, atomically, so the orderly path cannot
/// start one behind it.
///
/// The advertisement goes first (`on_exit`), then the database is frozen and
/// checkpointed on a helper thread that `_exit`s itself the moment the
/// checkpoint returns: frozen, no write can land after the checkpoint, and
/// ending on that same thread leaves no window for one either. The calling
/// thread is the budget: a checkpoint that blocks (a stuck connection, a
/// poisoned lock) must not turn a watchdog into one more thing that hangs, so
/// after [`CHECKPOINT_BUDGET`] it leaves without it. The checkpoint escalates
/// to `FORCE CHECKPOINT` when an open transaction refuses the plain one — see
/// [`AppState::checkpoint_for_exit`].
fn exit_now(state: &Arc<AppState>, life: &Lifecycle, on_exit: &ExitHook, code: i32) -> ! {
    if life.exiting.swap(true, Ordering::SeqCst) {
        // Another watchdog is already ending the process.
        loop {
            std::thread::park();
        }
    }
    state.cancel_indexing();
    match life.claim_exit_checkpoint() {
        ExitClaim::Ours => {}
        ExitClaim::OrderlyRunning => {
            life.wait_orderly_checkpoint(INDEX_CANCEL_CAP);
            on_exit();
            hard_exit(code)
        }
        ExitClaim::Done => {
            on_exit();
            hard_exit(code)
        }
    }
    on_exit();
    life.begin_checkpoint_marker();
    let s = state.clone();
    let marker = life.marker.clone();
    let _ = std::thread::Builder::new()
        .name("exit-checkpoint".into())
        .spawn(move || {
            s.checkpoint_for_exit(EXIT_FREEZE_WAIT);
            if let Some(m) = &marker {
                let _ = std::fs::remove_file(m);
            }
            hard_exit(code)
        });
    std::thread::sleep(CHECKPOINT_BUDGET);
    eprintln!(
        "DevCtxEngine: the exit checkpoint did not finish within {CHECKPOINT_BUDGET:?}; \
         leaving without it"
    );
    life.end_checkpoint_marker();
    hard_exit(code)
}

/// After a stop request, end the process if the orderly shutdown has not —
/// following the timeline on [`Lifecycle`].
fn arm_exit_watchdog(state: Arc<AppState>, life: Arc<Lifecycle>, on_exit: ExitHook) {
    let grace = shutdown_grace();
    let _ = std::thread::Builder::new()
        .name("stop-watchdog".into())
        .spawn(move || {
            let mut said_waiting = false;
            loop {
                std::thread::sleep(Duration::from_millis(100));
                let elapsed = life.since_stop();
                if elapsed < grace {
                    continue;
                }
                match life.final_checkpoint.load(Ordering::SeqCst) {
                    // Checkpointed, and only the runtime's wind-down is left:
                    // nothing to save, just leave.
                    CKPT_DONE => {
                        on_exit();
                        hard_exit(0);
                    }
                    // Never cut the final checkpoint short: that is the exact
                    // WAL this whole shutdown exists to fold. (`exit_now`
                    // re-checks under a claim, so reading "not started" here
                    // just before the orderly path starts is safe too.)
                    CKPT_RUNNING if elapsed < INDEX_CANCEL_CAP => continue,
                    _ => {}
                }
                if elapsed < INDEX_CANCEL_CAP && state.index_winding_down(INDEX_CANCEL_QUIET) {
                    if !said_waiting {
                        eprintln!(
                            "DevCtxEngine: waiting for the cancelled index to commit and \
                             checkpoint (at most {}s after the stop request).",
                            INDEX_CANCEL_CAP.as_secs()
                        );
                        said_waiting = true;
                    }
                    continue;
                }
                eprintln!(
                    "DevCtxEngine server did not stop within {:.1}s of the request \
                     (a blocking task is stuck); forcing exit.",
                    elapsed.as_secs_f32()
                );
                exit_now(&state, &life, &on_exit, 0);
            }
        });
}

/// Exit after `timeout` with no non-health request.
///
/// A plain thread, not a task: a task dies with the runtime's workers, and the
/// one timer whose whole job is to end a process that is not behaving must not
/// depend on the runtime behaving.
fn spawn_idle_watchdog(
    timeout: Duration,
    act: Arc<Mutex<Instant>>,
    state: Arc<AppState>,
    life: Arc<Lifecycle>,
    on_exit: ExitHook,
) {
    // Polling every 30 s is plenty for a window of minutes; a short window (the
    // tests use seconds) has to be noticed within a fraction of itself.
    let poll = (timeout / 4).clamp(Duration::from_millis(250), Duration::from_secs(30));
    let _ = std::thread::Builder::new()
        .name("idle-watchdog".into())
        .spawn(move || loop {
            std::thread::sleep(poll);
            // A stop is under way: its own watchdog owns the exit.
            if life.stopping() {
                return;
            }
            // A poisoned lock means a thread panicked holding it, not that a
            // request just arrived. Reading that as `unwrap_or_default()` —
            // `Duration::ZERO`, i.e. "busy right now" — silenced this timer
            // for the rest of the process's life, which is the exact runaway
            // it exists to prevent: the server then lingers forever, holding
            // an embedding model and a reranker in memory. A panic cannot
            // leave an `Instant` half-written, so the value behind the lock
            // is still sound. Take it and carry on.
            let since_request = match act.lock() {
                Ok(t) => t.elapsed(),
                Err(poisoned) => poisoned.into_inner().elapsed(),
            };
            // An indexing run's last move (or its end) counts as activity: its
            // request was stamped when it started, and the run is still
            // answering for a while after it marks itself finished.
            let idle_for = state
                .last_index_activity()
                .map_or(since_request, |at| since_request.min(at.elapsed()));
            if idle_for < timeout {
                continue;
            }
            // "No requests lately" is not the same as "nothing to do".
            // Indexing runs inside this process and can take far longer than
            // the idle window; the client that asked for it stops polling as
            // soon as its own read times out, and exiting here discarded a
            // nearly complete run. Only a run that is still advancing counts:
            // one that has not moved for a long time is stuck, and a stuck run
            // must not make the server immortal.
            if state.is_indexing() {
                continue;
            }
            eprintln!("DevCtxEngine server idle for {idle_for:?}; shutting down.");
            exit_now(&state, &life, &on_exit, 0);
        });
}

/// Exit once the project directory is really gone.
///
/// [`AppState::project_vanished`] only answers yes after repeated, definite
/// "not found" results; on top of that the exit waits for requests and an
/// advancing index to finish — a server mid-answer is not abandoned on a
/// filesystem's say-so.
fn spawn_vanish_watchdog(state: Arc<AppState>, life: Arc<Lifecycle>, on_exit: ExitHook) {
    let poll = vanish_poll();
    let _ = std::thread::Builder::new()
        .name("vanish-watchdog".into())
        .spawn(move || loop {
            std::thread::sleep(poll);
            if life.stopping() {
                return;
            }
            if !state.project_vanished() {
                continue;
            }
            if life.in_flight.load(Ordering::SeqCst) > 0 || state.is_indexing() {
                continue;
            }
            eprintln!("DevCtxEngine project directory is gone; shutting down.");
            exit_now(&state, &life, &on_exit, 0);
        });
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
    if req.uri().path() == "/health" {
        return next.run(req).await;
    }
    // Recover from poisoning rather than skip the write: dropping it would
    // freeze the clock at the last successful request, and a busy server
    // would then look idle and shut down under load.
    let stamp = || *act.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Instant::now();
    stamp();
    let response = next.run(req).await;
    // And again on the way out: the idle window is "time since the last
    // answer", not since a long request began.
    stamp();
    response
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
    let result = rt.block_on(serve(cfg, addr, token, idle));
    // Dropping a runtime waits, without a limit, for its blocking pool. Bound it.
    rt.shutdown_timeout(RUNTIME_WIND_DOWN);
    result
}

/// [`run_blocking`] with the [`serve_with`] hooks.
pub fn run_blocking_ready(
    cfg: ProjectConfig,
    addr: SocketAddr,
    token: Option<String>,
    idle: Option<Duration>,
    ready: impl FnOnce() -> anyhow::Result<()>,
    on_exit: impl Fn() + Send + Sync + 'static,
) -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = rt.block_on(serve_with(cfg, addr, token, idle, ready, on_exit));
    // The implicit drop waited for the blocking pool with no limit: a model
    // download that never answered kept a "stopped" server alive forever.
    rt.shutdown_timeout(RUNTIME_WIND_DOWN);
    result
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
    /// The branch to index; absent means the project's declared default, then
    /// whatever is checked out.
    #[serde(default)]
    branch: Option<String>,
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
    /// Comma-separated files the memory is about, used to link it to the graph.
    #[serde(default)]
    files: Option<String>,
    /// Who the memory should be attributed to, overriding what git reports.
    ///
    /// Only a group-bound MCP session sends this: it is writing on behalf of a
    /// product rather than a repository, and letting git answer would credit
    /// whichever member happens to be the default.
    #[serde(default)]
    provenance: Option<String>,
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
struct MemoryForgetBody {
    id: String,
}

#[derive(Deserialize)]
struct MemoryMoveBody {
    id: String,
    to: String,
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
    /// Routes per page (default 20).
    #[serde(default)]
    limit: Option<usize>,
    /// Skip this many routes (the `next_offset` of the previous page).
    #[serde(default)]
    offset: Option<usize>,
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
struct MemoryContextQuery {
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

/// Body for `POST /memories/backfill-links`.
#[derive(Deserialize)]
struct BackfillBody {
    #[serde(default)]
    dry_run: Option<bool>,
    /// Also recover paths from a memory's prose when it names no files.
    #[serde(default)]
    from_text: Option<bool>,
}

/// Body for `POST /context`.
#[derive(Deserialize)]
struct ContextBody {
    query: String,
    #[serde(default)]
    max_tokens: Option<usize>,
    #[serde(default)]
    include_memories: Option<bool>,
}

#[derive(Deserialize)]
struct MemoriesQuery {
    #[serde(default)]
    limit: Option<usize>,
    /// By-symbol / by-file only: skip this many memories.
    #[serde(default)]
    offset: Option<usize>,
    /// By-symbol / by-file only: keep each memory's whole content.
    #[serde(default)]
    full: Option<bool>,
}

#[derive(Deserialize)]
struct PlanStatusQuery {
    #[serde(default)]
    plan: Option<String>,
    /// Listing only: plans with unresolved tasks.
    #[serde(default)]
    active_only: Option<bool>,
    /// Listing only: plans per page (default 25).
    #[serde(default)]
    limit: Option<usize>,
    /// Listing only: skip this many plans.
    #[serde(default)]
    offset: Option<usize>,
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

async fn plans_status(State(api): State<Api>, Query(q): Query<PlanStatusQuery>) -> Response {
    let opts = PlanListOpts {
        active_only: q.active_only.unwrap_or(false),
        page: Page::new(q.limit, q.offset),
    };
    run(api.state, move |s| {
        do_plan_status(s, q.plan.as_deref(), opts)
    })
    .await
}

async fn plans_graph(State(api): State<Api>, Query(q): Query<PlanStatusQuery>) -> Response {
    run(api.state, move |s| do_plan_graph(s, q.plan.as_deref())).await
}

async fn memories(State(api): State<Api>, Query(q): Query<MemoriesQuery>) -> Response {
    run(api.state, move |s| {
        // Explicitly local: this endpoint predates the shared tier and its
        // callers expect this project's memories, not everyone's.
        do_memory_context(s, "local", q.limit.unwrap_or(25))
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
    run(api.state, move |s| {
        do_index_on(s, b.full.unwrap_or(false), b.branch.clone())
    })
    .await
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
        let files = b.files.clone().unwrap_or_default();
        // `group` and `global` both live in the central store; only the space
        // they land in differs.
        if devctx_memory::is_global(scope) || devctx_memory::is_group(scope) {
            let group = if devctx_memory::is_group(scope) {
                s.group_name()
            } else {
                String::new()
            };
            return do_remember_shared(
                s,
                &b.content,
                &title,
                &memory_type,
                &topic,
                &tags,
                &group,
                &files,
                // Absent, the bound repository IS the provenance — the API
                // serves one project's store. A group-bound MCP session sends
                // the override because it is writing for the product.
                b.provenance.as_deref(),
            );
        }
        do_remember(s, b.content, title, memory_type, topic, tags, files)
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

/// Link memories saved before the junction existed.
async fn backfill_links(State(api): State<Api>, Json(b): Json<BackfillBody>) -> Response {
    run(api.state, move |s| {
        do_backfill_links(s, b.dry_run.unwrap_or(false), b.from_text.unwrap_or(false))
    })
    .await
}

/// The most recent memories, with no query.
async fn memory_context(State(api): State<Api>, Query(q): Query<MemoryContextQuery>) -> Response {
    run(api.state, move |s| {
        do_memory_context(
            s,
            q.scope.as_deref().unwrap_or("all"),
            q.limit.unwrap_or(20),
        )
    })
    .await
}

/// A symbol's definition and code.
async fn read_symbol(
    State(api): State<Api>,
    Path(name): Path<String>,
    Query(q): Query<MemoriesQuery>,
) -> Response {
    run(api.state, move |s| {
        do_read_symbol(s, &name, q.limit.unwrap_or(5))
    })
    .await
}

/// One budgeted brief assembled for a question.
async fn build_context(State(api): State<Api>, Json(b): Json<ContextBody>) -> Response {
    run(api.state, move |s| {
        do_build_context(
            s,
            &b.query,
            b.max_tokens.unwrap_or(4096),
            b.include_memories.unwrap_or(true),
        )
    })
    .await
}

fn memories_opts(q: &MemoriesQuery) -> MemoriesOpts {
    MemoriesOpts {
        page: Page::new(q.limit, q.offset),
        full: q.full.unwrap_or(false),
    }
}

/// The memories recorded about a symbol — the memory↔graph join.
async fn memories_by_symbol(
    State(api): State<Api>,
    Path(symbol): Path<String>,
    Query(q): Query<MemoriesQuery>,
) -> Response {
    run(api.state, move |s| {
        do_memories_by_symbol(s, &symbol, memories_opts(&q))
    })
    .await
}

/// The memories recorded about a file.
async fn memories_by_file(
    State(api): State<Api>,
    Path(file): Path<String>,
    Query(q): Query<MemoriesQuery>,
) -> Response {
    run(api.state, move |s| {
        do_memories_by_file(s, &file, memories_opts(&q))
    })
    .await
}

/// The inverse: what code one memory concerns.
async fn memory_refs(State(api): State<Api>, Path(id): Path<String>) -> Response {
    run(api.state, move |s| do_memory_refs(s, &id)).await
}

/// Permanently delete one memory, wherever it lives (this project or the
/// shared store). Was missing entirely: `Backend::Remote::memory_forget`
/// (`backend.rs`) POSTs here, and until this route existed every call routed
/// through a per-project daemon fell through to axum's 404 fallback — the
/// tool call reads as "the memory does not exist" when it is really "this
/// route does not exist".
async fn memory_forget(State(api): State<Api>, Json(b): Json<MemoryForgetBody>) -> Response {
    run(api.state, move |s| do_memory_forget(s, &b.id)).await
}

/// Move a memory to another tier (local/group/global) or another repository.
/// Missing for the same reason as `memory_forget` above.
async fn memory_move(State(api): State<Api>, Json(b): Json<MemoryMoveBody>) -> Response {
    run(api.state, move |s| do_memory_move(s, &b.id, &b.to)).await
}

async fn references(State(api): State<Api>, Path(symbol): Path<String>) -> Response {
    run(api.state, move |s| do_references(s, &symbol)).await
}

async fn routes(State(api): State<Api>, Query(q): Query<RoutesQuery>) -> Response {
    let page = Page::new(q.limit, q.offset);
    run(api.state, move |s| {
        do_search_routes(s, q.method, q.path, page)
    })
    .await
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode as HttpStatus};
    use tower::ServiceExt;

    /// D1b item 7: the grace override only shortens the timeline. A larger
    /// value used to push "grace + checkpoint budget" past the 5 s the client
    /// waits, so SIGKILL landed in the middle of the exit checkpoint.
    #[test]
    fn the_grace_override_cannot_outgrow_the_stop_timeline() {
        assert_eq!(grace_from(None), Duration::from_secs(3));
        assert_eq!(grace_from(Some("1")), Duration::from_secs(1));
        assert_eq!(grace_from(Some("0")), Duration::ZERO);
        assert_eq!(grace_from(Some("30")), Duration::from_secs(3));
        assert_eq!(grace_from(Some("junk")), Duration::from_secs(3));
        assert!(grace_from(Some("99")) + CHECKPOINT_BUDGET <= STOP_TIMELINE);
    }

    /// D1b item 5: the watchdog may read "not started" just before the
    /// orderly path starts its checkpoint. The claim is atomic, so whichever
    /// arrives second finds out — and `exit_now` waits for a running
    /// checkpoint instead of `_exit`ing in the middle of it.
    #[test]
    fn the_final_checkpoint_is_claimed_once() {
        let life = Lifecycle::default();
        assert!(life.claim_orderly_checkpoint());
        assert_eq!(life.claim_exit_checkpoint(), ExitClaim::OrderlyRunning);
        assert!(!life.claim_orderly_checkpoint(), "never twice");

        // exit_now waits for it, and only as long as it runs.
        let life = Arc::new(Lifecycle::default());
        assert!(life.claim_orderly_checkpoint());
        let finishing = life.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            finishing
                .final_checkpoint
                .store(CKPT_DONE, Ordering::SeqCst);
        });
        let t0 = Instant::now();
        life.wait_orderly_checkpoint(Duration::from_secs(10));
        let waited = t0.elapsed();
        t.join().unwrap();
        assert!(
            waited >= Duration::from_millis(250) && waited < Duration::from_secs(5),
            "{waited:?}"
        );
        assert_eq!(life.claim_exit_checkpoint(), ExitClaim::Done);

        // The other order: exit_now first, the orderly path stands aside.
        let life = Lifecycle::default();
        assert_eq!(life.claim_exit_checkpoint(), ExitClaim::Ours);
        assert!(!life.claim_orderly_checkpoint());
        assert_eq!(life.claim_exit_checkpoint(), ExitClaim::Done);
    }

    fn test_cfg(dir: &std::path::Path) -> ProjectConfig {
        let path = dir.to_string_lossy().to_string();
        ProjectConfig {
            state_dir: path.clone(),
            project: devctx_core::config::Project {
                path,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// PLAN-006 TASK-003: `Backend::Remote::memory_forget`
    /// (`devctx-mcp/src/backend.rs`) POSTs to `/memory/forget` on the
    /// per-project daemon. That route did not exist on this router at all —
    /// no `.route("/memory/forget", …)`, no handler — so every call fell
    /// through to axum's built-in 404, which is what a user saw and reported
    /// as "memory_forget returns 404 on group-scope memories". The bug was
    /// never about store scoping (`Store::forget_memory` deletes by bare
    /// `id`, no project filter — see `devctx-store/src/memory.rs`); it
    /// reproduces for ANY memory forgotten through a routed (`Remote`)
    /// session, which is the default mode `devctx mcp` runs in.
    ///
    /// This seeds a memory directly into the store (no embedder needed —
    /// `upsert_memory` only touches the `memories` table) and forgets it
    /// through the real HTTP router, so a regression shows up as this test
    /// failing with 404 again rather than as a 500 from a store error.
    #[tokio::test]
    async fn memory_forget_route_exists_and_forgets_a_real_memory() {
        let dir =
            std::env::temp_dir().join(format!("devctx_api_forget_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = test_cfg(&dir);

        let dim = devctx_embed::dimension_for(&cfg.embeddings.provider, &cfg.embeddings.model);
        {
            // Own connection, closed before `AppState::build` opens its own —
            // DuckDB allows only one writer at a time.
            let store = devctx_store::Store::open(&cfg.db_path(), dim).unwrap();
            let m = devctx_store::Memory {
                id: "mem_test_forget_me".into(),
                title: "t".into(),
                content: "c".into(),
                memory_type: "note".into(),
                project: "default".into(),
                ..Default::default()
            };
            store.upsert_memory(&m).unwrap();
        }

        // No central daemon in this test: the local store already has the
        // memory, so `do_memory_forget`'s local path answers before it would
        // ever need one.
        std::env::set_var("DEVCTX_NO_AUTOSERVE", "1");
        let state = Arc::new(AppState::build(cfg).expect("build test AppState"));
        let app = router(Api { state, token: None });

        let req = Request::builder()
            .method("POST")
            .uri("/memory/forget")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({ "id": "mem_test_forget_me" }).to_string(),
            ))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            HttpStatus::OK,
            "the route must exist and succeed, not fall through to axum's 404"
        );
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["forgotten"], serde_json::json!(true), "{v}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
