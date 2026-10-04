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

use std::path::Path;
use std::sync::mpsc;
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
    let Some(limit) = stall_limit() else {
        return Ok(load());
    };
    let cache = crate::dirs::model_cache_dir();
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("model-load".into())
        .spawn(move || {
            let _ = tx.send(load());
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
            return Err(format!(
                "loading {what} made no progress for {}s (no data arrived from the model host); \
                 check the network, set HF_ENDPOINT to a reachable mirror, or fetch it with \
                 `devctx models --download`. Raise {MODEL_STALL_ENV} if the link is that slow.",
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
