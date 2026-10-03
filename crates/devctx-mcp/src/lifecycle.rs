//! What the MCP process knows about its own binary (PLAN-008 D2).
//!
//! The MCP lives as long as its client's session, and a reinstall of `devctx`
//! during that time leaves it running the old code. It must not exit or restart
//! itself over that — the client would see a dead server mid-session — but the
//! agent should be told, so the person can restart the session. Everything here
//! stays out of the `initialize` path: the facts are captured with two `stat`s
//! at startup, and the `--version` probe runs only when a tool asks, and only
//! if the binary was actually replaced.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};

/// How long `<exe> --version` may take before it is given up on.
const VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Suffix `/proc/self/exe` carries once the running file was unlinked.
const DELETED_SUFFIX: &str = " (deleted)";

#[derive(Debug)]
struct StartInfo {
    version: &'static str,
    /// Where `current_exe` pointed when the process started.
    exe: Option<PathBuf>,
    /// Modification time of the file at `exe`, at startup.
    mtime: Option<SystemTime>,
}

static START: OnceLock<StartInfo> = OnceLock::new();
static INSTALLED: OnceLock<Option<String>> = OnceLock::new();

/// Capture the startup facts. Call once before serving; later calls are no-ops.
pub fn init() {
    let _ = START.get_or_init(capture);
}

fn capture() -> StartInfo {
    let exe = devctx_core::self_exe().ok();
    let mtime = exe.as_deref().and_then(mtime_of);
    StartInfo {
        version: env!("CARGO_PKG_VERSION"),
        exe,
        mtime,
    }
}

fn mtime_of(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Was the binary this process runs replaced on disk since it started?
///
/// Either the kernel says the running file is gone (`install`/`mv` over it
/// unlinks the old inode), or the file at the path changed under us.
fn binary_replaced(start: &StartInfo) -> bool {
    let raw_deleted = std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(|s| s.ends_with(DELETED_SUFFIX)))
        .unwrap_or(false);
    if raw_deleted {
        return true;
    }
    match (&start.exe, start.mtime) {
        (Some(exe), Some(at_start)) => mtime_of(exe).is_some_and(|now| now != at_start),
        _ => false,
    }
}

/// Run `exe --version` with a hard timeout and return the version number.
fn probe_version(exe: &Path) -> Option<String> {
    let mut child = Command::new(exe)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + VERSION_PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    let out = child.wait_with_output().ok()?;
    parse_version(&String::from_utf8_lossy(&out.stdout))
}

/// `devctx 0.8.3` -> `0.8.3`.
fn parse_version(output: &str) -> Option<String> {
    output
        .split_whitespace()
        .last()
        .filter(|v| v.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .map(str::to_string)
}

/// The version of the binary now installed at this process's path, probed once.
fn installed_version(start: &StartInfo) -> Option<String> {
    INSTALLED
        .get_or_init(|| {
            let exe = start.exe.as_deref()?;
            // Without a file at the path there is nothing to ask (the binary
            // was removed, not replaced).
            if !exe.is_file() || exe == Path::new("/proc/self/exe") {
                return None;
            }
            probe_version(exe)
        })
        .clone()
}

/// The `"mcp"` object for `index_status` and `list_projects`.
///
/// May spawn `<exe> --version` (at most once, 2 s cap) when the binary was
/// replaced, so call it from a blocking context.
pub fn mcp_json() -> Value {
    let start = START.get_or_init(capture);
    let replaced = binary_replaced(start);
    let installed = if replaced {
        installed_version(start)
    } else {
        None
    };
    describe(start.version, replaced, installed)
}

fn describe(version: &str, replaced: bool, installed: Option<String>) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("version".into(), json!(version));
    obj.insert("binary_replaced".into(), json!(replaced));
    if let Some(v) = &installed {
        obj.insert("installed_version".into(), json!(v));
    }
    if replaced {
        let hint = match installed.as_deref() {
            Some(v) if v != version => format!(
                "this session runs devctx {version}; {v} is installed — restart the AI \
                 session to pick it up"
            ),
            Some(_) => format!(
                "the devctx binary was replaced on disk (same version, {version}) — restart \
                 the AI session to pick up the new build"
            ),
            None => format!(
                "this session runs devctx {version}, but the binary on disk was replaced — \
                 restart the AI session to pick up the installed one"
            ),
        };
        obj.insert("hint".into(), json!(hint));
    }
    Value::Object(obj)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_clap_version_line() {
        assert_eq!(parse_version("devctx 0.8.3\n").as_deref(), Some("0.8.3"));
        assert_eq!(parse_version("").as_deref(), None);
        assert_eq!(parse_version("error: nope").as_deref(), None);
    }

    #[test]
    fn current_binary_is_not_replaced() {
        init();
        let v = mcp_json();
        assert_eq!(v["binary_replaced"], json!(false));
        assert!(v.get("hint").is_none());
        assert_eq!(v["version"], json!(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn a_replaced_binary_suggests_restarting_with_both_versions() {
        let v = describe("0.8.2", true, Some("0.8.3".into()));
        assert_eq!(v["installed_version"], json!("0.8.3"));
        let hint = v["hint"].as_str().unwrap();
        assert!(hint.contains("0.8.2") && hint.contains("0.8.3") && hint.contains("restart"));
    }

    #[test]
    fn a_replaced_binary_with_unknown_installed_version_still_hints() {
        let v = describe("0.8.2", true, None);
        assert!(v.get("installed_version").is_none());
        assert!(v["hint"].as_str().unwrap().contains("restart"));
    }

    #[test]
    fn a_hanging_probe_gives_up() {
        // `sleep` ignores `--version`'s meaning and just runs: not a devctx, and
        // slower than the cap would allow if it were waited on.
        let t = std::time::Instant::now();
        let got = probe_version(Path::new("/bin/sleep"));
        assert!(got.is_none());
        assert!(t.elapsed() < Duration::from_secs(5));
    }
}
