//! The workspace's `tsconfig` for the link pass (PLAN-009 TASK-006, DD-7):
//! TypeScript/JavaScript imports through `paths` aliases (an Nx workspace's
//! `@acme/...`) and `baseUrl` resolve only with it.
//!
//! Best effort: `tsconfig.base.json` at the repository root, else
//! `tsconfig.json`; JSON with comments and trailing commas; one level of a
//! relative `extends` (what the file sets wins). A file that does not parse
//! leaves only relative imports, and the run says so once.

use devctx_parse::resolve::typescript::{dir_of, join, TsConfig};

/// The candidates, in the order they are tried.
const NAMES: &[&str] = &["tsconfig.base.json", "tsconfig.json"];

/// What was found: the config (`None` when no file exists or none parses)
/// and, when a file exists but could not be read as a `tsconfig`, which.
#[derive(Debug, Default)]
pub(crate) struct Loaded {
    pub config: Option<TsConfig>,
    pub unreadable: Option<String>,
}

/// Load the root `tsconfig` through `read` (a repository path → its text, or
/// `None` when it does not exist on the branch being indexed).
pub(crate) fn load(read: &dyn Fn(&str) -> Option<String>) -> Loaded {
    for name in NAMES {
        let Some(text) = read(name) else { continue };
        let Some(own) = TsConfig::parse(&text, dir_of(name)) else {
            return Loaded {
                config: None,
                unreadable: Some((*name).to_string()),
            };
        };
        let base = TsConfig::extends_of(&text)
            .map(|e| join(dir_of(name), &e))
            .and_then(|path| {
                let path = if path.ends_with(".json") {
                    path
                } else {
                    format!("{path}.json")
                };
                let text = read(&path)?;
                TsConfig::parse(&text, dir_of(&path))
            });
        let config = match base {
            Some(b) => own.over(b),
            None => own,
        };
        return Loaded {
            config: Some(config),
            unreadable: None,
        };
    }
    Loaded::default()
}

/// The `index_meta` value a branch was linked under: the config's
/// fingerprint, or `none`.
pub(crate) fn fingerprint(config: Option<&TsConfig>) -> String {
    config.map_or_else(|| "none".to_string(), TsConfig::fingerprint)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn reader(files: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = files
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |p: &str| map.get(p).cloned()
    }

    #[test]
    fn the_base_file_wins_and_extends_is_followed_once() {
        let read = reader(&[
            (
                "tsconfig.base.json",
                r#"{ "extends": "./configs/root", // the shared one
                     "compilerOptions": { "paths": { "@a/x": ["libs/x/src/index.ts"], } } }"#,
            ),
            (
                "configs/root.json",
                r#"{ "compilerOptions": { "baseUrl": "..", "paths": { "@a/y": ["y"] } } }"#,
            ),
            (
                "tsconfig.json",
                r#"{ "compilerOptions": { "baseUrl": "src" } }"#,
            ),
        ]);
        let c = load(&read).config.unwrap();
        assert_eq!(c.base_url.as_deref(), Some(""));
        assert_eq!(c.paths.len(), 1, "own paths win whole: {c:?}");
        assert_eq!(c.paths[0].0, "@a/x");
    }

    #[test]
    fn a_broken_file_is_reported_and_none_is_none() {
        let read = reader(&[("tsconfig.json", "{ nope")]);
        let l = load(&read);
        assert!(l.config.is_none());
        assert_eq!(l.unreadable.as_deref(), Some("tsconfig.json"));
        let l = load(&reader(&[]));
        assert!(l.config.is_none() && l.unreadable.is_none());
        assert_eq!(fingerprint(None), "none");
    }
}
