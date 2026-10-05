//! Helpers shared by the integration suites (`mod common;`).
//!
//! Not every suite uses every helper.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command};

/// Where the suites keep the embedding model: one cache for all of them.
///
/// Each test pins `DEVCTX_HOME` to its own temp directory, and the model cache
/// hangs off it — so every test that embedded anything downloaded the ~90 MB
/// model again, several at a time. That was slow, saturated the link, and left
/// half-finished downloads in the way of whoever ran next. The cache is content
/// addressed and read-only once filled, so sharing it is safe.
///
/// `DEVCTX_MODEL_CACHE` if the caller set it, otherwise a cache of the tests'
/// own under the cargo target directory (`test-model-cache`): never the user's
/// real `~/.local/share/devctx/models`, which a test run must not read, fill
/// or race a running server over. The first run that needs a model downloads
/// it into the dedicated cache; later runs reuse it.
pub fn shared_model_cache() -> PathBuf {
    resolve_model_cache(
        std::env::var_os("DEVCTX_MODEL_CACHE"),
        std::env::var_os("CARGO_TARGET_DIR"),
        Path::new(env!("CARGO_MANIFEST_DIR")),
    )
}

/// The pure part of [`shared_model_cache`].
pub fn resolve_model_cache(
    explicit: Option<std::ffi::OsString>,
    target_dir: Option<std::ffi::OsString>,
    manifest_dir: &Path,
) -> PathBuf {
    if let Some(explicit) = explicit.filter(|v| !v.is_empty()) {
        return PathBuf::from(explicit);
    }
    let target = target_dir
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        // crates/<name> -> workspace root -> target
        .unwrap_or_else(|| manifest_dir.join("..").join("..").join("target"));
    target.join("test-model-cache")
}

/// Make `home/models` (a test's `DEVCTX_HOME`) point at [`shared_model_cache`].
pub fn share_models(home: &Path) {
    let shared = shared_model_cache();
    let _ = std::fs::create_dir_all(&shared);
    let _ = std::fs::create_dir_all(home);
    let link = home.join("models");
    if link.exists() || link.symlink_metadata().is_ok() {
        return;
    }
    #[cfg(unix)]
    let _ = std::os::unix::fs::symlink(&shared, &link);
}

/// `ETXTBSY`: exec of a file some process still holds open for writing.
const ETXTBSY: i32 = 26;

/// Spawn, retrying while the executable is "text file busy".
///
/// A freshly copied binary can fail to exec with ETXTBSY even after the copy
/// is closed: a concurrent `fork` in another test thread briefly inherits the
/// write descriptor before its own `exec` closes it. The condition clears in
/// milliseconds, so retry instead of failing the test.
pub fn spawn_retrying(cmd: &mut Command) -> std::io::Result<Child> {
    let mut tries = 0;
    loop {
        match cmd.spawn() {
            Err(e) if e.raw_os_error() == Some(ETXTBSY) && tries < 100 => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            other => return other,
        }
    }
}

/// Install `src` at `dest` the way a package manager does: a private temp
/// file, fully written, fsynced and closed, then renamed into place.
pub fn install_copy(src: &Path, dest: &Path) {
    use std::io::Write;
    let staged = dest.with_extension("staged");
    {
        let mut out = std::fs::File::create(&staged).expect("creating the staged binary");
        let mut input = std::fs::File::open(src).expect("opening the source binary");
        std::io::copy(&mut input, &mut out).expect("copying the binary");
        out.flush().unwrap();
        out.sync_all().unwrap();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755));
    }
    std::fs::rename(&staged, dest).expect("renaming the binary into place");
}

/// Every live `devctx serve|api` whose working directory is under `root`.
pub fn servers_under(root: &Path) -> Vec<u32> {
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let mut found = Vec::new();
    let Ok(procs) = std::fs::read_dir("/proc") else {
        return found;
    };
    for entry in procs.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        if !devctx_core::procown::is_server_pid(pid) {
            continue;
        }
        let Ok(cwd) = std::fs::read_link(format!("/proc/{pid}/cwd")) else {
            continue;
        };
        let cwd = PathBuf::from(
            cwd.to_string_lossy()
                .trim_end_matches(" (deleted)")
                .to_string(),
        );
        if cwd.starts_with(&root) {
            found.push(pid);
        }
    }
    found
}

/// Stop every server whose cwd is under `root` that this test started, through
/// `procown::terminate` (SIGTERM, then SIGKILL), and return what was left
/// standing afterwards (empty when all is well).
///
/// Verified before every signal: the process must still be a devctx server
/// *and* its cwd must still be under `root`, so nothing a test did not start
/// is ever touched.
pub fn stop_servers_under(root: &Path) -> Vec<u32> {
    use std::time::Duration;
    for pid in servers_under(root) {
        let root = root.to_path_buf();
        let owns =
            move || devctx_core::procown::is_server_pid(pid) && servers_under(&root).contains(&pid);
        devctx_core::procown::terminate(pid, owns, Duration::from_secs(5), Duration::from_secs(5));
    }
    servers_under(root)
}

/// What a test's `Drop` does after its own `serve --stop` calls: reap any server
/// still living under `root` (one that never wrote a `serve.json`, one a
/// late-starting MCP spawned after the sweep, one launched by hand), and fail
/// the test run if any survive. A suite that leaves servers behind is the leak
/// that made later runs flaky (PLAN-008 B9), so it is an error, not a shrug.
pub fn reap_servers_under(root: &Path) {
    let leaked = servers_under(root);
    if !leaked.is_empty() {
        eprintln!("note: reaping server(s) {leaked:?} the test left under {root:?}");
    }
    let mut left = stop_servers_under(root);
    if !left.is_empty() {
        // A server that was mid-spawn when the first pass ran.
        std::thread::sleep(std::time::Duration::from_millis(300));
        left = stop_servers_under(root);
    }
    if !left.is_empty() && !std::thread::panicking() {
        panic!("devctx serve process(es) {left:?} outlived the test under {root:?}");
    }
}
