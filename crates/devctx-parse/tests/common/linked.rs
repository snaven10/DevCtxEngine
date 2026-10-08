//! A multi-file fixture parsed and linked as the indexer does, without a
//! store (PLAN-009 TASK-007): every source file under a fixture directory,
//! its manifests read into the link environment.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use devctx_parse::resolve::env::{CargoManifest, GoModule, LinkEnv, ManifestKind, PyManifest};
use devctx_parse::resolve::link::{link_rows, LinkEdge, LinkSymbol, Outcome, RepoIndex, Resolved};
use devctx_parse::{detect_lang, parse};

pub struct Linked {
    pub index: RepoIndex,
    pub edges: Vec<LinkEdge>,
    /// id → (qualified name, file).
    pub names: HashMap<u64, (String, String)>,
    pub symbols: Vec<LinkSymbol>,
}

/// `(destination qualified, its file, confidence, resolution, external)`.
pub type Shown = (
    Option<String>,
    Option<String>,
    &'static str,
    &'static str,
    bool,
);

pub fn s(x: &str) -> Option<String> {
    Some(x.to_string())
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

/// Parse and link every file under `tests/fixtures/<dir>`, reading its
/// `Cargo.toml`, `go.mod` and Python manifests into the environment; `edit`
/// may change the environment before the index is built.
pub fn link_dir_with(dir: &str, edit: impl FnOnce(&mut LinkEnv)) -> Linked {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(dir);
    let mut files = Vec::new();
    walk(&root, &mut files);
    files.sort();
    let (mut symbols, mut edges) = (Vec::new(), Vec::new());
    let mut env = LinkEnv::default();
    for f in files {
        let rel = f
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let text = std::fs::read_to_string(&f).unwrap();
        match ManifestKind::of(&rel) {
            Some(ManifestKind::Cargo) => env.cargo.extend(CargoManifest::parse(&text, &rel)),
            Some(ManifestKind::GoMod) => env.go.extend(GoModule::parse(&text, &rel)),
            Some(ManifestKind::PyProject | ManifestKind::SetupCfg | ManifestKind::Requirements) => {
                env.python.extend(PyManifest::parse(&text, &rel))
            }
            _ => {}
        }
        let Some(lang) = detect_lang(&f) else {
            continue;
        };
        let mut pf = parse(lang, &text).unwrap();
        pf.assign_ids("repo", &rel);
        let (s, e) = link_rows(&rel, &pf);
        symbols.extend(s);
        edges.extend(e);
    }
    env.sort();
    edit(&mut env);
    let names = symbols
        .iter()
        .map(|s| (s.id, (s.qualified.clone(), s.file.clone())))
        .collect();
    Linked {
        index: RepoIndex::with_env(symbols.clone(), &edges, env),
        edges,
        names,
        symbols,
    }
}

pub fn link_dir(dir: &str) -> Linked {
    link_dir_with(dir, |_| {})
}

impl Linked {
    pub fn src_name(&self, e: &LinkEdge) -> Option<&str> {
        self.names.get(&e.src_id).map(|(q, _)| q.as_str())
    }

    /// The outcomes of the edges of `kind` from `src` (qualified) to
    /// `dst_name`, in line order.
    pub fn all(&self, kind: &str, src: &str, dst: &str) -> Vec<Shown> {
        let found: Vec<Shown> = self
            .edges
            .iter()
            .filter(|e| e.kind == kind && e.dst_name == dst)
            .filter(|e| self.src_name(e) == Some(src))
            .map(|e| self.show(&self.index.resolve(e).expect("resolved")))
            .collect();
        assert!(
            !found.is_empty(),
            "no {kind} {src} -> {dst} in {:#?}",
            self.edges
                .iter()
                .filter(|e| self.src_name(e) == Some(src))
                .map(|e| format!(
                    "{} {} [{}]",
                    e.kind,
                    e.dst_name,
                    e.hint.as_deref().unwrap_or("-")
                ))
                .collect::<Vec<_>>()
        );
        found
    }

    /// The first call from `src` to `dst_name`.
    pub fn call(&self, src: &str, dst: &str) -> Shown {
        self.all("calls", src, dst).remove(0)
    }

    pub fn show(&self, o: &Outcome) -> Shown {
        match o {
            Outcome::Resolved(Resolved {
                dst_id,
                confidence,
                resolution,
                external,
            }) => {
                let d = dst_id.map(|d| self.names[&d].clone());
                (
                    d.as_ref().map(|(q, _)| q.clone()),
                    d.map(|(_, f)| f),
                    confidence,
                    resolution,
                    *external,
                )
            }
            Outcome::Discard => (None, None, "-", "discard", false),
        }
    }

    /// The import edge of `file` whose destination is `target`.
    pub fn import(&self, file: &str, target: &str) -> Shown {
        let e = self
            .edges
            .iter()
            .find(|e| e.kind == "imports" && e.file == file && e.dst_name == target)
            .unwrap_or_else(|| {
                panic!(
                    "no import {target} in {file}: {:?}",
                    self.edges
                        .iter()
                        .filter(|e| e.kind == "imports" && e.file == file)
                        .map(|e| &e.dst_name)
                        .collect::<Vec<_>>()
                )
            });
        self.show(&self.index.resolve(e).unwrap())
    }

    /// The edges of `kind` from `src` to `dst` (for counting occurrences).
    pub fn count(&self, kind: &str, src: &str, dst: &str) -> usize {
        self.edges
            .iter()
            .filter(|e| e.kind == kind && e.dst_name == dst && self.src_name(e) == Some(src))
            .count()
    }
}

/// An undecided outcome (no destination, no external mark, `low`).
pub const UNDECIDED: Shown = (None, None, "low", "name_only", false);
/// A discarded call.
pub const DISCARDED: Shown = (None, None, "-", "discard", false);
/// An external call with direct evidence.
pub const EXTERNAL: Shown = (None, None, "high", "external_known", true);
