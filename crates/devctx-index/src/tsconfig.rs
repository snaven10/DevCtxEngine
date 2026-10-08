//! What the link pass reads of a TypeScript/JavaScript workspace besides its
//! sources (PLAN-009 TASK-006, DD-7, DD-9): the root `tsconfig` (`paths`
//! aliases of an Nx workspace, `baseUrl`) and every `package.json` (what a
//! package is: a specifier is external only if a manifest declares it).
//!
//! Best effort: `tsconfig.base.json` at the repository root, else (missing or
//! unreadable) `tsconfig.json`; JSON with comments, trailing commas and a
//! byte-order mark; one level of relative `extends` (a string or an array,
//! later entries over earlier ones, the file over all). A file that cannot be
//! read is reported, and the caller keeps the last environment that could
//! (a merge conflict in `tsconfig.base.json` must not relink the branch
//! without its aliases).

use devctx_parse::resolve::typescript::{dir_of, join, Manifest, ScriptEnv, TsConfig};

/// The candidates, in the order they are tried.
const NAMES: &[&str] = &["tsconfig.base.json", "tsconfig.json"];

/// What was found.
#[derive(Debug, Default)]
pub(crate) struct Loaded {
    /// The environment as read (a broken file left out).
    pub env: ScriptEnv,
    /// Files that exist but could not be read (a `tsconfig`, a
    /// `package.json`): the environment is not trustworthy.
    pub unreadable: Vec<String>,
    /// What was read but not followed (an `extends` of a package, a base
    /// that does not parse): said once in the run's summary.
    pub notes: Vec<String>,
}

/// Load the environment through `read` (a repository path → its text, or
/// `None` when it does not exist on the branch being indexed); `files`: the
/// branch's paths, where its `package.json` files are found.
pub(crate) fn load(read: &dyn Fn(&str) -> Option<String>, files: &[String]) -> Loaded {
    let mut out = Loaded::default();
    for name in NAMES {
        let Some(text) = read(name) else { continue };
        let Some(own) = TsConfig::parse(&text, dir_of(name)) else {
            out.unreadable.push((*name).to_string());
            continue;
        };
        let (bases, packages) = TsConfig::extends_of(&text);
        for p in packages {
            out.notes
                .push(format!("{name}: `extends` of a package ({p}) not followed"));
        }
        let mut config = own;
        // Later entries of an `extends` array override earlier ones; the
        // file overrides them all.
        for e in bases.iter().rev() {
            let path = join(dir_of(name), e);
            let path = if path.ends_with(".json") {
                path
            } else {
                format!("{path}.json")
            };
            match read(&path).map(|t| TsConfig::parse(&t, dir_of(&path))) {
                Some(Some(base)) => config = config.over(base),
                Some(None) => out
                    .notes
                    .push(format!("{name}: its base {path} does not parse")),
                None => out
                    .notes
                    .push(format!("{name}: its base {path} does not exist")),
            }
        }
        out.env.tsconfig = Some(config);
        break;
    }
    for f in files {
        let is_manifest = f == "package.json" || f.ends_with("/package.json");
        if !is_manifest || f.split('/').any(|seg| seg == "node_modules") {
            continue;
        }
        let Some(text) = read(f) else { continue };
        match Manifest::parse(&text, dir_of(f)) {
            Some(m) => out.env.manifests.push(m),
            None => out.unreadable.push(f.clone()),
        }
    }
    out.env.manifests.sort_by(|a, b| a.dir.cmp(&b.dir));
    out
}

/// The environment the link pass runs under (stored as the last good one:
/// it is made only of parts that could be read) and its fingerprint. Per
/// component (TASK-006 second review):
/// what could be read is taken fresh; only an unreadable part comes from the
/// last good environment — its `tsconfig`, or the manifest of the same
/// directory — and an unreadable manifest that environment did not have is
/// left out. So a broken file relinks nothing by itself and loses no alias,
/// and never freezes the changes made to the rest.
pub(crate) fn choose(loaded: Loaded, stored_env: Option<&str>) -> (ScriptEnv, String) {
    let stored = stored_env.and_then(ScriptEnv::from_json);
    let mut env = loaded.env;
    if let Some(stored) = stored {
        let tsconfig_broken = loaded
            .unreadable
            .iter()
            .any(|f| NAMES.contains(&f.as_str()));
        if tsconfig_broken {
            env.tsconfig = stored.tsconfig.clone();
        }
        for f in loaded
            .unreadable
            .iter()
            .filter(|f| !NAMES.contains(&f.as_str()))
        {
            let dir = dir_of(f);
            if let Some(m) = stored.manifests.iter().find(|m| m.dir == dir) {
                env.manifests.push(m.clone());
            }
        }
        env.manifests.sort_by(|a, b| a.dir.cmp(&b.dir));
    }
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
        let l = load(&read, &[]);
        let c = l.env.tsconfig.unwrap();
        assert_eq!(c.base_url.as_deref(), Some(""));
        assert_eq!(c.paths.len(), 1, "own paths win whole: {c:?}");
        assert_eq!(c.paths[0].0, "@a/x");
        assert!(l.unreadable.is_empty() && l.notes.is_empty());
    }

    /// TASK-006 review, M4: a broken base falls back to `tsconfig.json` and
    /// is reported; so is a broken `package.json`.
    #[test]
    fn a_broken_file_is_reported_and_the_next_one_read() {
        let read = reader(&[
            ("tsconfig.base.json", "<<<<<<< HEAD\n{ nope"),
            (
                "tsconfig.json",
                r#"{ "compilerOptions": { "baseUrl": "src" } }"#,
            ),
            ("package.json", r#"{ "dependencies": { "rxjs": "7" } }"#),
            ("libs/x/package.json", "{ broken"),
            ("node_modules/y/package.json", "{ ignored"),
        ]);
        let files = [
            "package.json".to_string(),
            "libs/x/package.json".to_string(),
            "node_modules/y/package.json".to_string(),
        ];
        let l = load(&read, &files);
        assert_eq!(
            l.env.tsconfig.and_then(|c| c.base_url).as_deref(),
            Some("src")
        );
        assert_eq!(l.unreadable, ["tsconfig.base.json", "libs/x/package.json"]);
        assert_eq!(l.env.manifests.len(), 1);
        assert_eq!(l.env.manifests[0].deps, ["rxjs"]);
        let none = load(&reader(&[]), &[]);
        assert!(none.env.tsconfig.is_none() && none.unreadable.is_empty());
    }

    /// TASK-006 review, M4: an unreadable file keeps the last good
    /// environment and its fingerprint; a readable one replaces it.
    #[test]
    fn an_unreadable_file_keeps_the_last_good_environment() {
        let good = load(
            &reader(&[(
                "tsconfig.base.json",
                r#"{ "compilerOptions": { "paths": { "@a/x": ["x"] } } }"#,
            )]),
            &[],
        );
        let (env, fp) = choose(good, None);
        let json = env.to_json();
        let broken = load(&reader(&[("tsconfig.base.json", "<<<<<<< ours")]), &[]);
        let (kept, kept_fp) = choose(broken, Some(&json));
        assert_eq!((kept, kept_fp.as_str()), (env.clone(), fp.as_str()));
        // Nothing stored yet: what could be read.
        let broken = load(&reader(&[("tsconfig.base.json", "<<<<<<< ours")]), &[]);
        let (none, _) = choose(broken, None);
        assert!(none.tsconfig.is_none());
    }

    /// TASK-006 second review, mi1: a broken manifest added after a good
    /// index (an Nx generator's template) costs only itself: a change of
    /// `paths` in the same run is seen, and the other manifests are fresh.
    #[test]
    fn a_broken_manifest_added_later_freezes_nothing_else() {
        let cfg = |target: &str| {
            format!(r#"{{ "compilerOptions": {{ "paths": {{ "@a/x": ["{target}"] }} }} }}"#)
        };
        let good = load(
            &reader(&[
                ("tsconfig.base.json", &cfg("libs/a")),
                ("package.json", r#"{ "dependencies": { "rxjs": "7" } }"#),
            ]),
            &["package.json".to_string()],
        );
        let (env, fp) = choose(good, None);
        let json = env.to_json();
        let files = [
            "package.json".to_string(),
            "tools/generators/x/files/package.json".to_string(),
        ];
        let later = load(
            &reader(&[
                ("tsconfig.base.json", &cfg("libs/b")),
                (
                    "package.json",
                    r#"{ "dependencies": { "rxjs": "7", "lodash": "4" } }"#,
                ),
                (
                    "tools/generators/x/files/package.json",
                    r#"{ "name": "<%= name %>" "#,
                ),
            ]),
            &files,
        );
        let (now, now_fp) = choose(later, Some(&json));
        assert_eq!(
            now.tsconfig.unwrap().paths[0].1,
            ["libs/b".to_string()],
            "the new alias is seen"
        );
        assert_eq!(now.manifests.len(), 1, "{:?}", now.manifests);
        assert_eq!(now.manifests[0].deps, ["lodash", "rxjs"]);
        assert_ne!(now_fp, fp);
    }

    /// An unreadable `tsconfig` keeps only the stored `tsconfig`: a
    /// dependency added in the same run is seen.
    #[test]
    fn a_broken_tsconfig_keeps_only_the_stored_tsconfig() {
        let good = load(
            &reader(&[
                (
                    "tsconfig.base.json",
                    r#"{ "compilerOptions": { "paths": { "@a/x": ["libs/a"] } } }"#,
                ),
                ("package.json", r#"{ "dependencies": { "rxjs": "7" } }"#),
            ]),
            &["package.json".to_string()],
        );
        let (env, _) = choose(good, None);
        let json = env.to_json();
        let later = load(
            &reader(&[
                ("tsconfig.base.json", "<<<<<<< ours"),
                (
                    "package.json",
                    r#"{ "dependencies": { "rxjs": "7", "lodash": "4" } }"#,
                ),
            ]),
            &["package.json".to_string()],
        );
        let (now, _) = choose(later, Some(&json));
        assert_eq!(now.tsconfig, env.tsconfig);
        assert_eq!(now.manifests[0].deps, ["lodash", "rxjs"]);
    }

    /// An `extends` array (TypeScript 5): later entries over earlier ones;
    /// a package entry and a missing base are noted.
    #[test]
    fn an_extends_array_is_followed_and_what_is_not_is_noted() {
        let read = reader(&[
            (
                "tsconfig.base.json",
                r#"{ "extends": ["./a.json", "./b.json", "@tsconfig/node20", "./gone.json"] }"#,
            ),
            ("a.json", r#"{ "compilerOptions": { "baseUrl": "a" } }"#),
            ("b.json", r#"{ "compilerOptions": { "baseUrl": "b" } }"#),
        ]);
        let l = load(&read, &[]);
        assert_eq!(
            l.env.tsconfig.and_then(|c| c.base_url).as_deref(),
            Some("b")
        );
        assert_eq!(l.notes.len(), 2, "{:?}", l.notes);
    }
}
