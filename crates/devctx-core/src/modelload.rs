//! A stall guard for loading models that may have to be downloaded first.
//!
//! fastembed fetches a missing model through hf-hub's synchronous `ureq`
//! client, which has **no read timeout** and which a caller cannot replace. A
//! connection that goes quiet after the TLS handshake therefore blocks the
//! loading thread forever — and, since the thread holds the embedder lock and
//! sits on a runtime's blocking pool, it used to take the whole server's
//! shutdown with it (PLAN-008 B10).
//!
//! The guard runs the load on its own thread and watches the model cache: a
//! download that is alive makes its files grow, one that is not leaves them
//! untouched. When nothing has changed for [`stall_limit`] the caller gets an
//! error and the stuck thread is abandoned (it dies with the process). This is
//! progress-based, not a wall-clock cap, so a slow-but-moving download over a
//! poor link is never cut off.
//!
//! An abandoned loader keeps whatever it holds — in particular hf-hub's
//! exclusive `flock` on the model's cache entry. A second attempt in the same
//! process cannot take that lock (hf-hub tries `LOCK_NB` five times a second
//! apart and fails with `LockAcquisition`), and would leave one more stuck
//! thread behind. So a stall **poisons** the model for the rest of the process:
//! every later load of it fails at once with the same instruction — restart the
//! server — instead of a confusing lock error after five seconds. The poison
//! lasts only as long as its cause: an abandoned loader that does finish after
//! all (the link came back) releases the lock, and lifts the poison with it.

use std::collections::HashSet;
use std::path::Path;
use std::sync::{mpsc, Mutex};
use std::time::{Duration, Instant};

/// Seconds without any growth of the model cache before a load is declared
/// stuck. `0` disables the guard (the load then runs inline, unbounded).
pub const MODEL_STALL_ENV: &str = "DEVCTX_MODEL_STALL_SECS";

const DEFAULT_STALL_SECS: u64 = 120;

/// How long a model load may make no progress before it is abandoned.
pub fn stall_limit() -> Option<Duration> {
    let secs = std::env::var(MODEL_STALL_ENV)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_STALL_SECS);
    (secs > 0).then(|| Duration::from_secs(secs))
}

/// Marker present in every stall error, so callers can tell a stall from an
/// ordinary load failure (and not, say, retry it on another device).
pub const STALL_MARKER: &str = "model download stalled";

/// Whether `message` is (or wraps) a stall reported by [`guard_load`].
pub fn is_stall_error(message: &str) -> bool {
    message.contains(STALL_MARKER)
}

/// Models whose load stalled in this process (see the module docs).
static POISONED: Mutex<Option<HashSet<String>>> = Mutex::new(None);

fn poisoned_lock() -> std::sync::MutexGuard<'static, Option<HashSet<String>>> {
    POISONED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Whether a load of `what` already stalled in this process.
pub fn is_poisoned(what: &str) -> bool {
    poisoned_lock().as_ref().is_some_and(|s| s.contains(what))
}

fn poison(what: &str) {
    poisoned_lock()
        .get_or_insert_with(HashSet::new)
        .insert(what.to_string());
}

/// The loader of `what` finished: whatever lock it held is released.
fn unpoison(what: &str) {
    if let Some(set) = poisoned_lock().as_mut() {
        set.remove(what);
    }
}

fn poisoned_error(what: &str) -> String {
    format!(
        "{STALL_MARKER}: an earlier load of {what} in this process stopped receiving data and \
         its download is still holding the model cache lock, so it cannot be retried here. \
         Restart the server (`devctx serve --stop`; the next command starts a fresh one), or \
         fetch the model with `devctx models --download`."
    )
}

/// Fingerprint of the shared model cache (see [`fingerprint`]); `None` when no
/// cache directory can be determined. Lets a caller tell a download that is
/// moving from one that is not, e.g. to report progress while it waits.
pub fn cache_fingerprint() -> Option<(u64, u64)> {
    crate::dirs::model_cache_dir().map(|d| fingerprint(&d))
}

/// Total size and file count under `dir`, symlinks not followed. A cheap
/// fingerprint: a running download changes it, an idle one does not.
fn fingerprint(dir: &Path) -> (u64, u64) {
    let mut bytes = 0u64;
    let mut files = 0u64;
    let mut pending = vec![dir.to_path_buf()];
    while let Some(d) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let Ok(md) = e.metadata() else { continue };
            if md.is_dir() {
                pending.push(e.path());
            } else {
                files += 1;
                bytes += md.len();
            }
        }
    }
    (bytes, files)
}

/// Run `load` on its own thread, giving up when the model cache stops changing
/// for [`stall_limit`]. `what` names the model in the error.
pub fn guard_load<T: Send + 'static>(
    what: &str,
    load: impl FnOnce() -> T + Send + 'static,
) -> Result<T, String> {
    guard_load_with(what, stall_limit(), load)
}

/// [`guard_load`] with the stall limit passed in rather than read from
/// [`MODEL_STALL_ENV`] — the environment is process-global, and a test that
/// set it leaked a one-second limit into every other test of the binary.
fn guard_load_with<T: Send + 'static>(
    what: &str,
    limit: Option<Duration>,
    load: impl FnOnce() -> T + Send + 'static,
) -> Result<T, String> {
    if is_poisoned(what) {
        return Err(poisoned_error(what));
    }
    let Some(limit) = limit else {
        return Ok(load());
    };
    let cache = crate::dirs::model_cache_dir();
    let (tx, rx) = mpsc::channel();
    let name = what.to_string();
    std::thread::Builder::new()
        .name("model-load".into())
        .spawn(move || {
            let value = load();
            // Finished — in time, or long after it was abandoned: either way
            // it no longer holds the cache lock, and the model may be tried
            // again.
            unpoison(&name);
            let _ = tx.send(value);
        })
        .map_err(|e| format!("spawning the loader for {what}: {e}"))?;

    let tick = (limit / 4).clamp(Duration::from_millis(100), Duration::from_secs(1));
    let mut last = cache.as_deref().map(fingerprint);
    let mut changed = Instant::now();
    loop {
        match rx.recv_timeout(tick) {
            Ok(v) => return Ok(v),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(format!("loading {what} aborted unexpectedly"))
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        let now = cache.as_deref().map(fingerprint);
        if now != last {
            last = now;
            changed = Instant::now();
        } else if changed.elapsed() >= limit {
            // The loader thread is abandoned with whatever it holds; see the
            // module docs for why nothing in this process may try again.
            poison(what);
            return Err(format!(
                "{STALL_MARKER}: loading {what} made no progress for {}s (no data arrived from \
                 the model host); check the network, set HF_ENDPOINT to a reachable mirror, or \
                 fetch it with `devctx models --download`. Raise {MODEL_STALL_ENV} if the link \
                 is that slow. The stuck download keeps the model locked until this process \
                 ends: restart the server (`devctx serve --stop`) before retrying.",
                limit.as_secs()
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_load_that_finishes_returns_its_value() {
        assert_eq!(guard_load("m", || 7).unwrap(), 7);
    }

    /// Fixup D1 (item 5): once a load stalled, its thread still holds hf-hub's
    /// lock, so the next attempt in the same process must fail at once with the
    /// restart instruction — not run (and hang, or fail on the lock) again.
    #[test]
    fn a_stalled_load_poisons_the_model_for_the_process() {
        let limit = Some(Duration::from_secs(1));
        let key = format!("test-stall-{}", std::process::id());
        let err = guard_load_with(&key, limit, || std::thread::sleep(Duration::from_secs(30)))
            .expect_err("a load that never finishes must stall");
        assert!(is_stall_error(&err), "{err}");
        assert!(err.contains("devctx serve --stop"), "{err}");
        assert!(is_poisoned(&key));

        let ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = ran.clone();
        let t0 = Instant::now();
        let again = guard_load_with(&key, limit, move || {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        })
        .expect_err("a poisoned model must not be loaded again");
        assert!(is_stall_error(&again), "{again}");
        assert!(again.contains("Restart the server"), "{again}");
        assert!(
            t0.elapsed() < Duration::from_millis(500),
            "it fails at once"
        );
        assert!(
            !ran.load(std::sync::atomic::Ordering::SeqCst),
            "the loader must not run again"
        );
        // Other models are unaffected.
        assert_eq!(guard_load_with("another-model", limit, || 3).unwrap(), 3);
    }

    /// D1b nit: the poison is about a lock the abandoned loader holds. When
    /// that loader finishes after all, the lock is gone and so is the reason
    /// to refuse the model.
    #[test]
    fn the_poison_lifts_once_the_abandoned_loader_finishes() {
        let key = format!("test-unpoison-{}", std::process::id());
        let err = guard_load_with(&key, Some(Duration::from_millis(300)), || {
            std::thread::sleep(Duration::from_millis(1500))
        })
        .expect_err("it stalls past the limit");
        assert!(is_stall_error(&err));
        assert!(is_poisoned(&key));
        let t0 = Instant::now();
        while is_poisoned(&key) {
            assert!(
                t0.elapsed() < Duration::from_secs(10),
                "the poison outlived the loader"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(
            guard_load_with(&key, Some(Duration::from_secs(5)), || 9).unwrap(),
            9
        );
    }

    #[test]
    fn a_fingerprint_follows_growth() {
        let d = std::env::temp_dir().join(format!("devctx_fp_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let a = fingerprint(&d);
        std::fs::write(d.join("x"), b"abc").unwrap();
        assert_ne!(a, fingerprint(&d));
        let _ = std::fs::remove_dir_all(&d);
    }
}
