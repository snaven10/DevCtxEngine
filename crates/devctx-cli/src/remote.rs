//! Client side of "server mode": discover a running `devctx serve` that owns
//! the DuckDB file and route commands to it over HTTP, so no second process
//! ever takes a conflicting file lock. When no server is advertised (or it is
//! unreachable) the caller falls back to opening the store directly.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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

/// The pid `serve.json` currently advertises.
fn advertised_pid(cfg: &ProjectConfig) -> Option<u32> {
    std::fs::read(serve_file(cfg))
        .ok()
        .and_then(|raw| serde_json::from_slice::<ServeInfo>(&raw).ok())
        .and_then(|i| i.pid)
}

/// Whether "started background server" is true: the server that answered is
/// the process this call spawned, not one that won the race.
fn announce_spawn(spawned_pid: u32, advertised: Option<u32>) -> bool {
    advertised == Some(spawned_pid)
}

/// A reachable server we can route requests to.
#[derive(Clone)]
pub struct Remote {
    base: String,
    token: Option<String>,
    /// The project this server belongs to: what lets a request refused
    /// because the server was exiting be repeated against the next one.
    cfg: Option<Box<ProjectConfig>>,
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
    // No pre-check of the database: opening it here would put a transient
    // lock on the common cold path (every client, before every spawn) and could
    // take the lock a concurrent client's server is about to open. The spawn
    // goes first; only when it dies on a lock (or an exit checkpoint is under
    // way) is the holder waited out and the spawn repeated, once.
    //
    // Up to [`SPAWN_ATTEMPTS`] spawns: with three or more cold clients the loser
    // of the second spawn can die on a lock whose holder is another client (a
    // waiter's transient open, or the winner's server a moment before it writes
    // `serve.json`), so every failure on a lock gets its own wait first.
    let mut attempt = 1;
    loop {
        match spawn_and_wait(cfg) {
            Err(EnsureError::Failed { cause }) if lock_may_resolve(cfg, cause.as_deref()) => {
                let cause = cause.unwrap_or_default();
                match wait_out_lock(
                    cfg,
                    |p| Ok(devctx_store::Store::check_unlocked(p)?),
                    lock_wait_for(cfg, &cause),
                ) {
                    LockWait::Up(r) => return Ok(r),
                    // Still held after the wait: not ours to judge. The failure
                    // the spawn reported already names the holder; reporting it
                    // is fast.
                    LockWait::Held => return Err(EnsureError::Failed { cause: Some(cause) }),
                    LockWait::Free if attempt < SPAWN_ATTEMPTS => attempt += 1,
                    LockWait::Free => return Err(EnsureError::Failed { cause: Some(cause) }),
                }
            }
            other => return other,
        }
    }
}

/// Spawns [`ensure_checked`] makes before it reports a lock as a failure.
const SPAWN_ATTEMPTS: usize = 3;

/// Whether a spawn that died is worth repeating after waiting: it hit a held
/// database, or a server is taking its exit checkpoint right now.
fn lock_may_resolve(cfg: &ProjectConfig, cause: Option<&str>) -> bool {
    cause.is_some_and(is_lock_error) || devctx_api::checkpoint_marker(cfg).exists()
}

/// How long to wait out the holder named in `cause`: as long as a `devctx
/// serve` needs to come up (or to finish its exit) when that is who holds the
/// file, i.e. one a client spawned ([`AUTOSPAWN_ENV`]) or one that says it is
/// checkpointing; only [`LOCK_WAIT`] for anyone else — a server somebody ran
/// by hand and withdrew, an MCP, a TUI — so a foreign holder still fails fast.
fn lock_wait_for(cfg: &ProjectConfig, cause: &str) -> Duration {
    match lock_holder_pid(cause) {
        Some(pid) if procown::is_server_pid(pid) => holder_wait(
            spawned_by_a_client(pid),
            procown::age_secs(pid),
            checkpointing(cfg, pid),
        ),
        // The cause names no holder but an exit checkpoint is under way (the
        // marker exists): wait for it, not for half of it.
        None if devctx_api::checkpoint_marker(cfg).exists() => CHECKPOINT_WAIT,
        _ => LOCK_WAIT,
    }
}

/// The wait for a `devctx serve` that holds the file: [`HOLDER_WAIT`] when it
/// is checkpointing, or when a client spawned it ([`AUTOSPAWN_ENV`]) and it is
/// still inside its startup window ([`STARTUP_WINDOW`]); [`LOCK_WAIT`]
/// otherwise. An old autospawned serve whose `serve.json` was lost or withdrawn
/// (alive until its `--idle` timer, or hung) is not "starting": waiting for it
/// would turn a fail-fast into half a minute.
fn holder_wait(autospawned: bool, age_secs: Option<u64>, checkpointing: bool) -> Duration {
    let starting = autospawned && age_secs.is_some_and(|a| a < STARTUP_WINDOW.as_secs());
    if starting || checkpointing {
        HOLDER_WAIT
    } else {
        LOCK_WAIT
    }
}

/// Whether `pid` was launched by [`spawn_server`] (it carries [`AUTOSPAWN_ENV`]
/// in its environment). Linux only: elsewhere this is always false, nobody is
/// known to be starting, and a lock race falls back to the [`LOCK_WAIT`] of
/// 500 ms instead of [`HOLDER_WAIT`].
fn spawned_by_a_client(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        let want = format!("{AUTOSPAWN_ENV}=1");
        std::fs::read(format!("/proc/{pid}/environ"))
            .is_ok_and(|raw| raw.split(|b| *b == 0).any(|kv| kv == want.as_bytes()))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        false
    }
}

/// Spawn one server and wait for it to answer, or to die.
fn spawn_and_wait(cfg: &ProjectConfig) -> Result<Remote, EnsureError> {
    let spawned = spawn_server(cfg).map_err(|e| EnsureError::Spawn(format!("{e:#}")))?;
    // Said only once it is up: when the serve cannot start (the lock is held,
    // the port is taken) "started" would be a lie printed right before the error.
    // And only when the server that came up is OURS: when another one won the
    // race and our spawn exited, we started nothing.
    let started = |r: Remote| {
        if announce_spawn(spawned.pid, advertised_pid(cfg)) {
            eprintln!(
                "· started background server (devctx serve); it stays warm for later commands"
            );
        }
        Ok(r)
    };
    for _ in 0..WAIT_TICKS {
        std::thread::sleep(WAIT_TICK);
        match probe(cfg) {
            Discovery::Up(r) => return started(r),
            Discovery::Busy { pid, addr } => return Err(EnsureError::Busy { pid, addr }),
            Discovery::Down => {}
        }
        if spawned.exited.lock().is_ok_and(|e| e.is_some()) {
            // It may have exited because another server won the race and is up.
            if let Discovery::Up(r) = probe(cfg) {
                return started(r);
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

/// How long [`ensure_checked`] lets a held database resolve before it spawns
/// anyway. Short on purpose: a foreign holder must still fail fast (the CLI's
/// 2 s target), and the exit window is covered by the advertisement staying up.
const LOCK_WAIT: Duration = Duration::from_millis(500);

/// The same wait when the holder is a `devctx serve` (one another client just
/// spawned, still loading, or one finishing its exit checkpoint): it is going
/// to answer or to let go, and both end the wait. It is a ceiling, not a cost.
/// As long as the old 60 s wait for a spawned server (a cold model load on a
/// loaded machine can pass 30 s).
const HOLDER_WAIT: Duration = Duration::from_secs(60);

/// A server younger than this is "starting" for the purpose of [`holder_wait`].
const STARTUP_WINDOW: Duration = Duration::from_secs(60);

/// The wait when an exit checkpoint is under way but the holder's pid is not
/// known: more than the checkpoint budget (1.5 s) the server gives itself.
const CHECKPOINT_WAIT: Duration = Duration::from_secs(3);

/// What [`wait_out_lock`] found.
enum LockWait {
    /// A server came up while waiting.
    Up(Remote),
    /// The file is free (or fails for a reason that is not a lock).
    Free,
    /// Still held when the wait ran out.
    Held,
}

/// While `check` says the database is held by another process and no server is
/// advertised, keep looking for one (the holder may be one still starting):
/// [`LockWait::Up`] when one answers; [`LockWait::Free`] when the file is free
/// or the failure is not a lock; [`LockWait::Held`] when it is still held after
/// `wait`.
fn wait_out_lock(
    cfg: &ProjectConfig,
    check: impl Fn(&Path) -> Result<()>,
    wait: Duration,
) -> LockWait {
    let deadline = Instant::now() + wait;
    // Looking for a server is cheap (one small file); opening the database is
    // not (a WAL replay, and a transient lock of our own), so it is done every
    // `LOCK_CHECK_EVERY` and not on each tick.
    let mut next_check = Instant::now();
    loop {
        if Instant::now() >= next_check {
            match check(&cfg.db_path()) {
                Err(e) if is_lock_error(&format!("{e:#}")) => {}
                _ => return LockWait::Free,
            }
            next_check = Instant::now() + LOCK_CHECK_EVERY;
        }
        if let Discovery::Up(r) = probe(cfg) {
            return LockWait::Up(r);
        }
        if Instant::now() >= deadline {
            return LockWait::Held;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// How often [`wait_out_lock`] tries to open the database.
const LOCK_CHECK_EVERY: Duration = Duration::from_millis(250);

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
    cli_outcome(cfg, ensure_checked(cfg), |p| {
        Ok(devctx_store::Store::check_unlocked(p)?)
    })
}

/// [`ensure_cli`] over an already-computed answer; `check` is "can this file be
/// opened" (injected so the lock-holder cases are testable without another
/// process holding a DuckDB file).
fn cli_outcome(
    cfg: &ProjectConfig,
    ensured: Result<Remote, EnsureError>,
    check: impl Fn(&Path) -> Result<()>,
) -> Result<Option<Remote>> {
    match ensured {
        Ok(r) => Ok(Some(r)),
        Err(EnsureError::Busy { pid, addr }) => Err(anyhow::anyhow!(
            "the index of {} is held by PID {pid} (the server at {addr}, alive but not \
             answering; it is busy or hung). Wait for it, or release it with `devctx serve --stop`.",
            cfg.project.name
        )),
        Err(EnsureError::Failed { cause: Some(c) }) if is_lock_error(&c) => {
            Err(anyhow::anyhow!(lock_message(cfg, &c)))
        }
        // Nothing to ask (disabled, could not launch, or it died without a
        // lock cause in its log — or one cut short) and the caller will open
        // the store itself, after loading whatever model it needs, which can
        // take longer than the lock does to fail. Find out first.
        Err(EnsureError::Disabled | EnsureError::Spawn(_) | EnsureError::Failed { .. }) => {
            match check(&cfg.db_path()) {
                Err(e) if is_lock_error(&format!("{e:#}")) => {
                    Err(anyhow::anyhow!(lock_message(cfg, &format!("{e:#}"))))
                }
                _ => Ok(None),
            }
        }
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
                          otherwise `devctx serve --stop` (if that refuses, it says how to \
                          release it by hand).";
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
pub(crate) fn describe_process(pid: u32) -> String {
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
        if let Some(age) = procown::age_secs(pid) {
            parts.push(format!("running for {}", human_age(age)));
        }
        parts.join(", ")
    }
    #[cfg(not(target_os = "linux"))]
    {
        match procown::age_secs(pid) {
            Some(age) => format!("another process, running for {}", human_age(age)),
            None => "another process".to_string(),
        }
    }
}

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
    pid: u32,
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
    let pid = child.id();
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
        pid,
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
/// Derived from the server's own stop timeline ([`devctx_api::STOP_TIMELINE`]:
/// grace + exit checkpoint budget, 4.5 s) plus a margin wide enough for a
/// loaded machine (a 0.5 s margin measured 0.4 s of slack on WSL), so SIGKILL
/// never lands in the middle of the checkpoint the stop is waiting for. A
/// server that is still checkpointing past it says so (see
/// [`checkpointing`]) and is waited for.
const TERM_WAIT: Duration =
    Duration::from_millis(devctx_api::STOP_TIMELINE.as_millis() as u64 + 1500);
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

/// The warning for an `/index` answer that reports the run was cancelled (the
/// server was stopping), or `None` for a run that completed.
///
/// A cancelled answer is a well-formed success as far as HTTP goes, and every
/// client used to read it as one: `devctx index` exited 0, `projects reindex`
/// printed a summary, the watcher logged "re-indexed" and dropped the files it
/// had not reached.
pub fn index_cancelled(raw: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    if !v["cancelled"].as_bool().unwrap_or(false) {
        return None;
    }
    Some(format!(
        "indexing was cancelled because the server stopped, after {} file(s); what was indexed \
         is saved, and running `devctx index` again finishes the job",
        v["files_indexed"].as_u64().unwrap_or(0)
    ))
}

/// Whether server `pid` is taking its final checkpoint right now: it holds
/// [`devctx_api::checkpoint_marker`] with its pid in it. By then the server's
/// listener is closed, so this file is the only way to ask.
fn checkpointing(cfg: &ProjectConfig, pid: u32) -> bool {
    std::fs::read_to_string(devctx_api::checkpoint_marker(cfg))
        .is_ok_and(|s| s.trim() == pid.to_string())
}

/// Stop the advertised server and report whether it is gone. Never deletes
/// the discovery file: that is only right once the process is gone.
///
/// Past `term`, the wait goes on — up to [`INDEX_TERM_WAIT`] in all — for as
/// long as the server reports a checkpoint in progress: a final checkpoint of
/// a large WAL can outlast any fixed wait, and SIGKILL in the middle of it is
/// the one outcome this whole shutdown exists to avoid.
fn terminate_ours(
    cfg: &ProjectConfig,
    info: &ServeInfo,
    term: Duration,
    kill: Duration,
) -> procown::Termination {
    let Some(pid) = info.pid else {
        return procown::Termination::Gone;
    };
    procown::terminate_patient(
        pid,
        || owns_server(cfg, info),
        term,
        || checkpointing(cfg, pid),
        term.max(INDEX_TERM_WAIT),
        kill,
    )
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
            if terminate_ours(cfg, &info, term, kill).is_gone() {
                remove_serve_file(cfg);
                true
            } else {
                false
            }
        }
    }
}

/// What to tell someone whose `--stop` refused because the pid cannot be tied
/// to the file: why nothing was signalled, and the manual way out.
pub fn unverified_message(pid: u32, serve_json: &Path, what: &str) -> String {
    format!(
        "pid {pid} looks like a {what} but cannot be tied to {} (it predates start times, or \
         this platform cannot confirm the process); not signalling it and keeping the file. \
         If it is yours, stop it yourself with `kill {pid}` and delete {}.",
        serve_json.display(),
        serve_json.display()
    )
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
            // Not an `Ok`: the caller (a person, a script) asked for a stop and
            // nothing was stopped, and the lock message sends them here.
            anyhow::bail!("{}", unverified_message(pid, &path, "devctx server"));
        }
        Ownership::Ours => {
            let pid = info.pid.unwrap_or_default();
            // Wait for it to actually exit. Returning while it still holds the
            // DuckDB file means the next command spawns a server that cannot open
            // the database, and then waits out the full auto-spawn poll — a
            // measured 61 seconds — before giving up and falling back to a local
            // open.
            let term = term_wait_for(&info, term);
            let out = terminate_ours(cfg, &info, term, kill);
            if let Some(why) = out.failure("server", pid) {
                anyhow::bail!(
                    "{why}; keeping {} so it stays discoverable{}",
                    path.display(),
                    if out == procown::Termination::NoPermission {
                        format!(". Stop it as the user that owns it (`kill {pid}`)")
                    } else {
                        String::new()
                    }
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

/// How long a request refused by an exiting server waits for the next one:
/// the exit's freeze and checkpoint are budgeted at 1.5 s, plus margin.
const EXIT_RETRY_WAIT: Duration = Duration::from_secs(3);

/// Health check for the first look at `serve.json`: short, so a stale file does
/// not hang the CLI.
const HEALTH_QUICK: Duration = Duration::from_millis(400);
/// Second look, only for a server whose process is demonstrably alive: a busy
/// server (indexing, embedding) can take seconds to answer `/health`, and
/// reading that as "dead" is how a second process ends up fighting it for the
/// database.
///
/// 1.5 s: with [`HEALTH_QUICK`] a Busy verdict costs 1.9 s, inside the CLI's
/// 2 s fast-fail target (it was 3.4 s). A server that needs between 1.5 and
/// 3 s to answer `/health` now reads as Busy instead of Up; the next call
/// reaches it.
const HEALTH_PATIENT: Duration = Duration::from_millis(1500);

/// What a `/health` request found.
enum Health {
    Ok,
    /// Nothing listens on the port: the server is gone, whatever its pid is now.
    Refused,
    /// A 503 with the exiting marker: the server is freezing and checkpointing
    /// before it leaves. It still holds the lock; it is neither busy nor hung.
    Exiting,
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
        Err(ureq::Error::Status(503, r)) if r.header(procown::EXITING_HEADER).is_some() => {
            Health::Exiting
        }
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

/// One look at `serve.json` and its server.
enum Look {
    Found(Discovery),
    /// The advertised server is leaving (see [`Health::Exiting`]).
    Exiting {
        pid: u32,
        addr: String,
    },
}

/// Discover a running server for this project, telling "gone" apart from
/// "alive but slow" and from "leaving".
///
/// A server that says it is exiting is waited for, up to [`EXIT_RETRY_WAIT`]
/// (its freeze and checkpoint are budgeted at 1.5 s): the answer is then Up
/// (a replacement), Down (it left) or, only if it outlasts the wait, Busy.
/// Callers used to get Busy at once and report "busy or hung" about a server
/// that was doing its orderly exit.
pub fn probe(cfg: &ProjectConfig) -> Discovery {
    let deadline = Instant::now() + EXIT_RETRY_WAIT;
    loop {
        match look(cfg) {
            Look::Found(d) => return d,
            Look::Exiting { pid, addr } => {
                if Instant::now() >= deadline {
                    return Discovery::Busy { pid, addr };
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

fn look(cfg: &ProjectConfig) -> Look {
    let Some(info) = std::fs::read(serve_file(cfg))
        .ok()
        .and_then(|raw| serde_json::from_slice::<ServeInfo>(&raw).ok())
    else {
        return Look::Found(Discovery::Down);
    };
    let base = format!("http://{}", info.addr);
    let up = |token: Option<String>| {
        Look::Found(Discovery::Up(Remote {
            base: base.clone(),
            token,
            cfg: Some(Box::new(cfg.clone())),
        }))
    };
    let exiting = |info: ServeInfo| Look::Exiting {
        pid: info.pid.unwrap_or(0),
        addr: info.addr,
    };
    match health(&base, HEALTH_QUICK) {
        Health::Ok => return up(info.token),
        Health::Exiting => return exiting(info),
        // Nobody listens: the server is gone. A live pid on that number is a
        // different process (pids are reused, across repositories too).
        // — unless the process is verifiably OURS: then the listener is closed
        // but the server is not gone (shutting down, wedged), it may still hold
        // the lock, and spawning another would only hit it.
        Health::Refused => {
            return Look::Found(match info.pid {
                Some(pid) if owns_server(cfg, &info) => Discovery::Busy {
                    pid,
                    addr: info.addr,
                },
                _ => Discovery::Down,
            })
        }
        Health::NoAnswer => {}
    }
    match info.pid {
        Some(pid) if owns_server(cfg, &info) => match health(&base, HEALTH_PATIENT) {
            Health::Ok => up(info.token),
            Health::Exiting => exiting(info),
            _ => Look::Found(Discovery::Busy {
                pid,
                addr: info.addr,
            }),
        },
        _ => Look::Found(Discovery::Down),
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

    /// Send one request; when the server refuses it because it is exiting
    /// (a 503 from a middleware that runs before any handler, so nothing was
    /// processed), repeat it against the next server for up to
    /// [`EXIT_RETRY_WAIT`]. Any other status is final.
    fn send(
        &self,
        build: impl Fn(&Remote) -> std::result::Result<ureq::Response, Box<ureq::Error>>,
    ) -> Result<String> {
        let first = build(self);
        let exiting = |r: &std::result::Result<ureq::Response, Box<ureq::Error>>| {
            matches!(r, Err(e) if matches!(&**e, ureq::Error::Status(503, resp)
                if resp.header(procown::EXITING_HEADER).is_some()))
        };
        let Some(cfg) = self.cfg.as_ref().filter(|_| exiting(&first)) else {
            return read(first.map_err(|e| *e));
        };
        let deadline = Instant::now() + EXIT_RETRY_WAIT;
        let mut last = first;
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(200));
            // `ensure_checked` sees the old server busy until it is gone, then
            // starts the next one.
            let Ok(next) = ensure_checked(cfg.as_ref()) else {
                continue;
            };
            last = build(&next);
            if !exiting(&last) {
                break;
            }
        }
        read(last.map_err(|e| *e))
    }

    fn get(&self, path: &str) -> Result<String> {
        self.send(|r| {
            r.auth(r.agent().get(&format!("{}{path}", r.base)))
                .call()
                .map_err(Box::new)
        })
    }

    fn post(&self, path: &str, body: Value) -> Result<String> {
        let started = std::time::Instant::now();
        self.send(|r| {
            r.auth(r.agent().post(&format!("{}{path}", r.base)))
                .send_json(body.clone())
                .map_err(Box::new)
        })
        .map_err(|e| {
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
        sel: &devctx_search::KindSel,
    ) -> Result<String> {
        self.post(
            "/search",
            serde_json::json!({
                "query": query, "limit": limit, "language": language,
                "mode": mode, "rerank": rerank,
                "kind": sel.kind, "include_tests": sel.include_tests,
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
        sel: &devctx_search::KindSel,
    ) -> Result<String> {
        self.post(
            "/context",
            serde_json::json!({ "query": query, "max_tokens": max_tokens,
                                "include_memories": include_memories,
                                "kind": sel.kind, "include_tests": sel.include_tests }),
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
        // The command lists every route, as its local path does; the tool's
        // default page of 20 is for agents counting tokens.
        q.push("limit=100000".to_string());
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
        // DuckDB rewords the message (no "(PID "): still a lock error, and the
        // explanation falls back to its own text instead of inventing a pid.
        let reworded = "IO Error: Could not set lock on file \"/x/index.duckdb\": \
                        Conflicting lock is held by process 4242";
        assert!(is_lock_error(reworded));
        assert_eq!(lock_holder_pid(reworded), None);
        let cfg = cfg_at(Path::new("/x"));
        let text = lock_message(&cfg, reworded);
        assert!(text.contains("held by another process"), "{text}");
        assert!(text.contains("process 4242"), "{text}");
    }

    /// TASK-002 item 10: when the serve could not start for a reason that is
    /// not (visibly) the lock — it never launched, or its log says nothing, or
    /// the lock line was cut off — the CLI still asks the file before loading a
    /// model, so the answer is the fast lock message, not a slow open.
    #[test]
    fn every_unanswered_start_checks_the_lock_before_the_caller_loads_a_model() {
        let cfg = cfg_at(Path::new("/x"));
        let locked = |_: &Path| -> Result<()> {
            anyhow::bail!(
                "Could not set lock on file: Conflicting lock is held in /usr/bin/devctx (PID 99)"
            )
        };
        for err in [
            EnsureError::Disabled,
            EnsureError::Spawn("no such file".into()),
            EnsureError::Failed { cause: None },
            EnsureError::Failed {
                cause: Some("no answer after 60s".into()),
            },
            EnsureError::Failed {
                cause: Some("something unrelated, and the lock line was truncated".into()),
            },
        ] {
            let label = format!("{err:?}");
            let out = cli_outcome(&cfg, Err(err), locked);
            let msg = out
                .err()
                .unwrap_or_else(|| panic!("{label}: not refused"))
                .to_string();
            assert!(msg.contains("PID 99"), "{label}: {msg}");
        }
        // Not locked: the caller opens the store itself, as before.
        let free = |_: &Path| -> Result<()> { Ok(()) };
        assert!(cli_outcome(&cfg, Err(EnsureError::Spawn("x".into())), free)
            .unwrap()
            .is_none());
    }

    #[test]
    fn an_unverified_stop_says_what_to_do_by_hand() {
        let msg = unverified_message(
            77,
            Path::new("/p/.devctx/state/serve.json"),
            "devctx server",
        );
        assert!(msg.contains("`kill 77`"), "{msg}");
        assert!(msg.contains("/p/.devctx/state/serve.json"), "{msg}");
    }

    /// Field: a three-day process read "running for 2s" (the mtime of
    /// `/proc/<pid>` is when the inode was first looked up, not when the
    /// process started).
    #[test]
    #[cfg(target_os = "linux")]
    fn the_age_shown_is_the_process_age_not_the_procfs_lookup_time() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        std::thread::sleep(Duration::from_millis(2300));
        let text = describe_process(child.id());
        let _ = child.kill();
        let _ = child.wait();
        // 2.3 s of sleep plus whatever a loaded CI machine adds: the point is
        // seconds, not the days the procfs lookup time would have claimed.
        assert!(
            (2..=6).any(|n| text.contains(&format!("running for {n}s"))),
            "{text}"
        );
    }

    /// N1: the "started" line is for OUR spawn only.
    #[test]
    fn only_our_own_spawn_is_announced() {
        assert!(announce_spawn(42, Some(42)));
        assert!(!announce_spawn(42, Some(43)), "another server won the race");
        assert!(!announce_spawn(42, None));
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

    fn serve_fixed(reply: &'static str) -> SocketAddr {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || {
            for c in l.incoming() {
                let Ok(mut c) = c else { break };
                let mut buf = [0u8; 4096];
                let _ = c.read(&mut buf);
                let _ = c.write_all(reply.as_bytes());
            }
        });
        addr
    }

    const EXITING_503: &str = "HTTP/1.1 503 Service Unavailable\r\nx-devctx-exiting: 1\r\n\
                               Content-Length: 0\r\nConnection: close\r\n\r\n";
    const PLAIN_503: &str = "HTTP/1.1 503 Service Unavailable\r\n\
                             Content-Length: 0\r\nConnection: close\r\n\r\n";
    const OK_200: &str = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";

    /// TASK-017 fixup I1 (CLI): a request refused by a server that is exiting
    /// is repeated against the one `serve.json` now advertises; a 503 that does
    /// not carry the marker is a final answer.
    #[test]
    fn a_command_refused_by_an_exiting_server_reaches_the_next_one() {
        let dir = std::env::temp_dir().join(format!("devctx-i1-cli-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = cfg_at(&dir);
        let dying = serve_fixed(EXITING_503);
        let next = serve_fixed(OK_200);
        write_serve_file(&cfg, next, None).unwrap();
        let remote = Remote {
            base: format!("http://{dying}"),
            token: None,
            cfg: Some(Box::new(cfg)),
        };
        assert_eq!(remote.status().unwrap(), "ok");

        let plain = Remote {
            base: format!("http://{}", serve_fixed(PLAIN_503)),
            token: None,
            cfg: remote.cfg.clone(),
        };
        let started = Instant::now();
        assert!(plain.status().is_err());
        assert!(started.elapsed() < Duration::from_secs(2), "not retried");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_cancelled_index_answer_is_recognised() {
        let warn = index_cancelled(r#"{"cancelled":true,"files_indexed":3,"commit":"abc"}"#)
            .expect("a cancelled run must be reported as such");
        assert!(warn.contains("3 file(s)"), "{warn}");
        assert!(index_cancelled(r#"{"files_indexed":3,"commit":"abc"}"#).is_none());
        assert!(index_cancelled(r#"{"cancelled":false}"#).is_none());
        assert!(index_cancelled("not json").is_none());
    }

    /// D1b: `serve --stop` keeps waiting only for the server it signalled,
    /// and only while that server says it is checkpointing — a marker left by
    /// another (dead, or recycled) pid does not hold SIGKILL off.
    #[test]
    fn the_checkpoint_marker_speaks_only_for_its_own_pid() {
        let dir = std::env::temp_dir().join(format!("devctx_ckpt_marker_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = cfg_at(&dir);
        assert!(!checkpointing(&cfg, 4242), "no marker, no patience");
        std::fs::write(devctx_api::checkpoint_marker(&cfg), "4242").unwrap();
        assert!(checkpointing(&cfg, 4242));
        assert!(!checkpointing(&cfg, 4243), "someone else's marker");
        assert!(
            TERM_WAIT >= devctx_api::STOP_TIMELINE + Duration::from_secs(1),
            "the plain wait must clear the server's own timeline with room to spare"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A listener that answers `first` until `flip_at`, `then` after it.
    fn serve_switching(first: &'static str, then: &'static str, flip_at: Instant) -> SocketAddr {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || {
            for c in l.incoming() {
                let Ok(mut c) = c else { break };
                let mut buf = [0u8; 4096];
                let _ = c.read(&mut buf);
                let reply = if Instant::now() < flip_at {
                    first
                } else {
                    then
                };
                let _ = c.write_all(reply.as_bytes());
            }
        });
        addr
    }

    /// TASK-017 fixup K2: a server that answers `/health` with the marked 503
    /// is exiting, not "busy or hung": `probe` waits through the exit window
    /// and reports what is there afterwards (here, its replacement).
    #[test]
    fn probe_waits_through_the_exit_window() {
        let dir = std::env::temp_dir().join(format!("devctx_k2_probe_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = cfg_at(&dir);
        let started = Instant::now();
        let addr = serve_switching(EXITING_503, OK_200, started + Duration::from_millis(700));
        write_serve_file(&cfg, addr, None).unwrap();
        let found = probe(&cfg);
        assert!(
            matches!(found, Discovery::Up(_)),
            "a server in its exit window must be waited for, not reported busy"
        );
        assert!(
            started.elapsed() >= Duration::from_millis(600),
            "answered before the window closed"
        );

        // It leaves instead: the advertisement is withdrawn while we wait.
        let addr = serve_switching(EXITING_503, EXITING_503, started + Duration::from_secs(60));
        write_serve_file(&cfg, addr, None).unwrap();
        let remover = {
            let cfg = cfg.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(500));
                remove_serve_file(&cfg);
            })
        };
        let found = probe(&cfg);
        remover.join().unwrap();
        assert!(matches!(found, Discovery::Down), "it left: nothing is up");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// TASK-017 fixup J2: a held database with no server advertised gets a
    /// moment to resolve before a spawn; a server that appears meanwhile is
    /// used; a free file (or an error that is not a lock) is not waited on.
    #[test]
    fn a_held_database_gets_a_moment_before_a_spawn() {
        let dir = std::env::temp_dir().join(format!("devctx_j2_lock_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = cfg_at(&dir);
        let locked = |_: &Path| -> Result<()> {
            Err(anyhow::anyhow!(
                "IO Error: Could not set lock on file: Conflicting lock is held in devctx (PID 7)"
            ))
        };
        let started = Instant::now();
        assert!(matches!(
            wait_out_lock(&cfg, locked, Duration::from_millis(300)),
            LockWait::Held
        ));
        assert!(
            started.elapsed() >= Duration::from_millis(300),
            "did not wait"
        );

        let started = Instant::now();
        assert!(matches!(
            wait_out_lock(&cfg, |_| Ok(()), Duration::from_secs(5)),
            LockWait::Free
        ));
        assert!(matches!(
            wait_out_lock(
                &cfg,
                |_| Err(anyhow::anyhow!("disk full")),
                Duration::from_secs(5)
            ),
            LockWait::Free
        ));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "waited on a free file"
        );

        // The holder was a server still starting: it shows up while we wait.
        let next = serve_fixed(OK_200);
        let writer = {
            let cfg = cfg.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(250));
                write_serve_file(&cfg, next, None).unwrap();
            })
        };
        let up = wait_out_lock(&cfg, locked, Duration::from_secs(5));
        writer.join().unwrap();
        assert!(
            matches!(up, LockWait::Up(_)),
            "the server that appeared is used"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// TASK-017 fixup L (K1/K3): only a server that is STARTING is waited for
    /// the full ceiling; an old autospawned one that lost its advertisement
    /// (alive till its idle timer, or hung) fails fast like any foreign holder.
    #[test]
    fn only_a_starting_or_checkpointing_holder_gets_the_long_wait() {
        assert_eq!(holder_wait(true, Some(3), false), HOLDER_WAIT);
        assert_eq!(holder_wait(true, Some(59), false), HOLDER_WAIT);
        assert_eq!(
            holder_wait(true, Some(61), false),
            LOCK_WAIT,
            "old, not starting"
        );
        assert_eq!(holder_wait(true, Some(900), false), LOCK_WAIT);
        assert_eq!(holder_wait(true, None, false), LOCK_WAIT, "age unknown");
        assert_eq!(holder_wait(false, Some(3), false), LOCK_WAIT, "run by hand");
        assert_eq!(
            holder_wait(false, Some(900), true),
            HOLDER_WAIT,
            "checkpointing"
        );
        assert!(
            HOLDER_WAIT >= Duration::from_secs(60),
            "a model load over 30 s must not make the loser fail"
        );
    }

    /// Fixup L: a checkpoint marker whose pid the lock message does not give
    /// still earns a wait longer than the checkpoint budget (1.5 s).
    #[test]
    fn a_checkpoint_without_a_named_holder_is_waited_out() {
        let dir = std::env::temp_dir().join(format!("devctx_l_ckpt_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = cfg_at(&dir);
        assert_eq!(lock_wait_for(&cfg, "Could not set lock"), LOCK_WAIT);
        std::fs::write(devctx_api::checkpoint_marker(&cfg), "4242").unwrap();
        let wait = lock_wait_for(&cfg, "Could not set lock");
        assert!(
            wait >= Duration::from_millis(1500) + Duration::from_millis(500),
            "{wait:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Fixup L: `spawned_by_a_client` reads the autospawn mark out of the
    /// process environment (the e2e foreign-lock test never reaches it: its
    /// holder fails on its name first).
    #[cfg(target_os = "linux")]
    #[test]
    fn the_autospawn_mark_is_read_from_the_environment() {
        let mark = |on: bool| {
            let mut c = std::process::Command::new("sleep");
            c.arg("5");
            if on {
                c.env(AUTOSPAWN_ENV, "1");
            } else {
                c.env_remove(AUTOSPAWN_ENV);
            }
            c.spawn().unwrap()
        };
        let (mut yes, mut no) = (mark(true), mark(false));
        let (a, b) = (spawned_by_a_client(yes.id()), spawned_by_a_client(no.id()));
        let _ = (yes.kill(), no.kill(), yes.wait(), no.wait());
        assert!(a, "the marked process is not recognised");
        assert!(!b, "an unmarked process is taken for a spawned one");
        assert!(!spawned_by_a_client(u32::MAX - 1), "a pid that is gone");
    }

    /// Fixup L (K4): the wait looks for a server often but opens the database
    /// only every `LOCK_CHECK_EVERY` (it was ~600 opens in 30 s).
    #[test]
    fn the_wait_does_not_open_the_database_on_every_tick() {
        let dir = std::env::temp_dir().join(format!("devctx_l_k4_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = cfg_at(&dir);
        let opens = std::sync::atomic::AtomicUsize::new(0);
        let locked = |_: &Path| -> Result<()> {
            opens.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(anyhow::anyhow!("Could not set lock on file"))
        };
        assert!(matches!(
            wait_out_lock(&cfg, locked, Duration::from_millis(1100)),
            LockWait::Held
        ));
        let n = opens.load(std::sync::atomic::Ordering::SeqCst);
        assert!((2..=6).contains(&n), "{n} opens in 1.1 s");
        let _ = std::fs::remove_dir_all(&dir);
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
        // Refusing is a failure (non-zero exit), with the way out spelled out.
        let err = stop_server_with(&cfg, Duration::from_millis(200), Duration::from_millis(200))
            .expect_err("a stop that stopped nothing must not succeed");
        let msg = format!("{err:#}");
        assert!(msg.contains(&format!("`kill {}`", child.id())), "{msg}");
        assert!(msg.contains("serve.json"), "{msg}");
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
