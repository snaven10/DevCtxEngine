//! `Cargo.lock` / `Cargo.toml` reading shared by `build.rs` and its tests.
#![allow(dead_code)]

/// Every `(name, version)` of the `[[package]]` blocks of a lock file
/// (LF or CRLF).
pub fn packages(lock: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for block in lock.split("[[package]]").skip(1) {
        let (mut name, mut version) = (None, None);
        for line in block.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                break; // next table: the package's own keys are over
            }
            let val = |k: &str| {
                line.strip_prefix(k)
                    .and_then(|v| v.trim_start().strip_prefix('='))
                    .map(|v| v.trim().trim_matches('"').to_string())
            };
            if name.is_none() {
                name = val("name");
            }
            if version.is_none() {
                version = val("version");
            }
        }
        if let (Some(n), Some(v)) = (name, version) {
            out.push((n, v));
        }
    }
    out
}

/// The version requirement a manifest declares for `dep` (inline table or
/// plain string), e.g. `4` or `=2.0.0-rc.9`.
pub fn manifest_spec(manifest: &str, dep: &str) -> Option<String> {
    for line in manifest.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix(dep) else {
            continue;
        };
        let Some(rest) = rest.trim_start().strip_prefix('=') else {
            continue;
        };
        let rest = rest.trim();
        let after = match rest.find("version") {
            Some(i) if rest.starts_with('{') => rest[i + 7..].trim_start().strip_prefix('=')?,
            _ => rest,
        };
        let after = after.trim_start().strip_prefix('"')?;
        return after.split('"').next().map(str::to_string);
    }
    None
}

/// Whether `version` satisfies `spec` (`=x.y.z` exact; otherwise the spec's
/// components must prefix the version's, which is what `4` / `^4.1` ask for).
pub fn satisfies(version: &str, spec: &str) -> bool {
    let spec = spec.trim();
    if let Some(exact) = spec.strip_prefix('=') {
        return version == exact.trim();
    }
    let spec = spec.trim_start_matches(['^', '~']);
    let want: Vec<&str> = spec.split('.').collect();
    let have: Vec<&str> = version.split('.').collect();
    want.len() <= have.len() && want.iter().zip(&have).all(|(a, b)| a == b)
}

/// The locked version of `package`. With several versions in the lock, the one
/// the manifest requires; `Err` if that is not decidable.
pub fn locked_version(lock: &str, manifest: &str, package: &str) -> Result<Option<String>, String> {
    let mut found: Vec<String> = packages(lock)
        .into_iter()
        .filter(|(n, _)| n == package)
        .map(|(_, v)| v)
        .collect();
    match found.len() {
        0 => Ok(None),
        1 => Ok(found.pop()),
        _ => {
            let spec = manifest_spec(manifest, package);
            let mut ok: Vec<String> = match &spec {
                Some(s) => found.iter().filter(|v| satisfies(v, s)).cloned().collect(),
                None => Vec::new(),
            };
            if ok.len() == 1 {
                Ok(ok.pop())
            } else {
                Err(format!(
                    "Cargo.lock holds several `{package}` versions ({}) and the devctx-embed \
                     manifest requirement ({}) does not pick exactly one",
                    found.join(", "),
                    spec.as_deref().unwrap_or("none")
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCK: &str = "[[package]]\r\nname = \"ort\"\r\nversion = \"1.16.3\"\r\n\r\n\
        [[package]]\r\nname = \"ort\"\r\nversion = \"2.0.0-rc.9\"\r\ndependencies = [\r\n \"x\",\r\n]\r\n";

    #[test]
    fn reads_crlf_and_picks_the_required_version() {
        assert_eq!(packages(LOCK).len(), 2);
        let manifest = "ort = { version = \"=2.0.0-rc.9\", default-features = false }";
        assert_eq!(
            locked_version(LOCK, manifest, "ort").unwrap().as_deref(),
            Some("2.0.0-rc.9")
        );
        assert!(locked_version(LOCK, "", "ort").is_err());
        assert_eq!(locked_version(LOCK, manifest, "nope").unwrap(), None);
    }

    #[test]
    fn specs() {
        assert_eq!(
            manifest_spec("fastembed = \"4\"", "fastembed").as_deref(),
            Some("4")
        );
        assert_eq!(
            manifest_spec(
                "fastembed = { version = \"4\", optional = true }",
                "fastembed"
            )
            .as_deref(),
            Some("4")
        );
        assert!(satisfies("4.9.1", "4") && !satisfies("5.0.0", "4"));
        assert!(satisfies("1.2.3", "^1.2") && !satisfies("1.3.0", "=1.2.3"));
    }
}
