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

/// Kernel start time of `pid` in clock ticks since boot. `None` off Linux or
/// when the process is gone.
pub fn start_time(pid: u32) -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        parse_start_time(&stat)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        None
    }
}

/// Whether `pid` is a live `devctx serve|api` process (see [`is_server_proc`]).
/// Says it is *a* devctx server, not that it is the one a file advertises.
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
        std::process::Command::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
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
    if !is_server_pid(pid) {
        return Ownership::Gone;
    }
    #[cfg(target_os = "linux")]
    {
        match recorded_start {
            Some(t) if start_time(pid) == Some(t) => Ownership::Ours,
            Some(_) => Ownership::Gone,
            None if fallback(pid) => Ownership::Ours,
            None => Ownership::Unverified,
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (recorded_start, fallback);
        Ownership::Ours
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
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            Handle { pid }
        }
    }

    fn signal(&self, sig: i32) {
        #[cfg(target_os = "linux")]
        if let Some(fd) = self.fd {
            // SAFETY: `fd` is a pidfd we own; a null siginfo is allowed.
            unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    fd as libc::c_long,
                    sig as libc::c_long,
                    std::ptr::null::<libc::c_void>(),
                    0 as libc::c_long,
                );
            }
            return;
        }
        #[cfg(unix)]
        // SAFETY: plain syscall with integer arguments.
        unsafe {
            libc::kill(self.pid as libc::pid_t, sig);
        }
        #[cfg(not(unix))]
        let _ = sig;
    }
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

/// Stop `pid`: SIGTERM, wait `term_wait`, then SIGKILL and wait `kill_wait`.
/// Returns whether the process is gone.
///
/// `owns` answers "is the process on this pid still the one we mean": it is
/// checked before any signal and is what the waits poll, so a process that
/// ignores SIGTERM is escalated rather than abandoned. Callers delete the
/// discovery file only when this returns `true`; deleting it while the process
/// lives leaves an unadvertised server holding the database lock.
pub fn terminate(
    pid: u32,
    owns: impl Fn() -> bool,
    term_wait: Duration,
    kill_wait: Duration,
) -> bool {
    // Open the handle before verifying, so what is verified is what is signalled.
    let handle = Handle::open(pid);
    if !owns() {
        return true;
    }
    #[cfg(unix)]
    {
        handle.signal(libc::SIGTERM);
        if wait_until(|| !owns(), term_wait) {
            return true;
        }
        handle.signal(libc::SIGKILL);
    }
    #[cfg(not(unix))]
    let _ = &handle;
    wait_until(|| !owns(), kill_wait)
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
        assert!(gone, "SIGKILL escalation must remove it");
        let status = c.wait().unwrap();
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(libc::SIGKILL), "TERM was ignored");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_process_that_cannot_be_stopped_is_reported_alive() {
        let dir = tmp("alive");
        let c = fake_server(&dir, "sleep 30\n");
        let pid = c.id();
        assert!(wait_until(|| is_server_pid(pid), Duration::from_secs(3)));
        // `owns` that never lets go models a process that survives both signals.
        let gone = terminate(
            pid,
            || true,
            Duration::from_millis(100),
            Duration::from_millis(100),
        );
        assert!(!gone);
        reap(c);
        let _ = std::fs::remove_dir_all(&dir);
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
        ));
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
