//! "There is a newer version" — checked rarely, reported quietly, never acted on.
//!
//! Three constraints shape this. It must not slow anything down, so the answer
//! is cached and the check happens at most once a day. It must not fail
//! anything, so every error — no network, a rate limit, a malformed reply — is
//! silence. And it must not *act*: a binary that replaces itself under a
//! running server would swap the code out from under an index in flight, and
//! the running processes go on holding the old image anyway, which is precisely
//! the confusion this is meant to prevent rather than cause.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How long a check is trusted before asking again.
const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// Set to any value to never check.
const OPT_OUT: &str = "DEVCTX_NO_UPDATE_CHECK";

/// The newest published version, when it is newer than `current`.
///
/// `None` covers every uninteresting case: up to date, checked recently, opted
/// out, or unreachable. Callers print it or ignore it; nothing branches on the
/// failure, because there is nothing a user could do about it here.
pub fn available(repo: &str, current: &str) -> Option<String> {
    if std::env::var_os(OPT_OUT).is_some() {
        return None;
    }
    let cache = cache_path()?;
    if let Some((checked_at, latest)) = read_cache(&cache) {
        if now().saturating_sub(checked_at) < CHECK_EVERY.as_secs() {
            return newer(&latest, current);
        }
    }
    let latest = fetch_latest(repo)?;
    write_cache(&cache, &latest);
    newer(&latest, current)
}

/// Compare as version *numbers*, not strings: `0.1.10` is newer than `0.1.9`,
/// which a lexical comparison gets backwards.
fn newer(latest: &str, current: &str) -> Option<String> {
    let parse = |v: &str| -> Vec<u64> {
        v.trim_start_matches('v')
            .split('.')
            .map(|p| p.chars().take_while(char::is_ascii_digit).collect::<String>())
            .map(|p| p.parse().unwrap_or(0))
            .collect()
    };
    (parse(latest) > parse(current)).then(|| latest.to_string())
}

fn fetch_latest(repo: &str) -> Option<String> {
    let resp = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(4))
        .build()
        .get(&format!("https://api.github.com/repos/{repo}/releases/latest"))
        .set("User-Agent", "devctx")
        .call()
        .ok()?;
    let body: serde_json::Value = resp.into_json().ok()?;
    body.get("tag_name")?.as_str().map(str::to_string)
}

fn cache_path() -> Option<PathBuf> {
    Some(devctx_core::dirs::data_dir()?.join("update-check.json"))
}

fn read_cache(path: &PathBuf) -> Option<(u64, String)> {
    let raw = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    Some((
        v.get("checked_at")?.as_u64()?,
        v.get("latest")?.as_str()?.to_string(),
    ))
}

fn write_cache(path: &PathBuf, latest: &str) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(
        path,
        serde_json::json!({ "checked_at": now(), "latest": latest }).to_string(),
    );
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::newer;

    /// Versions are ordered numerically. A lexical comparison calls 0.1.10
    /// older than 0.1.9 and would then stop reporting updates at exactly the
    /// point releases start accumulating.
    #[test]
    fn versions_compare_as_numbers() {
        assert_eq!(newer("v0.1.2", "0.1.1").as_deref(), Some("v0.1.2"));
        assert_eq!(newer("v0.1.10", "0.1.9").as_deref(), Some("v0.1.10"));
        assert_eq!(newer("v0.2.0", "0.1.99").as_deref(), Some("v0.2.0"));
        assert!(newer("v0.1.1", "0.1.1").is_none());
        assert!(newer("v0.1.0", "0.1.1").is_none(), "never suggest downgrading");
        // Garbage must not be read as a new release.
        assert!(newer("not-a-version", "0.1.1").is_none());
    }
}
