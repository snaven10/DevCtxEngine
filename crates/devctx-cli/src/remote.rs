//! Client side of "server mode": discover a running `devctx serve` that owns
//! the DuckDB file and route commands to it over HTTP, so no second process
//! ever takes a conflicting file lock. When no server is advertised (or it is
//! unreachable) the caller falls back to opening the store directly.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use devctx_core::config::ProjectConfig;
use devctx_core::procown::{self, Ownership};
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
    /// Kernel start time of `pid` (`/proc/<pid>/stat` field 22), so a recycled
    /// pid is never mistaken for this server. Absent in files written by older
    /// servers and off Linux.
    #[serde(default)]
    pub start_time: Option<u64>,
}

/// A snapshot of the indexing run happening inside the server.
#[derive(Debug, Default, Deserialize)]
pub struct IndexProgress {
    /// Whether a run is in flight right now.
    pub running: bool,
    /// Which run these counts belong to. Defaults to zero against a server
    /// built before the field existed, which reads as "always the same run" —
    /// the same behaviour those servers already had.
    #[serde(default)]
    pub run: u64,
    /// Changes the run expects to process.
    pub total: usize,
    /// Changes it has started on.
    pub done: usize,
    /// The file it reached last.
    pub file: String,
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
        start_time: procown::start_time(std::process::id()),
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
///
/// Unconditional, so only for callers that have established the advertised
/// server is gone — [`reclaim_db`] and [`stop_server`], which kill it first and
/// wait for it to exit. A server tidying up after *itself* must use
/// [`remove_own_serve_file`].
pub fn remove_serve_file(cfg: &ProjectConfig) {
    let _ = std::fs::remove_file(serve_file(cfg));
}

/// Remove the discovery file only while it still advertises this process.
///
/// A server that fails to start — the port taken, the database held by the
/// server already running — used to delete the file on its way out, and the
/// file it deleted belonged to that healthy server. The healthy one kept
/// running, advertised nowhere, so every later command failed to discover it,
/// fell back to opening the database directly, and hit the lock it was holding.
/// The CLI was then unusable until someone found the process and killed it by
/// pid, with nothing on screen connecting the two.
///
/// Checking the pid makes the tidy-up idempotent under a race: whoever owns the
/// file removes it, and a loser removes nothing.
pub fn remove_own_serve_file(cfg: &ProjectConfig) {
    let path = serve_file(cfg);
    let ours = std::fs::read(&path)
        .ok()
        .and_then(|raw| serde_json::from_slice::<ServeInfo>(&raw).ok())
        .and_then(|info| info.pid)
        .is_some_and(|pid| pid == std::process::id());
    if ours {
        let _ = std::fs::remove_file(&path);
    }
}

/// A reachable server we can route requests to.
#[derive(Clone)]
pub struct Remote {
    base: String,
    token: Option<String>,
}

/// A deterministic loopback address per project, so auto-spawned servers for
/// different projects don't collide on one port.
fn auto_addr(cfg: &ProjectConfig) -> String {
    // FNV-1a over the project path → a stable high port.
    let mut h: u64 = 0xcbf29ce484222325;
    for b in cfg.project.path.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    // Stay below the kernel's ephemeral range (32768-60999 on Linux): any
    // outgoing connection on the machine can be holding a port in it, and a
    // server that finds its port taken never comes up.
    let port = 20000 + (h % 12000) as u16;
    format!("127.0.0.1:{port}")
}

/// Why no server could be reached for a project.
#[derive(Debug)]
pub enum EnsureError {
    /// `DEVCTX_NO_AUTOSERVE` is set and nothing is advertised.
    Disabled,
    /// A server is alive (its pid is a `devctx serve`) but did not answer in
    /// time. Never spawned over and never killed: it owns the database.
    Busy { pid: u32, addr: String },
    /// The server could not even be launched.
    Spawn(String),
    /// The spawned server exited (or never answered); `cause` is what it wrote
    /// to `serve.log`, when anything.
    Failed { cause: Option<String> },
}

impl std::fmt::Display for EnsureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EnsureError::Disabled => {
                write!(
                    f,
                    "auto-spawn is disabled (DEVCTX_NO_AUTOSERVE) and no server is running"
                )
            }
            EnsureError::Busy { pid, addr } => write!(
                f,
                "the server (pid {pid}, {addr}) is alive but not answering; it is busy or hung"
            ),
            EnsureError::Spawn(e) => write!(f, "could not launch `devctx serve`: {e}"),
            EnsureError::Failed { cause: Some(c) } => {
                write!(f, "`devctx serve` did not start: {c}")
            }
            EnsureError::Failed { cause: None } => {
                write!(f, "`devctx serve` did not start (nothing in serve.log)")
            }
        }
    }
}

impl std::error::Error for EnsureError {}

/// How long a freshly spawned server gets to answer, in steps. Not an estimate
/// of startup (that is fast) but the margin for a loaded machine; the loop
/// leaves the moment the server answers or its process exits.
const WAIT_TICK: Duration = Duration::from_millis(100);
const WAIT_TICKS: usize = 600;

/// Ensure a server is running for this project, auto-spawning one in the
/// background if needed, and return a client to it — or say why not.
///
/// Waits only as long as the spawned process lives: when it exits (a lock held
/// by someone else, a port taken, a broken config) the answer is immediate and
/// carries the last lines it wrote to `serve.log`. Every DB command routes
/// through this so the server is the single owner of the DuckDB file.
pub fn ensure_checked(cfg: &ProjectConfig) -> Result<Remote, EnsureError> {
    match probe(cfg) {
        Discovery::Up(r) => return Ok(r),
        Discovery::Busy { pid, addr } => return Err(EnsureError::Busy { pid, addr }),
        Discovery::Down => {}
    }
    if std::env::var_os("DEVCTX_NO_AUTOSERVE").is_some() {
        return Err(EnsureError::Disabled);
    }
    let spawned = spawn_server(cfg).map_err(|e| EnsureError::Spawn(format!("{e:#}")))?;
    eprintln!("· started background server (devctx serve); it stays warm for later commands");
    for _ in 0..WAIT_TICKS {
        std::thread::sleep(WAIT_TICK);
        match probe(cfg) {
            Discovery::Up(r) => return Ok(r),
            Discovery::Busy { pid, addr } => return Err(EnsureError::Busy { pid, addr }),
            Discovery::Down => {}
        }
        if spawned.exited.lock().is_ok_and(|e| e.is_some()) {
            // It may have exited because another server won the race and is up.
            if let Discovery::Up(r) = probe(cfg) {
                return Ok(r);
            }
            return Err(EnsureError::Failed {
                cause: spawned.failure_hint(),
            });
        }
    }
    Err(EnsureError::Failed {
        cause: Some(
            spawned
                .failure_hint()
                .map(|h| format!("no answer after {}s; {h}", WAIT_TICKS / 10))
                .unwrap_or_else(|| format!("no answer after {}s", WAIT_TICKS / 10)),
        ),
    })
}

/// [`ensure_checked`] for callers that only need "a server or not" and fall
/// back to opening the store themselves (the CLI commands, until they stop
/// doing that).
pub fn ensure(cfg: &ProjectConfig) -> Option<Remote> {
    ensure_checked(cfg).ok()
}

/// What a CLI command needs from [`ensure_checked`]: a server, or `None` to
/// open the store itself — except when the database is demonstrably held by
/// another process, where opening it can only fail, so the failure is returned
/// now, naming the holder and how to release it.
pub fn ensure_cli(cfg: &ProjectConfig) -> Result<Option<Remote>> {
    match ensure_checked(cfg) {
        Ok(r) => Ok(Some(r)),
        Err(EnsureError::Busy { pid, addr }) => Err(anyhow::anyhow!(
            "the index of {} is held by PID {pid} (the server at {addr}, alive but not \
             answering; it is busy or hung). Wait for it, or release it with `devctx serve --stop`.",
            cfg.project.name
        )),
        Err(EnsureError::Failed { cause: Some(c) }) if is_lock_error(&c) => {
            Err(anyhow::anyhow!(lock_message(cfg, &c)))
        }
        // Nothing to ask and the caller will open the store itself — after
        // loading whatever model it needs, which can take longer than the lock
        // does to fail. Find out first.
        Err(EnsureError::Disabled) => {
            match devctx_store::Store::check_unlocked(&cfg.db_path()) {
                Err(e) if is_lock_error(&format!("{e:#}")) => {
                    Err(anyhow::anyhow!(lock_message(cfg, &format!("{e:#}"))))
                }
                _ => Ok(None),
            }
        }
        Err(_) => Ok(None),
    }
}

/// Whether `text` is DuckDB refusing to open a file another process holds.
pub fn is_lock_error(text: &str) -> bool {
    text.contains("Could not set lock") || text.contains("Conflicting lock")
}

/// The PID DuckDB names in "Conflicting lock is held in <exe> (PID N)".
fn lock_holder_pid(text: &str) -> Option<u32> {
    let rest = &text[text.find("(PID ")? + 5..];
    let end = rest.find(|c: char| !c.is_ascii_digit())?;
    rest[..end].parse().ok()
}

/// The explanation for a database held by someone else; `detail` is DuckDB's
/// own text (from the error or from `serve.log`).
pub fn lock_message(cfg: &ProjectConfig, detail: &str) -> String {
    const REMEDY: &str = "If it is a `devctx mcp` from an old session, close that session; \
                          otherwise `devctx serve --stop`.";
    let name = &cfg.project.name;
    match lock_holder_pid(detail) {
        Some(pid) => format!(
            "the index of {name} is held by PID {pid} ({}). {REMEDY}",
            describe_process(pid)
        ),
        None => format!("the index of {name} is held by another process ({detail}). {REMEDY}"),
    }
}

/// A short account of a process for a human: its command line, whether its
/// binary was replaced, and for how long it has been running. Linux reads
/// `/proc`; elsewhere (or if the process is gone) there is just a word.
fn describe_process(pid: u32) -> String {
    #[cfg(target_os = "linux")]
    {
        let Ok(raw) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
            return "no longer running".to_string();
        };
        let args: Vec<String> = raw
            .split(|b| *b == 0)
            .filter(|a| !a.is_empty())
            .map(|a| String::from_utf8_lossy(a).into_owned())
            .collect();
        let mut cmd = args.join(" ");
        if cmd.chars().count() > 80 {
            cmd = cmd.chars().take(77).collect::<String>() + "...";
        }
        let deleted = std::fs::read_link(format!("/proc/{pid}/exe"))
            .is_ok_and(|p| p.to_string_lossy().ends_with(" (deleted)"));
        let mut parts = vec![if cmd.is_empty() {
            "unknown command".to_string()
        } else {
            cmd
        }];
        if deleted {
            parts.push("its binary was replaced (deleted)".to_string());
        }
        if let Some(age) = std::fs::metadata(format!("/proc/{pid}"))
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
        {
            parts.push(format!("running for {}", human_age(age.as_secs())));
        }
        parts.join(", ")
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        "another process".to_string()
    }
}

#[cfg(any(target_os = "linux", test))]
fn human_age(secs: u64) -> String {
    match secs {
        0..=89 => format!("{secs}s"),
        90..=5399 => format!("{}m", secs / 60),
        5400..=172_799 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

/// What a spawned `devctx serve` is doing.
struct Spawned {
    exited: std::sync::Arc<std::sync::Mutex<Option<std::process::ExitStatus>>>,
    log: PathBuf,
    /// Length of `serve.log` before this spawn, so a stale line from an earlier
    /// failure is never reported as this one's cause.
    log_offset: u64,
}

impl Spawned {
    /// The last lines this server wrote to `serve.log`.
    fn failure_hint(&self) -> Option<String> {
        use std::io::{Read, Seek, SeekFrom};
        let mut f = std::fs::File::open(&self.log).ok()?;
        f.seek(SeekFrom::Start(self.log_offset)).ok()?;
        let mut raw = String::new();
        f.read_to_string(&mut raw).ok()?;
        let lines: Vec<&str> = raw
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with("DevCtxEngine API listening"))
            .collect();
        if lines.is_empty() {
            return None;
        }
        let from = lines.len().saturating_sub(4);
        Some(lines[from..].join(" / ").chars().take(600).collect())
    }
}

/// `serve.log` next to the database. Spawned servers append their stderr to it.
pub fn serve_log(cfg: &ProjectConfig) -> PathBuf {
    serve_file(cfg).with_file_name("serve.log")
}

/// Above this the log is dropped at spawn time rather than grown forever.
const SERVE_LOG_MAX: u64 = 256 * 1024;

/// Launch `devctx serve` detached in the background, with an idle timeout so it
/// eventually exits on its own. Its stderr goes to `serve.log`, and the child
/// is reaped by a thread so its exit is observable (and it never lingers as a
/// zombie in a long-lived parent such as the MCP).
fn spawn_server(cfg: &ProjectConfig) -> Result<Spawned> {
    let exe = devctx_core::self_exe().context("locating the devctx binary")?;
    let log = serve_log(cfg);
    if let Some(dir) = log.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    if std::fs::metadata(&log).is_ok_and(|m| m.len() > SERVE_LOG_MAX) {
        let _ = std::fs::remove_file(&log);
    }
    let log_offset = std::fs::metadata(&log).map(|m| m.len()).unwrap_or(0);
    let sink = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
        .map(std::process::Stdio::from)
        .unwrap_or_else(|_| std::process::Stdio::null());
    let mut cmd = std::process::Command::new(&exe);
    // Never inherit the committing worktree's `GIT_*` from a git hook.
    devctx_core::clean_git_env(&mut cmd);
    cmd.args(["serve", "--addr", &auto_addr(cfg), "--idle", "900"])
        .current_dir(&cfg.project.path)
        // Tells `cmd_serve` it was spawned, not typed: it must not kill a
        // server that is already up.
        .env(AUTOSPAWN_ENV, "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(sink);
    // Detach from the parent's process group so it survives the CLI exiting.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        cmd.process_group(0);
        // `self_exe()` may be `/proc/self/exe`; keep a recognisable argv[0].
        cmd.arg0("devctx");
    }
    let child = cmd
        .spawn()
        .with_context(|| format!("spawning {}", exe.display()))?;
    let exited = std::sync::Arc::new(std::sync::Mutex::new(None));
    let slot = std::sync::Arc::clone(&exited);
    std::thread::spawn(move || {
        let mut child = child;
        if let Ok(status) = child.wait() {
            if let Ok(mut g) = slot.lock() {
                *g = Some(status);
            }
        }
    });
    Ok(Spawned {
        exited,
        log,
        log_offset,
    })
}

/// Set on a server launched by [`ensure_checked`]; absent when a person ran
/// `devctx serve`.
pub const AUTOSPAWN_ENV: &str = "DEVCTX_AUTOSPAWNED";

/// The base URL of a reachable running server for this project, if any.
pub fn running_server_url(cfg: &ProjectConfig) -> Option<String> {
    discover(cfg).map(|r| r.base)
}

/// What the advertised `info.pid` is. A pid that is not [`Ownership::Ours`] is
/// never signalled: after a reboot another repository's server (or any program)
/// can hold the number. Without a recorded start time (a file from an older
/// server) the process must run from exactly the project root.
fn ownership(cfg: &ProjectConfig, info: &ServeInfo) -> Ownership {
    let Some(pid) = info.pid else {
        return Ownership::Gone;
    };
    procown::classify(pid, info.start_time, |p| {
        procown::cwd_is(p, Path::new(&cfg.project.path))
    })
}

fn owns_server(cfg: &ProjectConfig, info: &ServeInfo) -> bool {
    ownership(cfg, info) == Ownership::Ours
}

/// How long a server gets to exit on SIGTERM, and then on SIGKILL.
///
/// Above the server's own stop timeline (grace 3 s + exit checkpoint budget
/// 1.5 s, see `devctx_api::Lifecycle`), so SIGKILL never lands in the middle of
/// the checkpoint the stop is waiting for.
const TERM_WAIT: Duration = Duration::from_secs(5);
const KILL_WAIT: Duration = Duration::from_secs(3);

/// SIGTERM wait for a server that reports an indexing run in flight: it
/// cancels the run at the next file, commits and checkpoints, and may take up
/// to [`devctx_api::INDEX_CANCEL_CAP`] doing so. Killing it sooner is what left
/// unfolded WALs behind.
const INDEX_TERM_WAIT: Duration = Duration::from_secs(devctx_api::INDEX_CANCEL_CAP.as_secs() + 5);

/// Whether the advertised server says an indexing run is in flight. A quick,
/// best-effort question: no answer means "no", and the normal wait applies.
fn indexing_in_flight(info: &ServeInfo) -> bool {
    let mut req = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(1))
        .build()
        .get(&format!("http://{}/index/progress", info.addr));
    if let Some(t) = &info.token {
        req = req.set("Authorization", &format!("Bearer {t}"));
    }
    req.call()
        .ok()
        .and_then(|r| r.into_json::<IndexProgress>().ok())
        .is_some_and(|p| p.running)
}

/// The SIGTERM wait for this server: longer when it is indexing.
fn term_wait_for(info: &ServeInfo, term: Duration) -> Duration {
    if indexing_in_flight(info) {
        eprintln!(
            "· the server is indexing; waiting up to {}s for it to stop at the next file \
             and checkpoint",
            INDEX_TERM_WAIT.as_secs()
        );
        term.max(INDEX_TERM_WAIT)
    } else {
        term
    }
}

/// Stop the advertised server and report whether it is gone. Never deletes
/// the discovery file: that is only right once the process is gone.
fn terminate_ours(cfg: &ProjectConfig, info: &ServeInfo, term: Duration, kill: Duration) -> bool {
    let Some(pid) = info.pid else {
        return true;
    };
    procown::terminate(pid, || owns_server(cfg, info), term, kill)
}

/// Stop any background server holding this project's DB so the caller can take
/// exclusive ownership (used by interactive owners like the TUI). Returns
/// whether a server was stopped. Best-effort and quiet.
pub fn reclaim_db(cfg: &ProjectConfig) -> bool {
    reclaim_db_with(cfg, TERM_WAIT, KILL_WAIT)
}

fn reclaim_db_with(cfg: &ProjectConfig, term: Duration, kill: Duration) -> bool {
    let Ok(raw) = std::fs::read(serve_file(cfg)) else {
        return false;
    };
    let Ok(info) = serde_json::from_slice::<ServeInfo>(&raw) else {
        return false;
    };
    match ownership(cfg, &info) {
        Ownership::Gone => {
            // Dead, or recycled into something else: the advertisement is stale.
            remove_serve_file(cfg);
            false
        }
        // A live server we cannot vouch for keeps its own file.
        Ownership::Unverified => false,
        Ownership::Ours => {
            // The file goes only with the process: dropping it while the server
            // lives leaves it unadvertised and holding the lock.
            let term = term_wait_for(&info, term);
            if terminate_ours(cfg, &info, term, kill) {
                remove_serve_file(cfg);
                true
            } else {
                false
            }
        }
    }
}

/// Stop the server advertised for this project (SIGTERM, then SIGKILL if it
/// ignores it; drop the file once it is gone).
pub fn stop_server(cfg: &ProjectConfig) -> Result<()> {
    stop_server_with(cfg, TERM_WAIT, KILL_WAIT)
}

fn stop_server_with(cfg: &ProjectConfig, term: Duration, kill: Duration) -> Result<()> {
    let path = serve_file(cfg);
    let Ok(raw) = std::fs::read(&path) else {
        println!("No server is registered for this project.");
        return Ok(());
    };
    let info: ServeInfo = serde_json::from_slice(&raw).context("parsing serve.json")?;
    match ownership(cfg, &info) {
        Ownership::Gone => remove_serve_file(cfg),
        Ownership::Unverified => {
            let pid = info.pid.unwrap_or_default();
            eprintln!(
                "· pid {pid} is a devctx server but cannot be tied to this project (serve.json \
                 predates start times and its working directory differs); not signalling it \
                 and keeping {}",
                path.display()
            );
        }
        Ownership::Ours => {
            let pid = info.pid.unwrap_or_default();
            // Wait for it to actually exit. Returning while it still holds the
            // DuckDB file means the next command spawns a server that cannot open
            // the database, and then waits out the full auto-spawn poll — a
            // measured 61 seconds — before giving up and falling back to a local
            // open.
            let term = term_wait_for(&info, term);
            if !terminate_ours(cfg, &info, term, kill) {
                anyhow::bail!(
                    "server {pid} survived SIGTERM and SIGKILL; keeping {} so it stays discoverable",
                    path.display()
                );
            }
            println!("Stopped server (pid {pid}).");
            remove_serve_file(cfg);
        }
    }
    Ok(())
}

/// What [`probe`] found.
pub enum Discovery {
    /// A server answered.
    Up(Remote),
    /// An advertised server is alive but did not answer in time.
    Busy { pid: u32, addr: String },
    /// Nothing advertised, or what is advertised is gone.
    Down,
}

/// Health check for the first look at `serve.json`: short, so a stale file does
/// not hang the CLI.
const HEALTH_QUICK: Duration = Duration::from_millis(400);
/// Second look, only for a server whose process is demonstrably alive: a busy
/// server (indexing, embedding) can take seconds to answer `/health`, and
/// reading that as "dead" is how a second process ends up fighting it for the
/// database.
const HEALTH_PATIENT: Duration = Duration::from_secs(3);

/// What a `/health` request found.
enum Health {
    Ok,
    /// Nothing listens on the port: the server is gone, whatever its pid is now.
    Refused,
    /// No answer in time, or an error answer: something may be there, busy.
    NoAnswer,
}

fn health(base: &str, timeout: Duration) -> Health {
    let res = ureq::AgentBuilder::new()
        .timeout(timeout)
        .build()
        .get(&format!("{base}/health"))
        .call();
    match res {
        Ok(_) => Health::Ok,
        Err(e) if connection_refused(&e) => Health::Refused,
        Err(_) => Health::NoAnswer,
    }
}

fn connection_refused(e: &ureq::Error) -> bool {
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(e);
    while let Some(err) = cur {
        if err
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::ConnectionRefused)
        {
            return true;
        }
        cur = err.source();
    }
    false
}

fn healthy(base: &str, timeout: Duration) -> bool {
    matches!(health(base, timeout), Health::Ok)
}

/// Discover a running server for this project, telling "gone" apart from
/// "alive but slow".
pub fn probe(cfg: &ProjectConfig) -> Discovery {
    let Some(info) = std::fs::read(serve_file(cfg))
        .ok()
        .and_then(|raw| serde_json::from_slice::<ServeInfo>(&raw).ok())
    else {
        return Discovery::Down;
    };
    let base = format!("http://{}", info.addr);
    let up = |token: Option<String>| {
        Discovery::Up(Remote {
            base: base.clone(),
            token,
        })
    };
    match health(&base, HEALTH_QUICK) {
        Health::Ok => return up(info.token),
        // Nobody listens: the server is gone. A live pid on that number is a
        // different process (pids are reused, across repositories too).
        // — unless the process is verifiably OURS: then the listener is closed
        // but the server is not gone (shutting down, wedged), it may still hold
        // the lock, and spawning another would only hit it.
        Health::Refused => {
            return match info.pid {
                Some(pid) if owns_server(cfg, &info) => Discovery::Busy {
                    pid,
                    addr: info.addr,
                },
                _ => Discovery::Down,
            }
        }
        Health::NoAnswer => {}
    }
    match info.pid {
        Some(pid) if owns_server(cfg, &info) => {
            if healthy(&base, HEALTH_PATIENT) {
                up(info.token)
            } else {
                Discovery::Busy {
                    pid,
                    addr: info.addr,
                }
            }
        }
        _ => Discovery::Down,
    }
}

/// Discover a running server for this project and confirm it is reachable.
/// Returns `None` when there is no server (so the caller runs locally).
pub fn discover(cfg: &ProjectConfig) -> Option<Remote> {
    match probe(cfg) {
        Discovery::Up(r) => Some(r),
        _ => None,
    }
}

impl Remote {
    /// Consume into `(base_url, token)` — for handing the connection to the TUI.
    pub fn into_parts(self) -> (String, Option<String>) {
        (self.base, self.token)
    }

    fn agent(&self) -> ureq::Agent {
        // Generous overall timeout: a routed `index` of a large repo (or a slow
        // model) can run for many minutes on the server before responding.
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

    fn get(&self, path: &str) -> Result<String> {
        let req = self.auth(self.agent().get(&format!("{}{path}", self.base)));
        read(req.call())
    }

    fn post(&self, path: &str, body: Value) -> Result<String> {
        let req = self.auth(self.agent().post(&format!("{}{path}", self.base)));
        let started = std::time::Instant::now();
        read(req.send_json(body)).map_err(|e| {
            // How long it took is what proves or disproves a timeout claim: an
            // agent that allows an hour cannot time out in four seconds.
            anyhow::anyhow!("{e}\n  after {:.1}s", started.elapsed().as_secs_f64())
        })
    }

    // --- typed endpoints (return the server's JSON string) ---

    pub fn status(&self) -> Result<String> {
        self.get("/status")
    }

    pub fn index(&self, full: bool, branch: Option<&str>) -> Result<String> {
        self.post(
            "/index",
            serde_json::json!({ "full": full, "branch": branch }),
        )
    }

    /// Index an explicit path list (what the watcher sends).
    pub fn index_paths(&self, paths: &[String]) -> Result<String> {
        self.post("/index", serde_json::json!({ "paths": paths }))
    }

    /// How far the server's current indexing run has got.
    ///
    /// Builds its own short-timeout agent rather than reusing [`Self::agent`]:
    /// that one waits an hour, which is right for the index request itself and
    /// wrong for a poll behind a progress bar. A progress call that hangs
    /// should give up quickly and let the bar fall back, not freeze with no
    /// explanation.
    pub fn index_progress(&self) -> Result<IndexProgress> {
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(2))
            .build();
        let req = self.auth(agent.get(&format!("{}/index/progress", self.base)));
        let body = read(req.call())?;
        serde_json::from_str(&body).context("parsing the server's index progress")
    }

    pub fn search(
        &self,
        query: &str,
        limit: usize,
        language: Option<&str>,
        mode: &str,
        rerank: bool,
    ) -> Result<String> {
        self.post(
            "/search",
            serde_json::json!({
                "query": query, "limit": limit, "language": language,
                "mode": mode, "rerank": rerank,
            }),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn remember(
        &self,
        content: &str,
        title: &str,
        memory_type: &str,
        topic: &str,
        tags: &str,
        files: &str,
        scope: &str,
    ) -> Result<String> {
        self.post(
            "/remember",
            serde_json::json!({
                "content": content, "title": title, "type": memory_type,
                "topic": topic, "tags": tags, "files": files, "scope": scope,
            }),
        )
    }

    /// Recall this project's OWN memories.
    ///
    /// The scope is sent explicitly: the endpoint defaults to `all`, and the
    /// caller already queries the group and global tiers itself. Leaving it out
    /// asked the server for everything and then fused it with those tiers a
    /// second time.
    pub fn recall(&self, query: &str, limit: usize) -> Result<String> {
        self.post(
            "/recall",
            serde_json::json!({ "query": query, "limit": limit, "scope": "local" }),
        )
    }

    pub fn memory_stats(&self) -> Result<String> {
        self.get("/memory/stats")
    }

    pub fn impact(&self, symbol: &str, depth: usize) -> Result<String> {
        self.get(&format!("/impact/{}?depth={depth}", urlencode(symbol)))
    }

    pub fn backfill_links(&self, dry_run: bool, from_text: bool) -> Result<String> {
        self.post(
            "/memories/backfill-links",
            serde_json::json!({ "dry_run": dry_run, "from_text": from_text }),
        )
    }

    pub fn read_symbol(&self, name: &str, limit: usize) -> Result<String> {
        self.get(&format!("/symbol/{}?limit={limit}", urlencode(name)))
    }

    pub fn build_context(
        &self,
        query: &str,
        max_tokens: usize,
        include_memories: bool,
    ) -> Result<String> {
        self.post(
            "/context",
            serde_json::json!({ "query": query, "max_tokens": max_tokens,
                                "include_memories": include_memories }),
        )
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

/// Read a response body, surfacing the server's own message on a failure status.
///
/// Without this a routed command reports only `status code 500` and throws away
/// the explanation the server already wrote — which is exactly the information
/// needed to act on it.
fn read(r: std::result::Result<ureq::Response, ureq::Error>) -> Result<String> {
    match r {
        Ok(resp) => Ok(resp.into_string()?),
        Err(ureq::Error::Status(code, resp)) => {
            let body = resp.into_string().unwrap_or_default();
            let msg = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
                .unwrap_or(body);
            if msg.trim().is_empty() {
                anyhow::bail!("the server returned status {code}");
            }
            anyhow::bail!("{}", msg.trim())
        }
        Err(e) => Err(describe_transport_failure(e)),
    }
}

/// Say what a transport failure actually was, rather than repeating ureq's
/// wording for it.
///
/// ureq normalises *any* `WouldBlock` from the socket into the string
/// "timed out reading response" (`stream.rs`, with the comment that the socket
/// "most definitely" is not non-blocking). So that message does not mean a
/// timeout happened — it means a read did not complete, for a reason it threw
/// away.
///
/// That mattered once: `index` reported it four seconds into a run whose agent
/// allows an hour, against a server that then indexed for two and a half hours.
/// Four seconds is not an hour, so no deadline expired; the message was wrong
/// about its own cause and there was nothing else to go on.
///
/// This does not fix that — the cause is still unknown. It records what the next
/// occurrence needs: the error kind underneath, and how long it took to get
/// there, which is the number that showed the timeout story could not be true.
fn describe_transport_failure(e: ureq::Error) -> anyhow::Error {
    let ureq::Error::Transport(t) = &e else {
        return anyhow::anyhow!(e.to_string());
    };
    let detail = match t.message() {
        Some(m) => format!("{} ({m})", t.kind()),
        None => t.kind().to_string(),
    };
    anyhow::anyhow!("{e}\n  transport failure: {detail}")
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
    #[test]
    fn the_lock_holder_pid_is_read_from_duckdbs_message() {
        let msg = "IO Error: Could not set lock on file \"/x/index.duckdb\": Conflicting lock \
                   is held in /usr/bin/devctx (PID 4242) by user me.";
        assert!(is_lock_error(msg));
        assert_eq!(lock_holder_pid(msg), Some(4242));
        assert_eq!(lock_holder_pid("no pid here"), None);
        assert!(!is_lock_error("disk full"));
    }

    #[test]
    fn ages_are_short_and_human() {
        assert_eq!(human_age(5), "5s");
        assert_eq!(human_age(600), "10m");
        assert_eq!(human_age(7200), "2h");
        assert_eq!(human_age(3 * 86_400), "3d");
    }

    use super::*;

    fn cfg_at(dir: &Path) -> ProjectConfig {
        let mut c = ProjectConfig::default();
        c.project.path = dir.to_string_lossy().to_string();
        c.storage.db_path = dir.join("index.duckdb").to_string_lossy().to_string();
        c
    }

    fn advertise(cfg: &ProjectConfig, pid: u32) {
        let info = ServeInfo {
            addr: "127.0.0.1:1".into(),
            token: None,
            pid: Some(pid),
            start_time: None,
        };
        let path = serve_file(cfg);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, serde_json::to_vec(&info).unwrap()).unwrap();
    }

    /// The failure this exists to prevent: a server that could not start used to
    /// delete the discovery file of the healthy one that beat it to the port.
    /// The healthy server kept running, advertised nowhere, and every later
    /// command fell back to opening the database it was holding.
    #[test]
    fn a_losing_server_does_not_delete_the_winners_advertisement() {
        let dir = std::env::temp_dir().join(format!("devctx_serve_race_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = cfg_at(&dir);

        // Someone else owns the file: a pid that is not ours.
        advertise(&cfg, std::process::id() + 1);
        remove_own_serve_file(&cfg);
        assert!(
            serve_file(&cfg).exists(),
            "another process's advertisement must survive"
        );

        // Our own, on the other hand, is ours to clean up.
        advertise(&cfg, std::process::id());
        remove_own_serve_file(&cfg);
        assert!(!serve_file(&cfg).exists(), "our own must go");

        // And with no file at all it is a no-op rather than an error.
        remove_own_serve_file(&cfg);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A child running a copy of `sh` named `devctx` as `devctx serve`, so it
    /// looks like a server to [`procown::is_server_pid`] without being one. The
    /// script `serve` in `dir` is its program.
    #[cfg(target_os = "linux")]
    fn fake_server(dir: &Path, script: &str) -> std::process::Child {
        use std::process::{Command, Stdio};
        std::fs::create_dir_all(dir).unwrap();
        let exe = dir.join("devctx");
        if !exe.exists() {
            std::fs::copy("/bin/sh", &exe).unwrap();
        }
        std::fs::write(dir.join("serve"), script).unwrap();
        // A fork racing the copy can make the first exec fail with ETXTBSY.
        for _ in 0..50 {
            match Command::new(&exe)
                .arg("serve")
                .current_dir(dir)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(c) => {
                    let pid = c.id();
                    for _ in 0..100 {
                        if procown::is_server_pid(pid) {
                            return c;
                        }
                        std::thread::sleep(Duration::from_millis(30));
                    }
                    panic!("the fake server never looked like one");
                }
                Err(e) if e.raw_os_error() == Some(26) => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                Err(e) => panic!("spawning the fake server: {e}"),
            }
        }
        panic!("ETXTBSY never cleared");
    }

    #[cfg(target_os = "linux")]
    fn closed_port() -> u16 {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    }

    #[cfg(target_os = "linux")]
    fn advertise_at(cfg: &ProjectConfig, port: u16, pid: u32, start_time: Option<u64>) {
        let info = ServeInfo {
            addr: format!("127.0.0.1:{port}"),
            token: None,
            pid: Some(pid),
            start_time,
        };
        std::fs::create_dir_all(serve_file(cfg).parent().unwrap()).unwrap();
        std::fs::write(serve_file(cfg), serde_json::to_vec(&info).unwrap()).unwrap();
    }

    #[cfg(target_os = "linux")]
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("devctx_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The test process itself is alive but is not a server. A spawned `sleep`
    /// keeps this independent of the test binary's own argv (e.g. `... serve`).
    #[test]
    #[cfg(target_os = "linux")]
    fn a_live_process_that_is_not_a_server_does_not_count() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawning sleep");
        let pid = child.id();
        assert!(!procown::is_server_pid(pid));
        let dir = scratch("owns");
        let cfg = cfg_at(&dir);
        let info = ServeInfo {
            addr: "127.0.0.1:1".into(),
            token: None,
            pid: Some(pid),
            start_time: procown::start_time(pid),
        };
        assert!(!owns_server(&cfg, &info));
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// M-2 (strengthened): the real `connection_refused` must see the
    /// `io::Error` inside the `ureq::Error`, not reach `Down` by fallback.
    #[test]
    fn connection_refused_recognises_a_real_ureq_error() {
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let err = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(2))
            .build()
            .get(&format!("http://127.0.0.1:{port}/health"))
            .call()
            .expect_err("nothing listens there");
        assert!(connection_refused(&err), "not detected in: {err:?}");
        assert!(matches!(
            health(&format!("http://127.0.0.1:{port}"), Duration::from_secs(2)),
            Health::Refused
        ));

        // A listener that accepts and never answers is *not* a refusal.
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let err = ureq::AgentBuilder::new()
            .timeout(Duration::from_millis(200))
            .build()
            .get(&format!("http://{addr}/health"))
            .call()
            .expect_err("it never answers");
        assert!(!connection_refused(&err), "a timeout read as refused");
        assert!(matches!(
            health(&format!("http://{addr}"), Duration::from_millis(200)),
            Health::NoAnswer
        ));
    }

    /// M-2: a refused port whose advertised pid is a *different* server (same
    /// binary and subcommand, other start time) is Down, and is never killed.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_refused_port_with_someone_elses_server_pid_is_down_and_untouched() {
        let dir = scratch("refused_foreign");
        let cfg = cfg_at(&dir);
        let mut child = fake_server(&dir.join("srv"), "sleep 30\n");
        let real = procown::start_time(child.id()).unwrap();
        advertise_at(&cfg, closed_port(), child.id(), Some(real + 1));
        assert!(matches!(probe(&cfg), Discovery::Down));
        assert!(!reclaim_db_with(
            &cfg,
            Duration::from_millis(200),
            Duration::from_millis(200)
        ));
        assert!(
            child.try_wait().unwrap().is_none(),
            "the foreign pid was killed"
        );
        // Its start time proves the advertisement stale, so the file goes.
        assert!(!serve_file(&cfg).exists());
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The reviewer's case: our own server, listener closed but process (and
    /// lock) alive, must read as Busy — not Down, which would spawn a second
    /// server straight into the lock.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_refused_port_with_our_live_server_is_busy_not_down() {
        let dir = scratch("refused_ours");
        let cfg = cfg_at(&dir);
        let mut child = fake_server(&dir.join("srv"), "sleep 30\n");
        advertise_at(
            &cfg,
            closed_port(),
            child.id(),
            procown::start_time(child.id()),
        );
        match probe(&cfg) {
            Discovery::Busy { pid, .. } => assert_eq!(pid, child.id()),
            Discovery::Down => panic!("our live server was reported Down"),
            Discovery::Up(_) => panic!("nothing listens there"),
        }
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Items 1 and 6: a server that ignores SIGTERM is escalated to SIGKILL and
    /// the file goes only once it is gone.
    #[test]
    #[cfg(target_os = "linux")]
    fn stop_escalates_to_sigkill_and_only_then_drops_the_file() {
        let dir = scratch("stop_escalate");
        let cfg = cfg_at(&dir);
        let mut child = fake_server(
            &dir.join("srv"),
            "trap '' TERM\nwhile :; do sleep 1; done\n",
        );
        std::thread::sleep(Duration::from_millis(300));
        advertise_at(
            &cfg,
            closed_port(),
            child.id(),
            procown::start_time(child.id()),
        );
        stop_server_with(&cfg, Duration::from_millis(400), Duration::from_secs(3)).unwrap();
        let status = child.wait().unwrap();
        assert!(!status.success(), "it must have been killed: {status:?}");
        assert!(!serve_file(&cfg).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The same, for `reclaim_db`.
    #[test]
    #[cfg(target_os = "linux")]
    fn reclaim_escalates_to_sigkill_too() {
        let dir = scratch("reclaim_escalate");
        let cfg = cfg_at(&dir);
        let mut child = fake_server(
            &dir.join("srv"),
            "trap '' TERM\nwhile :; do sleep 1; done\n",
        );
        std::thread::sleep(Duration::from_millis(300));
        advertise_at(
            &cfg,
            closed_port(),
            child.id(),
            procown::start_time(child.id()),
        );
        assert!(reclaim_db_with(
            &cfg,
            Duration::from_millis(400),
            Duration::from_secs(3)
        ));
        assert!(!child.wait().unwrap().success(), "it must have been killed");
        assert!(!serve_file(&cfg).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Item 6: an old `serve.json` (no start time) whose server's cwd is not the
    /// project root is neither killed nor orphaned by `stop` — even when the
    /// server runs from a *nested* directory ("inside" is not "is").
    #[test]
    #[cfg(target_os = "linux")]
    fn an_unverifiable_server_is_neither_killed_nor_unadvertised() {
        let dir = scratch("unverified");
        let cfg = cfg_at(&dir);
        let mut child = fake_server(&dir.join("nested"), "sleep 30\n");
        advertise_at(&cfg, closed_port(), child.id(), None);
        stop_server_with(&cfg, Duration::from_millis(200), Duration::from_millis(200)).unwrap();
        assert!(
            child.try_wait().unwrap().is_none(),
            "an unverified server was killed"
        );
        assert!(serve_file(&cfg).exists(), "its advertisement was dropped");
        assert!(!reclaim_db_with(
            &cfg,
            Duration::from_millis(200),
            Duration::from_millis(200)
        ));
        assert!(child.try_wait().unwrap().is_none());
        assert!(serve_file(&cfg).exists());
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
