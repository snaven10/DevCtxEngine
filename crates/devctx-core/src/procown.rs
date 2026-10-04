//! Process ownership: deciding whether a pid recorded in a `serve.json` is
//! still *our* server, and stopping it without ever touching anything else.
//!
//! Pids are recycled, across reboots and across repositories, so "the pid
//! exists" proves nothing. A server is identified by its binary and subcommand
//! (`/proc/<pid>/exe` + `cmdline`) and by its kernel start time, recorded in
//! `serve.json` when it started. Only a process that passes is ever signalled.
//!
//! Shared by the project server (`devctx-cli`) and the central daemon
//! (`devctx-central`), which used to differ: one verified, the other killed the
//! recorded pid blindly.

use std::path::Path;
use std::time::{Duration, Instant};

/// What a recorded `pid` turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ownership {
    /// Verifiably the server that wrote the file: safe to signal.
    Ours,
    /// Dead, recycled into another program, or into a *different* server (start
    /// time mismatch): the advertisement is stale and may be dropped.
    Gone,
    /// A live devctx server that cannot be tied to this advertisement (an old
    /// file without a start time, whose cwd does not match). Not signalled, and
    /// its file is not dropped either: deleting it would orphan a live server.
    Unverified,
}

/// Whether `exe` (the target of `/proc/<pid>/exe`) and the NUL-separated
/// `/proc/<pid>/cmdline` describe `devctx … serve|api`.
///
/// `exe` is the reliable name: a server started through `/proc/self/exe` has
/// that very string as argv[0], and after a reinstall the link reads
/// `<path> (deleted)`, which is stripped. Without it argv[0] stands in. The
/// subcommand is the first argument that is not a flag, so `devctx search api`
/// is not a server.
pub fn is_server_proc(exe: Option<&str>, cmdline: &[u8]) -> bool {
    let mut args = cmdline.split(|b| *b == 0).filter(|a| !a.is_empty());
    let Some(argv0) = args.next() else {
        return false;
    };
    let argv0 = String::from_utf8_lossy(argv0);
    let path = exe.unwrap_or(&argv0);
    let path = path.strip_suffix(" (deleted)").unwrap_or(path);
    let name = path.rsplit('/').next().unwrap_or("");
    name.starts_with("devctx")
        && args
            .find(|a| !a.starts_with(b"-"))
            .is_some_and(|a| a == b"serve" || a == b"api")
}

/// Field 22 of a `/proc/<pid>/stat` line. The command name (field 2) may hold
/// spaces and parentheses, so count from the last `)`.
pub fn parse_start_time(stat: &str) -> Option<u64> {
    let rest = &stat[stat.rfind(')')? + 1..];
    // `rest` starts at field 3; field 22 is the 20th token after it.
    rest.split_whitespace().nth(19)?.parse().ok()
}

/// Kernel start time of `pid` in clock ticks since boot (Linux), or a stable
/// token of `ps -o lstart=` (macOS and other Unix: only ever compared for
/// equality). `None` when the process is gone or cannot be asked.
pub fn start_time(pid: u32) -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        parse_start_time(&stat)
    }
    #[cfg(not(target_os = "linux"))]
    {
        match ps_info(pid) {
            PsInfo::Proc { lstart, .. } => Some(lstart_token(&lstart)),
            _ => None,
        }
    }
}

/// What `ps -p <pid> -o lstart=,command=` said.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub(crate) enum PsInfo {
    /// `ps` ran and there is no such process.
    Absent,
    /// `ps` could not be run or its output was not understood (and on Windows
    /// there is no `ps`): nothing can be claimed about the pid.
    Unknown,
    Proc {
        lstart: String,
        command: String,
    },
}

/// Parse one line of `ps -o lstart=,command=`: `lstart` is the fixed
/// five-token `Mon Jan  2 15:04:05 2006`, followed by the command line.
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub(crate) fn parse_ps_line(line: &str) -> PsInfo {
    let mut it = line.split_whitespace();
    let toks: Vec<&str> = it.by_ref().take(5).collect();
    if toks.len() < 5 {
        return PsInfo::Unknown;
    }
    let command = it.collect::<Vec<_>>().join(" ");
    if command.is_empty() {
        return PsInfo::Unknown;
    }
    PsInfo::Proc {
        lstart: toks.join(" "),
        command,
    }
}

/// A stable 64-bit token (FNV-1a) of an `lstart` string, so it fits the
/// numeric `start_time` slot of `serve.json`. Compared, never ordered.
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub(crate) fn lstart_token(lstart: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in lstart.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

#[cfg_attr(target_os = "linux", allow(dead_code))]
pub(crate) fn ps_info(pid: u32) -> PsInfo {
    #[cfg(unix)]
    {
        let out = std::process::Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "lstart=,command="])
            .env("LC_ALL", "C")
            .stderr(std::process::Stdio::null())
            .output();
        match out {
            Err(_) => PsInfo::Unknown,
            Ok(o) => {
                let text = String::from_utf8_lossy(&o.stdout);
                match text.lines().find(|l| !l.trim().is_empty()) {
                    Some(l) => parse_ps_line(l),
                    // BSD/macOS `ps -p` exits 1 with no output for an absent pid.
                    None if o.status.code() == Some(1) => PsInfo::Absent,
                    None => PsInfo::Unknown,
                }
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        PsInfo::Unknown
    }
}

/// Whether `pid` is a live `devctx serve|api` process (see [`is_server_proc`]).
/// Says it is *a* devctx server, not that it is the one a file advertises.
/// Off Linux this is `ps`'s word for it, never a bare `kill -0`.
pub fn is_server_pid(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        let Ok(cmdline) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
            return false;
        };
        let exe = std::fs::read_link(format!("/proc/{pid}/exe"))
            .ok()
            .map(|p| p.to_string_lossy().into_owned());
        is_server_proc(exe.as_deref(), &cmdline)
    }
    #[cfg(not(target_os = "linux"))]
    {
        matches!(ps_info(pid), PsInfo::Proc { command, .. } if ps_command_is_server(&command))
    }
}

/// [`is_server_proc`] over a `ps` command column (space-separated).
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub(crate) fn ps_command_is_server(command: &str) -> bool {
    is_server_proc(None, command.replace(' ', "\0").as_bytes())
}

/// Seconds since `pid` started. Linux: its start ticks against `/proc/uptime`
/// and `sysconf(_SC_CLK_TCK)` (the mtime of `/proc/<pid>` is not it: that inode
/// is created on first lookup, so a three-day process reads "2s"). Elsewhere:
/// `ps -o etime=`.
pub fn age_secs(pid: u32) -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let start = start_time(pid)?;
        let uptime: f64 = std::fs::read_to_string("/proc/uptime")
            .ok()?
            .split_whitespace()
            .next()?
            .parse()
            .ok()?;
        // SAFETY: plain libc query.
        let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        age_from_ticks(start, uptime, if hz > 0 { hz as u64 } else { 100 })
    }
    #[cfg(not(target_os = "linux"))]
    {
        #[cfg(unix)]
        {
            let out = std::process::Command::new("ps")
                .args(["-p", &pid.to_string(), "-o", "etime="])
                .stderr(std::process::Stdio::null())
                .output()
                .ok()?;
            parse_etime(String::from_utf8_lossy(&out.stdout).trim())
        }
        #[cfg(not(unix))]
        {
            let _ = pid;
            None
        }
    }
}

/// Age in seconds of a process that started `start_ticks` after boot, `hz`
/// ticks per second, when the machine has been up `uptime_secs`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn age_from_ticks(start_ticks: u64, uptime_secs: f64, hz: u64) -> Option<u64> {
    let started = start_ticks as f64 / hz.max(1) as f64;
    (uptime_secs >= started).then_some((uptime_secs - started) as u64)
}

/// `ps -o etime=`: `[[dd-]hh:]mm:ss`.
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub(crate) fn parse_etime(s: &str) -> Option<u64> {
    let (days, rest) = match s.split_once('-') {
        Some((d, r)) => (d.parse::<u64>().ok()?, r),
        None => (0, s),
    };
    let mut secs = 0u64;
    let parts: Vec<&str> = rest.split(':').collect();
    if parts.is_empty() || parts.len() > 3 {
        return None;
    }
    for p in parts {
        secs = secs * 60 + p.trim().parse::<u64>().ok()?;
    }
    Some(days * 86_400 + secs)
}

/// Whether the working directory of `pid` is exactly `root` (both resolved).
/// "Inside" is not enough: a server of a repository nested under `root` would
/// pass. A cwd the kernel reports as deleted never matches.
pub fn cwd_is(pid: u32, root: &Path) -> bool {
    #[cfg(target_os = "linux")]
    {
        let Ok(cwd) = std::fs::read_link(format!("/proc/{pid}/cwd")) else {
            return false;
        };
        if cwd.to_string_lossy().ends_with(" (deleted)") {
            return false;
        }
        let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        let cwd = std::fs::canonicalize(&cwd).unwrap_or(cwd);
        cwd == root
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (pid, root);
        false
    }
}

/// Whether `pid`'s command line contains `flag` as an argument.
pub fn cmdline_has(pid: u32, flag: &str) -> bool {
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .map(|c| c.split(|b| *b == 0).any(|a| a == flag.as_bytes()))
        .unwrap_or(false)
}

/// Classify the pid a discovery file advertises.
///
/// With a recorded `start_time` the answer is exact. Without one (a file from
/// an older server) `fallback` must vouch for the process — a cwd check for a
/// project server, `--central` for the daemon.
pub fn classify(
    pid: u32,
    recorded_start: Option<u64>,
    fallback: impl Fn(u32) -> bool,
) -> Ownership {
    #[cfg(target_os = "linux")]
    {
        if !is_server_pid(pid) {
            return Ownership::Gone;
        }
        match recorded_start {
            Some(t) if start_time(pid) == Some(t) => Ownership::Ours,
            Some(_) => Ownership::Gone,
            None if fallback(pid) => Ownership::Ours,
            None => Ownership::Unverified,
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        classify_ps(ps_info(pid), pid, recorded_start, fallback)
    }
}

/// The off-Linux classification, over what `ps` said. Anything `ps` does not
/// positively confirm is [`Ownership::Unverified`] (never signalled, file kept):
/// after a reboot the pid can hold any program, and "it is alive" proves
/// nothing. Pure, so it is tested on every platform.
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub(crate) fn classify_ps(
    info: PsInfo,
    pid: u32,
    recorded_start: Option<u64>,
    fallback: impl Fn(u32) -> bool,
) -> Ownership {
    match info {
        PsInfo::Absent => Ownership::Gone,
        PsInfo::Unknown => Ownership::Unverified,
        PsInfo::Proc { lstart, command } => {
            if !ps_command_is_server(&command) {
                return Ownership::Gone;
            }
            match recorded_start {
                Some(t) if t == lstart_token(&lstart) => Ownership::Ours,
                Some(_) => Ownership::Gone,
                None if fallback(pid) => Ownership::Ours,
                None => Ownership::Unverified,
            }
        }
    }
}

/// A handle on a process that survives pid reuse: a pidfd on Linux, so the
/// signal goes to the process that was verified and not to whatever inherits
/// the number between the check and the `kill`. Elsewhere it is the plain pid
/// (the window is documented, not closed).
struct Handle {
    pid: u32,
    #[cfg(target_os = "linux")]
    fd: Option<i32>,
    /// Kernel start time at open, to tell "exited" from "pid reused" when no
    /// pidfd is available.
    #[cfg(target_os = "linux")]
    start: Option<u64>,
}

impl Handle {
    fn open(pid: u32) -> Self {
        #[cfg(target_os = "linux")]
        {
            // SAFETY: plain syscall with integer arguments.
            let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::c_long, 0) };
            Handle {
                pid,
                fd: (fd >= 0).then_some(fd as i32),
                start: start_time(pid),
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            Handle { pid }
        }
    }

    /// Send `sig`. `Err(errno)` when the kernel refused it; a process that is
    /// already gone (`ESRCH`) is not a failure.
    fn signal(&self, sig: i32) -> Result<(), i32> {
        #[cfg(target_os = "linux")]
        if let Some(fd) = self.fd {
            // SAFETY: `fd` is a pidfd we own; a null siginfo is allowed.
            let rc = unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    fd as libc::c_long,
                    sig as libc::c_long,
                    std::ptr::null::<libc::c_void>(),
                    0 as libc::c_long,
                )
            };
            return errno_result(rc);
        }
        #[cfg(unix)]
        {
            // SAFETY: plain syscall with integer arguments.
            let rc = unsafe { libc::kill(self.pid as libc::pid_t, sig) };
            errno_result(rc as libc::c_long)
        }
        #[cfg(not(unix))]
        {
            let _ = sig;
            Ok(())
        }
    }

    /// Wait until the process has *exited*: its descriptors are closed, so any
    /// lock it held is released. Not "stopped looking like a server": while the
    /// kernel tears down a large address space `/proc/<pid>/cmdline` already
    /// reads empty, and the DuckDB lock is still held.
    fn wait_exit(&self, limit: Duration, owns: &dyn Fn() -> bool) -> bool {
        #[cfg(target_os = "linux")]
        if let Some(fd) = self.fd {
            // A pidfd is readable once the process has exited.
            let deadline = Instant::now() + limit;
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                let mut p = libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                };
                let ms = left.as_millis().min(i32::MAX as u128) as libc::c_int;
                // SAFETY: one valid pollfd.
                let n = unsafe { libc::poll(&mut p, 1, ms) };
                if n > 0 {
                    return true;
                }
                if n < 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                    break;
                }
                if left.is_zero() {
                    return false;
                }
            }
        }
        #[cfg(target_os = "linux")]
        if let Some(t0) = self.start {
            return wait_until(|| exited(self.pid, t0), limit);
        }
        wait_until(|| !owns(), limit)
    }
}

/// `Ok` for a successful syscall (or a vanished target), the errno otherwise.
fn errno_result(rc: libc::c_long) -> Result<(), i32> {
    if rc == 0 {
        return Ok(());
    }
    match std::io::Error::last_os_error().raw_os_error() {
        Some(libc::ESRCH) => Ok(()),
        Some(e) => Err(e),
        None => Err(libc::EIO),
    }
}

/// Linux: whether the process that started at `t0` has exited (gone, zombie,
/// or the pid reused by a newer process).
#[cfg(target_os = "linux")]
fn exited(pid: u32, t0: u64) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return true;
    };
    if parse_start_time(&stat) != Some(t0) {
        return true;
    }
    matches!(
        stat.rfind(')')
            .and_then(|i| stat[i + 1..].split_whitespace().next()),
        Some("Z") | Some("X")
    )
}

#[cfg(target_os = "linux")]
impl Drop for Handle {
    fn drop(&mut self) {
        if let Some(fd) = self.fd {
            // SAFETY: closing a descriptor we opened.
            unsafe { libc::close(fd) };
        }
    }
}

fn wait_until(mut done: impl FnMut() -> bool, limit: Duration) -> bool {
    let start = Instant::now();
    loop {
        if done() {
            return true;
        }
        if start.elapsed() >= limit {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// How [`terminate`] ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Termination {
    /// The process has exited (or was never ours to stop).
    Gone,
    /// Still alive after SIGTERM and SIGKILL.
    Survived,
    /// The kernel refused the signal (`EPERM`): another user's or a `sudo`
    /// process. Waiting would only repeat the refusal.
    NoPermission,
}

impl Termination {
    pub fn is_gone(self) -> bool {
        self == Termination::Gone
    }

    /// The sentence for a stop that did not stop it; `None` when it did.
    pub fn failure(self, what: &str, pid: u32) -> Option<String> {
        match self {
            Termination::Gone => None,
            Termination::Survived => Some(format!("{what} {pid} survived SIGTERM and SIGKILL")),
            Termination::NoPermission => Some(format!(
                "no permission to signal PID {pid} (it belongs to another user, or runs as root)"
            )),
        }
    }
}

/// Stop `pid`: SIGTERM, wait `term_wait`, then SIGKILL and wait `kill_wait`.
///
/// `owns` answers "is the process on this pid still the one we mean": it is
/// checked before any signal. It returns [`Termination::Gone`] only once the
/// process has *exited* (descriptors closed, locks released) — not merely
/// once it stopped looking like ours — so a command run right after never
/// meets the lock of the process it just stopped. A process that ignores
/// SIGTERM is escalated rather than abandoned, and a refused signal is
/// reported as such. Callers delete the discovery file only on `Gone`;
/// deleting it while the process lives leaves an unadvertised server holding
/// the database lock.
pub fn terminate(
    pid: u32,
    owns: impl Fn() -> bool,
    term_wait: Duration,
    kill_wait: Duration,
) -> Termination {
    terminate_patient(pid, owns, term_wait, || false, term_wait, kill_wait)
}

/// [`terminate`], with patience: past `term_wait`, keep waiting on SIGTERM —
/// up to `patience_cap` in all — for as long as `patient()` says the process
/// is doing something that must not be cut short (a server folding its WAL
/// says so through its checkpoint marker). SIGKILL comes only after that.
pub fn terminate_patient(
    pid: u32,
    owns: impl Fn() -> bool,
    term_wait: Duration,
    patient: impl Fn() -> bool,
    patience_cap: Duration,
    kill_wait: Duration,
) -> Termination {
    // Open the handle before verifying, so what is verified is what is signalled.
    let handle = Handle::open(pid);
    terminate_with(
        &handle,
        owns,
        term_wait,
        (&patient, patience_cap),
        kill_wait,
        |h, limit, owns| h.wait_exit(limit, owns),
    )
}

fn terminate_with(
    handle: &Handle,
    owns: impl Fn() -> bool,
    term_wait: Duration,
    (patient, patience_cap): (&dyn Fn() -> bool, Duration),
    kill_wait: Duration,
    wait: impl Fn(&Handle, Duration, &dyn Fn() -> bool) -> bool,
) -> Termination {
    if !owns() {
        return Termination::Gone;
    }
    #[cfg(unix)]
    {
        if handle.signal(libc::SIGTERM) == Err(libc::EPERM) {
            return Termination::NoPermission;
        }
        if wait(handle, term_wait, &owns) {
            return Termination::Gone;
        }
        let mut waited = term_wait;
        while waited < patience_cap && patient() {
            let slice = Duration::from_millis(250).min(patience_cap - waited);
            if wait(handle, slice, &owns) {
                return Termination::Gone;
            }
            waited += slice;
        }
        if handle.signal(libc::SIGKILL) == Err(libc::EPERM) {
            return Termination::NoPermission;
        }
    }
    #[cfg(not(unix))]
    let _ = (handle, term_wait, patient, patience_cap);
    if wait(handle, kill_wait, &owns) {
        Termination::Gone
    } else {
        Termination::Survived
    }
}

#[cfg(test)]
mod pure_tests {
    use super::*;

    /// A recycled pid must not count as a live server.
    #[test]
    fn only_a_devctx_serve_cmdline_counts_as_a_server() {
        let is = is_server_proc;
        assert!(is(
            None,
            b"/home/u/.local/bin/devctx\0serve\0--addr\x00127.0.0.1:20111\x00"
        ));
        assert!(is(None, b"devctx\0api\0"));
        assert!(!is(None, b"/usr/bin/vim\0serve\0"));
        assert!(!is(None, b"/usr/bin/devctx\0mcp\0"));
        assert!(!is(None, b""));
        // The subcommand is the first non-flag argument, not any argument.
        assert!(!is(None, b"devctx\0search\0api\0"));
        assert!(!is(None, b"devctx\0index\0serve\0"));
        assert!(is(None, b"devctx\0--verbose\0serve\0"));
    }

    #[test]
    fn process_age_comes_from_start_ticks_not_from_procfs_mtime() {
        // Up 3 days, started at tick 100 of 100 Hz: 3 days minus one second.
        assert_eq!(age_from_ticks(100, 259_200.0, 100), Some(259_199));
        assert_eq!(age_from_ticks(1000, 5.0, 100), None);
        assert_eq!(parse_etime("00:42"), Some(42));
        assert_eq!(parse_etime("03:00:01"), Some(10_801));
        assert_eq!(parse_etime("3-00:00:00"), Some(259_200));
        assert_eq!(parse_etime("x"), None);
    }

    /// Off Linux a live pid is only ours if `ps` says it is a `devctx serve`
    /// started when the file says: a reboot can hand the number to anything.
    #[test]
    fn off_linux_only_ps_confirmation_makes_a_pid_ours() {
        let line =
            "Thu Oct  3 09:15:02 2026 /Users/u/.local/bin/devctx serve --addr 127.0.0.1:20111";
        let info = parse_ps_line(line);
        let PsInfo::Proc { lstart, command } = info.clone() else {
            panic!("{info:?}")
        };
        assert_eq!(lstart, "Thu Oct 3 09:15:02 2026");
        assert!(ps_command_is_server(&command));
        let t = lstart_token(&lstart);
        let no = |_: u32| false;
        // Matching start: ours. Different start: a different process.
        assert_eq!(classify_ps(info.clone(), 7, Some(t), no), Ownership::Ours);
        assert_eq!(
            classify_ps(info.clone(), 7, Some(t + 1), no),
            Ownership::Gone
        );
        // No recorded start and nothing vouching: never signalled.
        assert_eq!(
            classify_ps(info.clone(), 7, None, no),
            Ownership::Unverified
        );
        assert_eq!(classify_ps(info, 7, None, |_| true), Ownership::Ours);
        // Some other program inherited the number.
        let other = parse_ps_line("Thu Oct  3 09:15:02 2026 /usr/bin/vim notes.txt");
        assert_eq!(classify_ps(other, 7, Some(t), no), Ownership::Gone);
        // `ps` absent, or unreadable output: not sure, so not Ours and not Gone.
        assert_eq!(
            classify_ps(PsInfo::Unknown, 7, Some(t), no),
            Ownership::Unverified
        );
        assert_eq!(classify_ps(PsInfo::Absent, 7, Some(t), no), Ownership::Gone);
        assert_eq!(parse_ps_line("garbage"), PsInfo::Unknown);
    }

    /// Launched through `/proc/self/exe`, argv[0] says nothing; the executable
    /// link does, with or without its "(deleted)" suffix.
    #[test]
    fn a_server_started_through_proc_self_exe_is_recognised() {
        let cmd = b"/proc/self/exe\0serve\0--addr\x00127.0.0.1:1\x00";
        assert!(is_server_proc(Some("/home/u/.local/bin/devctx"), cmd));
        assert!(is_server_proc(
            Some("/home/u/.local/bin/devctx (deleted)"),
            cmd
        ));
        assert!(!is_server_proc(Some("/usr/bin/vim"), cmd));
        assert!(!is_server_proc(None, cmd));
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::process::{Child, Command, Stdio};

    /// A child running a copy of `sh` named `devctx` as `devctx serve`, so it
    /// passes [`is_server_pid`]. The script `serve` in `dir` is its program.
    pub(crate) fn fake_server(dir: &Path, script: &str) -> Child {
        std::fs::create_dir_all(dir).unwrap();
        let exe = dir.join("devctx");
        if !exe.exists() {
            std::fs::copy("/bin/sh", &exe).unwrap();
        }
        std::fs::write(dir.join("serve"), script).unwrap();
        // Another thread forking while the copy was open can make the first
        // exec fail with ETXTBSY; it clears in microseconds.
        for _ in 0..50 {
            match Command::new(&exe)
                .arg("serve")
                .current_dir(dir)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(c) => return c,
                Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                Err(e) => panic!("spawning the fake server: {e}"),
            }
        }
        panic!("ETXTBSY never cleared");
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("devctx_procown_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn reap(mut c: Child) {
        let _ = c.kill();
        let _ = c.wait();
    }

    #[test]
    fn a_fake_server_is_identified_and_a_wrong_start_time_is_gone() {
        let dir = tmp("classify");
        let c = fake_server(&dir, "sleep 30\n");
        let pid = c.id();
        // Give the exec a moment to replace the forked image.
        assert!(wait_until(|| is_server_pid(pid), Duration::from_secs(3)));
        let t = start_time(pid).unwrap();
        assert_eq!(classify(pid, Some(t), |_| false), Ownership::Ours);
        assert_eq!(classify(pid, Some(t + 1), |_| true), Ownership::Gone);
        assert_eq!(classify(pid, None, |_| false), Ownership::Unverified);
        assert_eq!(classify(pid, None, |_| true), Ownership::Ours);
        reap(c);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A live process reports how long it has really been running.
    #[test]
    fn age_secs_of_a_live_process_counts_from_its_start() {
        let c = Command::new("sleep").arg("30").spawn().unwrap();
        std::thread::sleep(Duration::from_millis(2200));
        let age = age_secs(c.id()).unwrap();
        assert!((2..=6).contains(&age), "age {age}");
        reap(c);
    }

    #[test]
    fn the_cwd_must_be_the_root_not_merely_inside_it() {
        let dir = tmp("cwd");
        let c = fake_server(&dir, "sleep 30\n");
        let pid = c.id();
        assert!(wait_until(|| is_server_pid(pid), Duration::from_secs(3)));
        assert!(cwd_is(pid, &dir));
        // The server of a nested repository must not pass for its parent's.
        assert!(!cwd_is(pid, &dir.join("sub")));
        assert!(!cwd_is(pid, dir.parent().unwrap()));
        reap(c);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The reviewer's case: a server that ignores SIGTERM must be escalated to
    /// SIGKILL, and only then reported gone.
    #[test]
    fn a_process_that_ignores_sigterm_is_killed_and_reported_gone() {
        let dir = tmp("term");
        // `trap '' TERM` is inherited by `sleep`; the loop keeps sh alive.
        let mut c = fake_server(&dir, "trap '' TERM\nwhile :; do sleep 1; done\n");
        let pid = c.id();
        assert!(wait_until(|| is_server_pid(pid), Duration::from_secs(3)));
        let t = start_time(pid);
        // Let sh install the trap before we signal it.
        std::thread::sleep(Duration::from_millis(300));
        let owns = || classify(pid, t, |_| false) == Ownership::Ours;
        let gone = terminate(
            pid,
            owns,
            Duration::from_millis(500),
            Duration::from_secs(3),
        );
        assert!(gone.is_gone(), "SIGKILL escalation must remove it");
        let status = c.wait().unwrap();
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(libc::SIGKILL), "TERM was ignored");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// D1b: a server past the plain SIGTERM wait that says it is still
    /// checkpointing gets the patience it asks for — it exits on its own, not
    /// by SIGKILL — and one that does not say so is escalated as before.
    #[test]
    fn patience_holds_sigkill_off_while_the_process_asks_for_it() {
        use std::os::unix::process::ExitStatusExt;
        for patient in [true, false] {
            let dir = tmp(if patient { "patient" } else { "impatient" });
            let mut c = fake_server(
                &dir,
                "trap 'sleep 2; exit 0' TERM\nwhile :; do sleep 0.1; done\n",
            );
            let pid = c.id();
            assert!(wait_until(|| is_server_pid(pid), Duration::from_secs(3)));
            let t = start_time(pid);
            std::thread::sleep(Duration::from_millis(300));
            let owns = || classify(pid, t, |_| false) == Ownership::Ours;
            let out = terminate_patient(
                pid,
                owns,
                Duration::from_millis(300),
                || patient,
                Duration::from_secs(10),
                Duration::from_secs(3),
            );
            assert!(out.is_gone());
            let status = c.wait().unwrap();
            if patient {
                assert_eq!(status.code(), Some(0), "it was killed: {status:?}");
            } else {
                assert_eq!(status.signal(), Some(libc::SIGKILL), "{status:?}");
            }
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn a_process_that_cannot_be_stopped_is_reported_alive() {
        let dir = tmp("alive");
        let c = fake_server(&dir, "sleep 30\n");
        let pid = c.id();
        assert!(wait_until(|| is_server_pid(pid), Duration::from_secs(3)));
        // A wait that never sees it exit models a process that survives both
        // signals (a real one cannot be made to survive SIGKILL in a test).
        let handle = Handle::open(pid);
        let out = terminate_with(
            &handle,
            || true,
            Duration::from_millis(100),
            (&|| false, Duration::ZERO),
            Duration::from_millis(100),
            |_, limit, _| {
                std::thread::sleep(limit);
                false
            },
        );
        assert_eq!(out, Termination::Survived);
        reap(c);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// N2 (field): `serve --stop` returned while the process still held the
    /// DuckDB lock, and the next command met "Conflicting lock (PID <it>)".
    /// While the kernel tears down a big address space `/proc/<pid>/cmdline`
    /// already reads empty, so "no longer looks like ours" is not "dead".
    /// Here `owns` flips to false at once while the process is still busy
    /// shutting down: terminate must wait for the exit, not for that flip.
    #[test]
    fn terminate_returns_only_after_the_process_has_exited() {
        let dir = tmp("exited");
        let mut c = fake_server(
            &dir,
            "trap 'sleep 1; exit 0' TERM\nwhile :; do sleep 0.1; done\n",
        );
        let pid = c.id();
        assert!(wait_until(|| is_server_pid(pid), Duration::from_secs(3)));
        let t0 = start_time(pid).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let first = std::cell::Cell::new(true);
        let owns = || first.replace(false);
        let started = Instant::now();
        let out = terminate(pid, owns, Duration::from_secs(5), Duration::from_secs(2));
        assert_eq!(out, Termination::Gone);
        assert!(
            started.elapsed() >= Duration::from_millis(800),
            "returned after {:?}, before the process finished exiting",
            started.elapsed()
        );
        assert!(exited(pid, t0), "the process must be dead on return");
        let _ = c.wait();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A signal the kernel refuses is reported as such, not as "survived".
    /// Needs a process we may not signal: pid 1 for a non-root user.
    #[test]
    fn a_refused_signal_is_reported_as_no_permission() {
        // SAFETY: plain syscall.
        if unsafe { libc::geteuid() } == 0 {
            return; // root may signal pid 1: never run this as root.
        }
        let out = terminate(
            1,
            || true,
            Duration::from_millis(100),
            Duration::from_millis(100),
        );
        assert_eq!(out, Termination::NoPermission);
        assert!(out
            .failure("server", 1)
            .unwrap()
            .contains("no permission to signal PID 1"));
    }

    #[test]
    fn something_not_ours_is_never_signalled() {
        let mut c = Command::new("sleep").arg("30").spawn().unwrap();
        let pid = c.id();
        let owns = || classify(pid, start_time(pid), |_| true) == Ownership::Ours;
        assert!(terminate(
            pid,
            owns,
            Duration::from_millis(100),
            Duration::from_millis(100)
        )
        .is_gone());
        assert!(c.try_wait().unwrap().is_none(), "a foreign process died");
        reap(c);
    }

    #[test]
    fn the_start_time_is_field_22_even_with_parens_in_the_name() {
        let stat = "42 (we ird) name) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 777 23";
        assert_eq!(parse_start_time(stat), Some(777));
        assert_eq!(parse_start_time("garbage"), None);
    }
}
