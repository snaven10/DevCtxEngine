//! How much memory this process holds, read from the kernel (PLAN-010 DD-1).
//!
//! `status` reports it so "devctx eats RAM" has an answer without reaching for
//! `/proc` by hand. Linux only: elsewhere there is no cheap, honest source, so
//! [`ProcMemory::read`] returns `None` and [`process_json`] says `unavailable`
//! rather than inventing zeros.

use serde_json::{json, Value};

/// Resident-set figures of one process, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProcMemory {
    /// `VmRSS`: everything resident.
    pub rss: u64,
    /// `RssAnon`: private heap, model weights, ORT arenas, DuckDB buffers.
    /// `None` on kernels < 4.5, which do not report it (never a fake zero).
    pub rss_anon: Option<u64>,
    /// `RssFile`: mapped files (the binary, its libraries). `None` if absent.
    pub rss_file: Option<u64>,
    /// `RssShmem`: shared memory. `None` if absent.
    pub rss_shmem: Option<u64>,
    /// `VmHWM`: peak `VmRSS` since the process started.
    pub hwm: u64,
}

impl ProcMemory {
    /// Parse the text of `/proc/<pid>/status`. Pure, so it is tested on a fixed
    /// sample on every platform. `None` when no `VmRSS` line is present (a
    /// kernel thread, or text that is not a status file).
    pub fn parse_status(text: &str) -> Option<Self> {
        let mut m = Self::default();
        let mut seen_rss = false;
        let mut hwm_seen = false;
        for line in text.lines() {
            let Some((key, rest)) = line.split_once(':') else {
                continue;
            };
            let Some(bytes) = parse_kb(rest) else {
                continue;
            };
            match key {
                "VmRSS" => {
                    seen_rss = true;
                    m.rss = bytes;
                }
                "RssAnon" => m.rss_anon = Some(bytes),
                "RssFile" => m.rss_file = Some(bytes),
                "RssShmem" => m.rss_shmem = Some(bytes),
                "VmHWM" => {
                    hwm_seen = true;
                    m.hwm = bytes;
                }
                _ => {}
            }
        }
        // Without VmHWM the peak is unknown; the current RSS is its lower bound.
        if !hwm_seen {
            m.hwm = m.rss;
        }
        seen_rss.then_some(m)
    }

    /// This process's figures, or `None` where they cannot be read (any OS but
    /// Linux, or a `/proc` that is not mounted).
    #[cfg(target_os = "linux")]
    pub fn read() -> Option<Self> {
        let text = std::fs::read_to_string("/proc/self/status").ok()?;
        Self::parse_status(&text)
    }

    /// See the Linux version.
    #[cfg(not(target_os = "linux"))]
    pub fn read() -> Option<Self> {
        None
    }
}

/// `"   123456 kB"` to bytes.
fn parse_kb(rest: &str) -> Option<u64> {
    let mut it = rest.split_whitespace();
    let n: u64 = it.next()?.parse().ok()?;
    let unit = it.next().unwrap_or("kB");
    (unit.eq_ignore_ascii_case("kB")).then(|| n.saturating_mul(1024))
}

/// Name of the engine that runs an embedder/reranker for `provider`: the
/// in-process one for `local`, otherwise `remote`. One place to change when the
/// local engine does.
pub fn engine_for(provider: &str) -> &'static str {
    if provider == "local" {
        LOCAL_ENGINE
    } else {
        "remote"
    }
}

/// The in-process inference engine behind `provider: local`.
pub const LOCAL_ENGINE: &str = "fastembed";

/// The `memory.process` block of `status`.
pub fn process_json(mem: Option<ProcMemory>) -> Value {
    match mem {
        Some(m) => json!({
            "rss_bytes": m.rss,
            "rss_anon_bytes": m.rss_anon,   // null when the kernel omits it
            "rss_file_bytes": m.rss_file,
            "rss_shmem_bytes": m.rss_shmem,
            "hwm_bytes": m.hwm,
            "source": "proc",
        }),
        None => json!({
            "rss_bytes": null,
            "rss_anon_bytes": null,
            "rss_file_bytes": null,
            "rss_shmem_bytes": null,
            "hwm_bytes": null,
            "source": "unavailable",
            "reason": "process memory is read from /proc, which only Linux provides",
        }),
    }
}

/// Current RSS in bytes, for log lines; `None` off Linux.
pub fn rss_bytes() -> Option<u64> {
    ProcMemory::read().map(|m| m.rss)
}

/// Seconds since the Unix epoch, for `loaded_at` (no calendar crate in the tree).
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str =
        "Name:\tdevctx\nVmPeak:\t 900000 kB\nVmHWM:\t  600000 kB\nVmRSS:\t  512000 kB\n\
RssAnon:\t  400000 kB\nRssFile:\t  100000 kB\nRssShmem:\t   12000 kB\nThreads:\t12\n";

    #[test]
    fn parses_a_status_file_into_bytes() {
        let m = ProcMemory::parse_status(SAMPLE).unwrap();
        assert_eq!(m.rss, 512_000 * 1024);
        assert_eq!(m.rss_anon, Some(400_000 * 1024));
        assert_eq!(m.rss_file, Some(100_000 * 1024));
        assert_eq!(m.rss_shmem, Some(12_000 * 1024));
        assert_eq!(m.hwm, 600_000 * 1024);
    }

    #[test]
    fn text_without_vmrss_is_not_a_status_file() {
        assert_eq!(ProcMemory::parse_status("Name:\tkthreadd\n"), None);
        assert_eq!(ProcMemory::parse_status(""), None);
    }

    #[test]
    fn old_kernel_without_rss_breakdown_gives_none_not_zero() {
        let m = ProcMemory::parse_status("VmHWM:\t 2000 kB\nVmRSS:\t 1000 kB\n").unwrap();
        assert_eq!(m.rss_anon, None);
        assert_eq!(m.rss_file, None);
        assert_eq!(m.rss_shmem, None);
        let v = process_json(Some(m));
        assert_eq!(v["source"], "proc");
        assert!(v["rss_anon_bytes"].is_null());
        assert!(v["rss_bytes"].is_u64());
    }

    #[test]
    fn unavailable_is_explicit_not_zero() {
        let v = process_json(None);
        assert_eq!(v["source"], "unavailable");
        assert!(v["rss_bytes"].is_null());
    }

    #[test]
    fn available_block_has_every_field() {
        let v = process_json(ProcMemory::parse_status(SAMPLE));
        assert_eq!(v["source"], "proc");
        for k in [
            "rss_bytes",
            "rss_anon_bytes",
            "rss_file_bytes",
            "rss_shmem_bytes",
            "hwm_bytes",
        ] {
            assert!(v[k].is_u64(), "{k}");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn reads_this_process() {
        let m = ProcMemory::read().expect("/proc/self/status");
        assert!(m.rss > 0 && m.hwm >= m.rss / 2);
    }
}
