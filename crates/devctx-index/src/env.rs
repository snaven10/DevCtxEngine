//! What the link pass reads of a workspace besides its sources (PLAN-009
//! TASK-006 and TASK-007, DD-6, DD-9): the TypeScript/JavaScript environment
//! (`tsconfig`, `package.json`, see [`crate::tsconfig`]) and every
//! `Cargo.toml`, `go.mod` and Python manifest (`pyproject.toml`,
//! `setup.cfg`, `requirements*.txt`) of the branch.
//!
//! A file that cannot be read is reported, and only that file comes from the
//! last environment that could be read (the same path), so a broken manifest
//! relinks nothing by itself and never freezes the changes made to the rest.

use devctx_parse::resolve::env::{CargoManifest, GoModule, LinkEnv, ManifestKind, PyManifest};

/// Directories whose manifests describe no module of the repository: build
/// output, vendored code, virtual environments.
const NOT_WORKSPACE: &[&str] = &[
    "node_modules",
    "dist",
    "vendor",
    "third_party",
    "bower_components",
    "target",
    ".venv",
    "venv",
    "site-packages",
    ".tox",
];

/// Whether the branch path `path` is a manifest the link pass reads.
pub(crate) fn is_manifest(path: &str) -> bool {
    ManifestKind::of(path).is_some()
        && !path
            .split('/')
            .rev()
            .skip(1)
            .any(|seg| NOT_WORKSPACE.contains(&seg))
}

/// What was found.
#[derive(Debug, Default)]
pub(crate) struct Loaded {
    /// The TypeScript/JavaScript part, with its own unreadable files.
    pub script: crate::tsconfig::Loaded,
    /// The `Cargo.toml`, `go.mod` and Python manifests that could be read.
    pub env: LinkEnv,
    /// Of those, the files that exist but could not be read.
    pub unreadable: Vec<String>,
}

impl Loaded {
    /// Every file that could not be read, TypeScript's included.
    pub fn unreadable(&self) -> impl Iterator<Item = &String> {
        self.script.unreadable.iter().chain(&self.unreadable)
    }

    /// The environment as read (broken files left out).
    #[cfg(test)]
    pub fn env(&self) -> LinkEnv {
        let mut env = self.env.clone();
        env.script = self.script.env.clone();
        env.sort();
        env
    }
}

/// Load the environment through `read` (a repository path → its text, or
/// `None` when it does not exist on the branch being indexed); `files`: the
/// branch's paths, where its manifests are found.
pub(crate) fn load(read: &dyn Fn(&str) -> Option<String>, files: &[String]) -> Loaded {
    let manifests: Vec<String> = files.iter().filter(|f| is_manifest(f)).cloned().collect();
    let mut out = Loaded {
        script: crate::tsconfig::load(read, &manifests),
        ..Default::default()
    };
    for f in &manifests {
        let Some(kind) = ManifestKind::of(f) else {
            continue;
        };
        if kind == ManifestKind::Npm {
            continue; // the TypeScript part's
        }
        let Some(text) = read(f) else { continue };
        let ok = match kind {
            ManifestKind::Cargo => CargoManifest::parse(&text, f).map(|m| out.env.cargo.push(m)),
            ManifestKind::GoMod => GoModule::parse(&text, f).map(|m| out.env.go.push(m)),
            _ => PyManifest::parse(&text, f).map(|m| out.env.python.push(m)),
        };
        if ok.is_none() {
            out.unreadable.push(f.clone());
        }
    }
    out.env.sort();
    out
}

/// The environment the link pass runs under (stored as the last good one:
/// it is made only of parts that could be read) and its fingerprint. Per
/// file: what could be read is taken fresh; an unreadable file comes from
/// the last good environment (the same path), and one that environment did
/// not have is left out.
pub(crate) fn choose(loaded: Loaded, stored_env: Option<&str>) -> (LinkEnv, String) {
    let stored = stored_env.and_then(LinkEnv::from_json);
    let mut env = loaded.env.clone();
    let stored_script = stored.as_ref().map(|s| s.script.to_json());
    let (script, _) = crate::tsconfig::choose(loaded.script, stored_script.as_deref());
    env.script = script;
    if let Some(stored) = stored {
        for f in &loaded.unreadable {
            if let Some(m) = stored.cargo.iter().find(|m| &m.path == f) {
                env.cargo.push(m.clone());
            }
            if let Some(m) = stored.go.iter().find(|m| &m.path == f) {
                env.go.push(m.clone());
            }
            if let Some(m) = stored.python.iter().find(|m| &m.path == f) {
                env.python.push(m.clone());
            }
        }
    }
    env.sort();
    let fp = env.fingerprint();
    (env, fp)
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

    fn paths(files: &[(&str, &str)]) -> Vec<String> {
        files.iter().map(|(p, _)| p.to_string()).collect()
    }

    #[test]
    fn manifests_are_found_by_name_outside_build_and_vendored_code() {
        assert!(is_manifest("Cargo.toml"));
        assert!(is_manifest("crates/a/Cargo.toml"));
        assert!(is_manifest("svc/go.mod"));
        assert!(is_manifest("requirements-dev.txt"));
        assert!(!is_manifest("vendor/x/Cargo.toml"));
        assert!(!is_manifest("target/package/x/Cargo.toml"));
        assert!(!is_manifest(".venv/lib/site-packages/y/pyproject.toml"));
        assert!(!is_manifest("src/main.rs"));
    }

    #[test]
    fn every_kind_is_read_and_a_broken_one_reported() {
        let files = [
            ("Cargo.toml", "[workspace]\nmembers = [\"a\"]\n"),
            ("a/Cargo.toml", "[package]\nname = \"a\"\n"),
            ("b/Cargo.toml", "[package\n"),
            ("go.mod", "module example.com/m\n"),
            ("requirements.txt", "requests\n"),
            ("package.json", "{ \"dependencies\": { \"rxjs\": \"7\" } }"),
        ];
        let l = load(&reader(&files), &paths(&files));
        let env = l.env();
        assert_eq!(env.cargo.len(), 2);
        assert_eq!(env.go[0].module.as_deref(), Some("example.com/m"));
        assert_eq!(env.python[0].deps, ["requests"]);
        assert_eq!(env.script.manifests[0].deps, ["rxjs"]);
        assert_eq!(l.unreadable().collect::<Vec<_>>(), ["b/Cargo.toml"]);
    }

    /// A broken `Cargo.toml` keeps its last good version, and only it: a
    /// crate added in the same run is seen.
    #[test]
    fn a_broken_manifest_keeps_only_its_own_last_good_version() {
        let good = [
            (
                "a/Cargo.toml",
                "[package]\nname = \"a\"\n[dependencies]\nserde = \"1\"\n",
            ),
            ("go.mod", "module example.com/m\n"),
        ];
        let (env, fp) = choose(load(&reader(&good), &paths(&good)), None);
        let json = env.to_json();
        let later = [
            ("a/Cargo.toml", "[package]\n<<<<<<< HEAD\n"),
            ("b/Cargo.toml", "[package]\nname = \"b\"\n"),
            ("go.mod", "module example.com/renamed\n"),
        ];
        let (now, now_fp) = choose(load(&reader(&later), &paths(&later)), Some(&json));
        let crates: Vec<_> = now.cargo.iter().map(|m| m.krate.clone()).collect();
        assert_eq!(crates, [Some("a".to_string()), Some("b".to_string())]);
        assert_eq!(now.cargo[0].deps, ["serde"], "a's last good version");
        assert_eq!(now.go[0].module.as_deref(), Some("example.com/renamed"));
        assert_ne!(now_fp, fp);
        // Nothing stored: the broken file is left out.
        let (none, _) = choose(load(&reader(&later), &paths(&later)), None);
        assert_eq!(none.cargo.len(), 1);
        // The same files again: the same fingerprint.
        let (again, again_fp) = choose(load(&reader(&good), &paths(&good)), Some(&json));
        assert_eq!((again, again_fp), (env, fp));
    }
}
