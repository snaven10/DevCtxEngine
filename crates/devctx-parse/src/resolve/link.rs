//! The link pass's resolution (PLAN-009 DD-6, DD-7, DD-8, DD-9): which
//! symbol of the branch an occurrence reaches, or that it is external, or
//! that nothing decides it.
//!
//! Pure: a [`RepoIndex`] is built from rows (the store's, or a test's
//! parses, [`link_rows`]) and answers one [`LinkEdge`] at a time from its
//! `dst_name` and `hint` — what the file said locally (PLAN-009 TASK-005) —
//! without the source. `devctx-index` loads the rows, picks which edges to
//! re-resolve and writes the answers back.
//!
//! `contains` is never resolved here ([`RepoIndex::resolve`] answers `None`):
//! it is intra-file and written resolved at parse time (DD-6).

use std::collections::{HashMap, HashSet, VecDeque};

use devctx_core::symbol_id::{kind_class, FILE_KIND};

use crate::facts::{CONTAINS, IMPLEMENTS, IMPORTS, INHERITS};
use crate::lang::Lang;
use crate::resolve::java::{arity, declared_type};
use crate::resolve::resolver_for;
use crate::resolve::typescript::{TsConfig, TsImport};
use crate::types::ParsedFile;

/// A symbol of the branch, as the link pass needs it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LinkSymbol {
    /// Stable id (DD-3).
    pub id: u64,
    /// The container's id; `None` for a file symbol.
    pub parent_id: Option<u64>,
    /// Repo-relative path.
    pub file: String,
    /// `file`/`class`/`method`/…
    pub kind: String,
    /// Bare name.
    pub name: String,
    /// `Outer.Inner.method`; the path for the file symbol.
    pub qualified: String,
    /// Java/Go package, module path elsewhere.
    pub package: Option<String>,
    /// The head of the definition (a return type, a field's type, an arity).
    pub signature: Option<String>,
    /// Visible outside its module (`export`, `public`…); `None` when the
    /// language says nothing.
    pub exported: Option<bool>,
    /// 1-based first and last line (a Rust `use` inside an inline `mod`
    /// belongs to it).
    pub start_line: i32,
    pub end_line: i32,
}

/// One occurrence to resolve.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LinkEdge {
    /// `calls`/`instantiates`/`imports`/…
    pub kind: String,
    /// The symbol the occurrence is in.
    pub src_id: u64,
    /// The destination as the file wrote it (DD-2).
    pub dst_name: String,
    /// The receiver as the file saw it (`edges.hint`); `None` but for calls.
    pub hint: Option<String>,
    /// File of the occurrence.
    pub file: String,
    /// 1-based line.
    pub line: i32,
}

/// What the link pass writes on an edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// The destination symbol, when the repository defines it.
    pub dst_id: Option<u64>,
    /// `high`/`medium`/`low` (DD-8).
    pub confidence: &'static str,
    /// Why (DD-8): `self`, `local`, `param`, `field`, `ctor_inject`,
    /// `import`, `same_file`, `same_package`, `inherited`, `return_type`,
    /// `unique_name`, `external_known`, `name_only`.
    pub resolution: &'static str,
    /// Defined outside the repository, with evidence (DD-9).
    pub external: bool,
}

/// The answer for one edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Write this on the edge.
    Resolved(Resolved),
    /// A call on a receiver nothing types (a fluent chain, an untyped
    /// local) to a name the repository does not define: dropped and counted
    /// (DD-7), not kept as an undecided name.
    Discard,
}

pub(super) fn hi(dst: usize, idx: &RepoIndex, resolution: &'static str) -> Outcome {
    let confidence = if resolution == "unique_name" {
        "medium"
    } else {
        "high"
    };
    Outcome::Resolved(Resolved {
        dst_id: Some(idx.syms[dst].id),
        confidence,
        resolution,
        external: false,
    })
}

pub(super) fn external(resolution: &'static str) -> Outcome {
    Outcome::Resolved(Resolved {
        dst_id: None,
        confidence: "high",
        resolution,
        external: true,
    })
}

/// External, but only as far as the receiver goes (an untyped one's
/// `toString`, a repository name after an external call): `medium`.
pub(super) fn external_medium(resolution: &'static str) -> Outcome {
    Outcome::Resolved(Resolved {
        dst_id: None,
        confidence: "medium",
        resolution,
        external: true,
    })
}

/// A destination found by convention, not by a definition (a Lombok
/// accessor of a field): `medium`.
pub(super) fn medium(dst: usize, idx: &RepoIndex, resolution: &'static str) -> Outcome {
    Outcome::Resolved(Resolved {
        dst_id: Some(idx.syms[dst].id),
        confidence: "medium",
        resolution,
        external: false,
    })
}

pub(super) fn undecided() -> Outcome {
    Outcome::Resolved(Resolved {
        dst_id: None,
        confidence: "low",
        resolution: "name_only",
        external: false,
    })
}

/// How a type name resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TypeRef {
    /// A type of the repository, and how it was found.
    Repo(usize, &'static str),
    /// From outside the repository, with evidence.
    External,
    /// Nothing says.
    Unknown,
}

/// What a call returns, as a receiver's type (`x = make()`, TASK-007).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ValueType {
    /// An instance of a repository type: how it was found and how surely.
    Repo(usize, &'static str, &'static str),
    /// A type from outside the repository, declared (a return annotation,
    /// a builtin type called), and how surely.
    External(&'static str),
    /// The value of a call from outside the repository: nothing types it.
    FromExternalCall,
    /// Nothing says.
    Unknown,
}

/// A member lookup through the supertypes.
enum Member {
    /// Found, in the type itself (`false`) or a supertype (`true`); and
    /// whether an overload takes the call's arguments (`false`: the first
    /// one, no surer than `medium`).
    Found(usize, bool, bool),
    /// Not in the repository; some supertype is external (Panache,
    /// `Object`, an enum's `Enum`).
    External,
    /// Not found, and nothing says it is external.
    Missing,
}

#[derive(Default)]
pub(super) struct FileInfo {
    pub(super) lang: Option<Lang>,
    pub(super) package: Option<String>,
    pub(super) imports: Vec<String>,
    /// TypeScript/JavaScript: each import and re-export, read back.
    pub(super) script: Vec<TsImport>,
    /// Python: each import, read back.
    pub(super) py: Vec<crate::resolve::python::PyImport>,
    /// Go: each import path and its alias.
    pub(super) go: Vec<(String, Option<String>)>,
    /// The file symbol's id.
    pub(super) id: Option<u64>,
}

/// What the link pass knows of a branch: its symbols, by id, qualified name,
/// bare name and container; each file's package and imports; each type's
/// supertypes, resolved.
pub struct RepoIndex {
    pub(super) syms: Vec<LinkSymbol>,
    pub(super) by_id: HashMap<u64, usize>,
    pub(super) by_qualified: HashMap<String, Vec<usize>>,
    pub(super) by_name: HashMap<String, Vec<usize>>,
    children: HashMap<u64, Vec<usize>>,
    pub(super) files: HashMap<String, FileInfo>,
    packages: HashSet<String>,
    /// Java: `package.Qualified` of every type.
    fq_types: HashMap<String, Vec<usize>>,
    supers: HashMap<u64, Vec<TypeRef>>,
    /// Types sharing their fully qualified name with another (two modules
    /// of one repository): never a `high` destination.
    ambiguous: HashSet<usize>,
    /// The workspace's `tsconfig` (TypeScript `baseUrl`/`paths`).
    pub(super) ts: Option<TsConfig>,
    /// The workspace's `package.json` files: what a package is (DD-9).
    pub(super) manifests: Vec<crate::resolve::typescript::Manifest>,
    pub(super) ts_cache: crate::resolve::typescript::TsCache,
    /// The workspace's Python manifests (PLAN-009 TASK-007).
    pub(super) python: Vec<crate::resolve::env::PyManifest>,
    pub(super) py_cache: crate::resolve::python::PyCache,
    /// Every directory that holds a file of the branch (a namespace
    /// package, a Go package).
    pub(super) dirs: HashSet<String>,
    /// The workspace's `Cargo.toml` files (PLAN-009 TASK-007).
    pub(super) cargo: Vec<crate::resolve::env::CargoManifest>,
    /// The workspace's `go.mod` files.
    pub(super) go: Vec<crate::resolve::env::GoModule>,
    /// Rust modules and `use`s.
    pub(super) rs: crate::resolve::rust::RsIndex,
    /// The type a symbol belongs to when it is not its container: a Rust
    /// `impl` in a file other than its type's, a Go method beside its
    /// type's file.
    pub(super) owners: HashMap<usize, usize>,
}

/// Methods every Java class has from `Object`, and every enum from `Enum`.
const JAVA_OBJECT: &[&str] = &[
    "toString",
    "equals",
    "hashCode",
    "getClass",
    "clone",
    "finalize",
    "notify",
    "notifyAll",
    "wait",
];
const JAVA_ENUM: &[&str] = &["values", "valueOf", "name", "ordinal", "compareTo"];

pub(super) fn is_type(kind: &str) -> bool {
    kind_class(kind) == "type"
}

pub(super) fn is_callable(kind: &str) -> bool {
    kind_class(kind) == "callable"
}

/// How a TypeScript type found through an import that could not be followed
/// for sure (a default import by its local name, a barrel branch not
/// followed) is labelled: `import`, never surer than `medium`.
pub(super) const IMPORT_WEAK: &str = "import_weak";

/// The last segment of a destination (`a.b.C.m` → `m`, `x::y` → `y`).
pub(super) fn leaf(name: &str) -> &str {
    let after_colons = name.rsplit("::").next().unwrap_or(name);
    after_colons.rsplit('.').next().unwrap_or(after_colons)
}

impl RepoIndex {
    /// Index `symbols`; `facts` brings each file's `imports` and each type's
    /// `inherits`/`implements` (every other edge in it is ignored).
    pub fn new(symbols: Vec<LinkSymbol>, facts: &[LinkEdge]) -> Self {
        Self::with_script_env(symbols, facts, Default::default())
    }

    /// [`RepoIndex::new`] with only a `tsconfig` (no `package.json`: no
    /// package has evidence, DD-9).
    pub fn with_ts_config(
        symbols: Vec<LinkSymbol>,
        facts: &[LinkEdge],
        ts: Option<TsConfig>,
    ) -> Self {
        Self::with_script_env(
            symbols,
            facts,
            crate::resolve::typescript::ScriptEnv {
                tsconfig: ts,
                manifests: Vec::new(),
            },
        )
    }

    /// [`RepoIndex::new`] with a TypeScript/JavaScript workspace's
    /// `tsconfig` and `package.json` files (PLAN-009 TASK-006).
    pub fn with_script_env(
        symbols: Vec<LinkSymbol>,
        facts: &[LinkEdge],
        env: crate::resolve::typescript::ScriptEnv,
    ) -> Self {
        Self::with_env(
            symbols,
            facts,
            crate::resolve::env::LinkEnv {
                script: env,
                ..Default::default()
            },
        )
    }

    /// [`RepoIndex::new`] with the workspace's whole environment: the
    /// TypeScript/JavaScript one, and the `Cargo.toml`, `go.mod` and Python
    /// manifests (PLAN-009 TASK-007).
    pub fn with_env(
        symbols: Vec<LinkSymbol>,
        facts: &[LinkEdge],
        env: crate::resolve::env::LinkEnv,
    ) -> Self {
        let crate::resolve::env::LinkEnv {
            script: env,
            python,
            cargo,
            go,
        } = env;
        let mut idx = Self {
            syms: symbols,
            by_id: HashMap::new(),
            by_qualified: HashMap::new(),
            by_name: HashMap::new(),
            children: HashMap::new(),
            files: HashMap::new(),
            packages: HashSet::new(),
            fq_types: HashMap::new(),
            supers: HashMap::new(),
            ambiguous: HashSet::new(),
            ts: env.tsconfig,
            manifests: env.manifests,
            ts_cache: Default::default(),
            python,
            py_cache: Default::default(),
            dirs: HashSet::new(),
            cargo,
            go,
            rs: Default::default(),
            owners: HashMap::new(),
        };
        for (i, s) in idx.syms.iter().enumerate() {
            idx.by_id.insert(s.id, i);
            let info = idx.files.entry(s.file.clone()).or_default();
            if info.lang.is_none() {
                info.lang = crate::detect_lang(std::path::Path::new(&s.file));
            }
            if info.package.is_none() {
                info.package = s.package.clone();
            }
            if s.kind == FILE_KIND {
                info.id = Some(s.id);
                continue;
            }
            idx.by_qualified
                .entry(s.qualified.clone())
                .or_default()
                .push(i);
            idx.by_name.entry(s.name.clone()).or_default().push(i);
            if let Some(p) = s.parent_id {
                idx.children.entry(p).or_default().push(i);
            }
            if let Some(pkg) = &s.package {
                idx.packages.insert(pkg.clone());
                if is_type(&s.kind) {
                    idx.fq_types
                        .entry(format!("{pkg}.{}", s.qualified))
                        .or_default()
                        .push(i);
                }
            }
        }
        for f in idx.files.keys() {
            let mut d = f.as_str();
            while let Some((parent, _)) = d.rsplit_once('/') {
                if !idx.dirs.insert(parent.to_string()) {
                    break;
                }
                d = parent;
            }
        }
        idx.ambiguous = idx
            .fq_types
            .values()
            .filter(|v| v.len() > 1)
            .flatten()
            .copied()
            .collect();
        for e in facts.iter().filter(|e| e.kind == IMPORTS) {
            if let Some(info) = idx.files.get_mut(&e.file) {
                info.imports.push(e.dst_name.clone());
                if info.lang.is_some_and(is_script_lang) {
                    info.script
                        .push(TsImport::from_row(&e.dst_name, e.hint.as_deref()));
                }
                if info.lang.is_some_and(|l| l.key() == "python") {
                    info.py.push(crate::resolve::python::PyImport::from_row(
                        &e.dst_name,
                        e.hint.as_deref(),
                    ));
                }
                if info.lang.is_some_and(|l| l.key() == "go") {
                    let alias = e
                        .hint
                        .as_deref()
                        .and_then(|h| h.split_whitespace().skip_while(|t| *t != "as").nth(1))
                        .map(str::to_string);
                    info.go.push((e.dst_name.clone(), alias));
                }
            }
        }
        idx.rs_build(facts);
        idx.go_build();
        let mut supers: HashMap<u64, Vec<TypeRef>> = HashMap::new();
        for e in facts
            .iter()
            .filter(|e| e.kind == INHERITS || e.kind == IMPLEMENTS)
        {
            let Some(&src) = idx.by_id.get(&e.src_id) else {
                continue;
            };
            // A Rust `impl Trait for T` gives `T` its supertype (in its
            // file, or `T`'s other file).
            let owner = if idx.syms[src].kind == "impl" {
                idx.syms[src]
                    .parent_id
                    .and_then(|p| idx.by_id.get(&p).copied())
                    .filter(|&p| is_type(&idx.syms[p].kind))
                    .or_else(|| idx.owners.get(&src).copied())
                    .unwrap_or(src)
            } else {
                src
            };
            let r = idx.resolve_type(&e.dst_name, &e.file, Some(src));
            // A TypeScript class gets no implementation from an interface it
            // `implements`: an external one makes no missing member external.
            if r == TypeRef::External && e.kind == IMPLEMENTS && idx.is_script(&e.file) {
                continue;
            }
            supers.entry(idx.syms[owner].id).or_default().push(r);
        }
        idx.supers = supers;
        idx
    }

    /// Resolve one edge; `None` for a kind the link pass never touches
    /// (`contains`).
    pub fn resolve(&self, e: &LinkEdge) -> Option<Outcome> {
        let src = self.by_id.get(&e.src_id).copied();
        Some(match e.kind.as_str() {
            CONTAINS => return None,
            "calls" => self.resolve_call(e, src, 0),
            IMPORTS => self.resolve_import(e),
            _ => self.resolve_type_edge(e, src),
        })
    }

    /// The types below any of `roots` (ids), transitively through the
    /// repository supertypes, `roots` included: what a changed supertype
    /// can change the members of (the incremental pass, DD-6).
    pub fn subtypes_of(&self, roots: &HashSet<u64>) -> HashSet<u64> {
        let mut below: HashMap<u64, Vec<u64>> = HashMap::new();
        for (owner, supers) in &self.supers {
            for r in supers {
                if let TypeRef::Repo(s, _) = r {
                    below.entry(self.syms[*s].id).or_default().push(*owner);
                }
            }
        }
        let mut out: HashSet<u64> = roots
            .iter()
            .copied()
            .filter(|id| {
                self.by_id
                    .get(id)
                    .is_some_and(|&i| is_type(&self.syms[i].kind))
            })
            .collect();
        let mut queue: Vec<u64> = out.iter().copied().collect();
        while let Some(t) = queue.pop() {
            for &sub in below.get(&t).into_iter().flatten() {
                if out.insert(sub) {
                    queue.push(sub);
                }
            }
        }
        out
    }

    /// The files whose imports may resolve differently after `written`
    /// changed: those importing a written file, and through files that
    /// re-export what they import (a TypeScript barrel, any Python module),
    /// theirs, up to four levels (DD-6, the incremental pass, mode d). A
    /// specifier is matched by every path it may name, so a file added or
    /// deleted under it counts.
    pub fn importers_of(&self, written: &HashSet<String>) -> HashSet<String> {
        let mut out: HashSet<String> = HashSet::new();
        if written.is_empty() {
            return out;
        }
        // Every path each file's imports may name, computed once: the
        // rounds below only look them up.
        let reach: Vec<(&String, Vec<String>, bool)> = self
            .files
            .keys()
            .filter_map(|file| {
                let (paths, reexports) = match self.lang_key(file) {
                    Some("typescript" | "tsx" | "javascript") => self.script_reach(file)?,
                    Some("python") => self.py_file_reach(file)?,
                    Some("rust") => self.rs_file_reach(file)?,
                    _ => return None,
                };
                Some((file, paths, reexports))
            })
            .collect();
        let mut frontier: HashSet<String> = written.clone();
        for _ in 0..=crate::resolve::typescript::BARREL_DEPTH {
            let mut next = HashSet::new();
            for (file, paths, reexports) in &reach {
                if out.contains(*file) {
                    continue;
                }
                if paths.iter().any(|p| frontier.contains(p)) {
                    out.insert((*file).clone());
                    if *reexports {
                        next.insert((*file).clone());
                    }
                }
            }
            if next.is_empty() {
                break;
            }
            frontier = next;
        }
        out
    }

    /// The bare name of the symbol `id`.
    pub fn name_of(&self, id: u64) -> Option<&str> {
        self.by_id.get(&id).map(|&i| self.syms[i].name.as_str())
    }

    /// Whether the symbol `src_id` sits in (or is) one of `types`.
    pub fn within(&self, src_id: u64, types: &HashSet<u64>) -> bool {
        let src = self.by_id.get(&src_id).copied();
        self.chain(src)
            .iter()
            .any(|&i| types.contains(&self.syms[i].id))
    }

    pub(super) fn lang_of(&self, file: &str) -> Option<Lang> {
        self.files.get(file).and_then(|f| f.lang)
    }

    /// The registry key of `file`'s language.
    pub(super) fn lang_key(&self, file: &str) -> Option<&'static str> {
        self.lang_of(file).map(|l| l.key())
    }

    /// The symbols whose container is `id`.
    pub(super) fn children_of(&self, id: u64) -> impl Iterator<Item = usize> + '_ {
        self.children.get(&id).into_iter().flatten().copied()
    }

    fn is_java(&self, file: &str) -> bool {
        self.lang_of(file).is_some_and(|l| l.key() == "java")
    }

    /// TypeScript, TSX or JavaScript.
    pub(super) fn is_script(&self, file: &str) -> bool {
        self.lang_of(file).is_some_and(is_script_lang)
    }

    pub(super) fn platform(&self, file: &str, name: &str) -> bool {
        self.lang_of(file)
            .is_some_and(|l| resolver_for(l).is_platform(l.def(), name))
    }

    /// `src` and every symbol around it, innermost first (file symbol last).
    pub(super) fn chain(&self, src: Option<usize>) -> Vec<usize> {
        let mut out = Vec::new();
        let mut cur = src;
        while let Some(i) = cur {
            if out.len() > 64 || out.contains(&i) {
                break;
            }
            out.push(i);
            cur = self.syms[i]
                .parent_id
                .and_then(|p| self.by_id.get(&p).copied());
        }
        out
    }

    /// The types around `src`, innermost first; a symbol with an owner (a
    /// Rust `impl` in another file than its type, a Go method) counts its
    /// owner.
    pub(super) fn containers(&self, src: Option<usize>) -> Vec<usize> {
        let mut out = Vec::new();
        for i in self.chain(src) {
            if is_type(&self.syms[i].kind) {
                if !out.contains(&i) {
                    out.push(i);
                }
            } else if let Some(&t) = self.owners.get(&i) {
                if !out.contains(&t) {
                    out.push(t);
                }
            }
        }
        out
    }

    /// Make `child` one of the symbol `parent`'s children (an owner's).
    pub(super) fn add_child(&mut self, parent: u64, child: usize) {
        let list = self.children.entry(parent).or_default();
        if !list.contains(&child) {
            list.push(child);
        }
    }

    /// Direct members of the type `t` named `name` (a Rust type's also in
    /// its `impl`s).
    fn members(&self, t: usize, name: &str) -> Vec<usize> {
        let mut out = Vec::new();
        let id = self.syms[t].id;
        for &c in self.children.get(&id).into_iter().flatten() {
            if self.syms[c].name == name && self.syms[c].kind != "impl" {
                out.push(c);
            }
            if self.syms[c].kind == "impl" {
                let imp = self.syms[c].id;
                for &m in self.children.get(&imp).into_iter().flatten() {
                    if self.syms[m].name == name {
                        out.push(m);
                    }
                }
            }
        }
        out
    }

    /// `name` in `t` or its supertypes (breadth first).
    fn find_member(&self, t: usize, name: &str, callable: bool, args: Option<usize>) -> Member {
        let mut seen = HashSet::new();
        let mut queue = VecDeque::from([(t, false)]);
        let mut external_super = false;
        // An overload no arity fits: kept while a supertype may have one
        // that does (`persist(Entity)` own, `persist(Iterable)` inherited).
        let mut loose: Option<(usize, bool)> = None;
        while let Some((cur, inherited)) = queue.pop_front() {
            if !seen.insert(cur) || seen.len() > 64 {
                continue;
            }
            let found: Vec<usize> = self
                .members(cur, name)
                .into_iter()
                .filter(|&m| is_callable(&self.syms[m].kind) == callable)
                .collect();
            if !found.is_empty() {
                let (m, exact) = self.pick_overload(&found, args);
                if exact {
                    return Member::Found(m, inherited, true);
                }
                loose.get_or_insert((m, inherited));
            }
            for r in self.supers.get(&self.syms[cur].id).into_iter().flatten() {
                match *r {
                    TypeRef::Repo(s, _) => queue.push_back((s, true)),
                    TypeRef::External => external_super = true,
                    TypeRef::Unknown => {}
                }
            }
        }
        if let Some((m, inherited)) = loose {
            return Member::Found(m, inherited, false);
        }
        let java = self.is_java(&self.syms[t].file);
        let implicit = java
            && callable
            && (JAVA_OBJECT.contains(&name)
                || (self.syms[t].kind == "enum" && JAVA_ENUM.contains(&name)));
        if external_super || implicit {
            Member::External
        } else {
            Member::Missing
        }
    }

    /// What a Lombok-generated method of the Java type `t` stands for: the
    /// field a `getX`/`isX`/`setX` accesses, `t` itself for `builder()`,
    /// when `t` (or a repository supertype) is annotated to generate them
    /// (`@Data`, `@Getter`, `@Setter`, `@Value`, `@Builder`, a record's
    /// accessor). Never `high`: no definition says so.
    fn accessor(&self, t: usize, callee: &str) -> Option<usize> {
        if !self.is_java(&self.syms[t].file) {
            return None;
        }
        let sig = self.syms[t].signature.as_deref().unwrap_or_default();
        let lombok = [
            "@Data",
            "@Getter",
            "@Setter",
            "@Value",
            "@Builder",
            "@SuperBuilder",
        ]
        .iter()
        .any(|a| {
            // `@lombok.Data` is `@Data`.
            sig.split_whitespace()
                .map(|w| w.replacen("@lombok.", "@", 1))
                .any(|w| w.starts_with(a))
        });
        if !lombok {
            return None;
        }
        if callee == "builder" {
            return Some(t);
        }
        let rest = callee
            .strip_prefix("get")
            .or_else(|| callee.strip_prefix("set"))
            .or_else(|| callee.strip_prefix("is"))?;
        let mut chars = rest.chars();
        let first = chars.next()?;
        if !first.is_uppercase() {
            return None;
        }
        let field: String = first.to_lowercase().chain(chars).collect();
        match self.find_member(t, &field, false, None) {
            Member::Found(f, _, _) => Some(f),
            _ => None,
        }
    }

    /// Of several overloads, the first whose arity takes `args`, and
    /// whether one does (else the first, not exact).
    pub(super) fn pick_overload(&self, found: &[usize], args: Option<usize>) -> (usize, bool) {
        // Arity is read from Java signatures only (a Rust method's `&self`
        // is a parameter the call does not pass).
        let fits = |m: usize| -> bool {
            if !self.is_java(&self.syms[m].file) {
                return true;
            }
            let (Some(n), Some(sig)) = (args, self.syms[m].signature.as_deref()) else {
                return true;
            };
            match arity(sig) {
                Some((k, varargs)) => k == n || (varargs && n + 1 >= k),
                None => true,
            }
        };
        match found.iter().copied().find(|&m| fits(m)) {
            Some(m) => (m, true),
            None => (found[0], false),
        }
    }

    /// Callables named `name` anywhere, whose arity takes `args` (Java).
    fn candidates(&self, name: &str, file: &str, args: Option<usize>) -> Vec<usize> {
        let java = self.is_java(file);
        self.by_name
            .get(name)
            .into_iter()
            .flatten()
            .copied()
            .filter(|&i| is_callable(&self.syms[i].kind))
            .filter(|&i| {
                if !java {
                    return true;
                }
                let (Some(n), Some(sig)) = (args, self.syms[i].signature.as_deref()) else {
                    return true;
                };
                arity(sig).is_none_or(|(k, v)| k == n || (v && n + 1 >= k))
            })
            .collect()
    }

    /// Rules 7-9 for a callee nothing else decided.
    pub(super) fn by_name_only(&self, callee: &str, file: &str, args: Option<usize>) -> Outcome {
        let c = self.candidates(callee, file, args);
        match c.len() {
            1 => hi(c[0], self, "unique_name"),
            0 if self.platform(file, callee) => external("external_known"),
            _ => undecided(),
        }
    }

    /// A receiver nothing types: the name must exist in the repository to
    /// be kept, and then nothing decides it.
    pub(super) fn untyped(&self, c: &Call<'_>) -> Outcome {
        let (callee, file, args) = (c.callee, c.file, c.args);
        // Every Java object has `Object`'s methods; a repository type may
        // override them, so no more than medium.
        if self.is_java(file) && JAVA_OBJECT.contains(&callee) {
            return external_medium("inherited");
        }
        if self.candidates(callee, file, args).is_empty() {
            Outcome::Discard
        } else {
            undecided()
        }
    }

    /// A receiver whose type is named but unknown: never discarded, never
    /// more than medium.
    pub(super) fn by_name_only_low(&self, c: &Call<'_>) -> Outcome {
        let k = self.candidates(c.callee, c.file, c.args);
        if k.len() == 1 {
            hi(k[0], self, "unique_name")
        } else {
            undecided()
        }
    }

    /// A dotted receiver outside Java: by name, never discarded.
    fn untyped_or_low(&self, c: &Call<'_>) -> Outcome {
        self.by_name_only_low(c)
    }

    /// How sure a type found as `how` is: `medium` by its name alone
    /// (`unique_name`) or when two types share its fully qualified name
    /// (two modules), else `high`.
    pub(super) fn type_conf(&self, t: usize, how: &str) -> &'static str {
        if how == "unique_name" || how == IMPORT_WEAK || self.ambiguous.contains(&t) {
            "medium"
        } else {
            "high"
        }
    }

    // ------------------------------------------------------------------ types

    /// Resolve a type name as written in `file`, seen from `ctx`.
    pub(super) fn resolve_type(&self, name: &str, file: &str, ctx: Option<usize>) -> TypeRef {
        if self.is_java(file) {
            return self.java_type(name, file, ctx);
        }
        if self.is_script(file) {
            return self.script_type(name, file, ctx);
        }
        match self.lang_key(file) {
            Some("python") => return self.py_type(name, file, ctx),
            Some("rust") => return self.rs_type(name, file, ctx),
            Some("go") => return self.go_type(name, file, ctx),
            _ => {}
        }
        // Generic: this file (nested along the containers, then top level),
        // then a unique type of that name, then the platform.
        if let Some(t) = self.same_file_type(name, file, ctx) {
            return TypeRef::Repo(t, "same_file");
        }
        let simple = leaf(name);
        let types: Vec<usize> = self
            .by_name
            .get(simple)
            .into_iter()
            .flatten()
            .copied()
            .filter(|&i| is_type(&self.syms[i].kind) && self.syms[i].qualified == simple)
            .collect();
        if types.len() == 1 {
            return TypeRef::Repo(types[0], "unique_name");
        }
        if self.platform(file, name) {
            return TypeRef::External;
        }
        TypeRef::Unknown
    }

    /// A type of `file` named `name`: nested in a container around `ctx`
    /// (innermost first), or top level.
    pub(super) fn same_file_type(
        &self,
        name: &str,
        file: &str,
        ctx: Option<usize>,
    ) -> Option<usize> {
        let in_file = |q: &str| {
            self.by_qualified
                .get(q)
                .into_iter()
                .flatten()
                .copied()
                .find(|&i| self.syms[i].file == file && is_type(&self.syms[i].kind))
        };
        for c in self.containers(ctx) {
            if let Some(t) = in_file(&format!("{}.{name}", self.syms[c].qualified)) {
                return Some(t);
            }
        }
        in_file(name)
    }

    fn fq(&self, name: &str) -> Option<usize> {
        self.fq_types.get(name).and_then(|v| v.first().copied())
    }

    /// Java (DD-7): nested and top-level types of the file, single-type
    /// imports, the package, wildcard imports, `java.lang`; a name with a
    /// package path is looked up whole. An import of a package the
    /// repository does not have is evidence of external (DD-9).
    fn java_type(&self, name: &str, file: &str, ctx: Option<usize>) -> TypeRef {
        let info = self.files.get(file);
        let package = info.and_then(|f| f.package.clone()).unwrap_or_default();
        let how_pkg = |t: usize| {
            if self.syms[t].package.as_deref() == Some(package.as_str()) {
                "same_package"
            } else {
                "import"
            }
        };
        if let Some((first, rest)) = name.split_once('.') {
            if let Some(t) = self.fq(name) {
                return TypeRef::Repo(t, how_pkg(t));
            }
            match self.java_type(first, file, ctx) {
                TypeRef::Repo(t, how) => {
                    let q = format!("{}.{rest}", self.syms[t].qualified);
                    let nested = self
                        .by_qualified
                        .get(&q)
                        .into_iter()
                        .flatten()
                        .copied()
                        .find(|&i| {
                            self.syms[i].file == self.syms[t].file && is_type(&self.syms[i].kind)
                        });
                    return nested.map_or(TypeRef::Unknown, |n| TypeRef::Repo(n, how));
                }
                TypeRef::External => return TypeRef::External,
                TypeRef::Unknown => {}
            }
            if self.platform(file, name) {
                return TypeRef::External;
            }
            // A package path is external only with evidence: an import from
            // that package root (the platform was checked above), outside
            // the repository's packages. A dotted name the scope did not
            // bind may be an inherited field's path, never a package by its
            // case alone (TASK-005 review).
            let pkg = name.rsplit_once('.').map_or("", |(p, _)| p);
            let lower = first.chars().next().is_some_and(char::is_lowercase);
            let imported = info.is_some_and(|f| {
                f.imports
                    .iter()
                    .any(|i| i.starts_with(&format!("{first}.")))
            });
            if lower && imported && !self.packages.contains(pkg) && !self.packages.contains(first) {
                return TypeRef::External;
            }
            return TypeRef::Unknown;
        }
        if let Some(t) = self.same_file_type(name, file, ctx) {
            return TypeRef::Repo(t, "same_file");
        }
        let imports = info.map(|f| f.imports.as_slice()).unwrap_or_default();
        for imp in imports.iter().filter(|i| !i.ends_with(".*")) {
            if leaf(imp) != name {
                continue;
            }
            if let Some(t) = self.fq(imp) {
                return TypeRef::Repo(t, "import");
            }
            let pkg = imp.rsplit_once('.').map_or("", |(p, _)| p);
            return if self.packages.contains(pkg) || self.fq(pkg).is_some() {
                TypeRef::Unknown
            } else {
                TypeRef::External
            };
        }
        if let Some(t) = self.fq(&format!("{package}.{name}")) {
            return TypeRef::Repo(t, "same_package");
        }
        let wildcards: Vec<&str> = imports
            .iter()
            .filter_map(|i| i.strip_suffix(".*"))
            .collect();
        for w in &wildcards {
            if let Some(t) = self.fq(&format!("{w}.{name}")) {
                return TypeRef::Repo(t, "import");
            }
        }
        if self.platform(file, name) {
            return TypeRef::External;
        }
        // Inherited member types are not followed; a wildcard over a
        // package the repository does not have is the evidence left — for a
        // name spelt as a type (a lambda parameter `x` is none), and a
        // package's wildcard (`java.util.*`), not a class's static one.
        let typeish = name.chars().next().is_some_and(char::is_uppercase);
        if typeish
            && wildcards.iter().any(|w| {
                leaf(w).chars().next().is_some_and(char::is_lowercase)
                    && !self.packages.contains(*w)
                    && self.fq(w).is_none()
            })
        {
            return TypeRef::External;
        }
        TypeRef::Unknown
    }

    fn resolve_type_edge(&self, e: &LinkEdge, src: Option<usize>) -> Outcome {
        match self.resolve_type(&e.dst_name, &e.file, src) {
            TypeRef::Repo(t, how) => {
                let res = if how == IMPORT_WEAK { "import" } else { how };
                cap(hi(t, self, res), self.type_conf(t, how))
            }
            TypeRef::External => external("external_known"),
            TypeRef::Unknown => {
                let simple = leaf(&e.dst_name);
                let types: Vec<usize> = self
                    .by_name
                    .get(simple)
                    .into_iter()
                    .flatten()
                    .copied()
                    .filter(|&i| is_type(&self.syms[i].kind))
                    .collect();
                if types.len() == 1 {
                    hi(types[0], self, "unique_name")
                } else {
                    undecided()
                }
            }
        }
    }

    fn resolve_import(&self, e: &LinkEdge) -> Outcome {
        if self.is_script(&e.file) {
            return self.script_import(e);
        }
        match self.lang_key(&e.file) {
            Some("python") => return self.py_import(e),
            Some("rust") => return self.rs_import(e),
            Some("go") => return self.go_import(e),
            _ => {}
        }
        let target = e.dst_name.as_str();
        if !self.is_java(&e.file) {
            return if self.platform(&e.file, target) {
                external("external_known")
            } else {
                undecided()
            };
        }
        if let Some(pkg) = target.strip_suffix(".*") {
            if let Some(t) = self.fq(pkg) {
                return hi(t, self, "import");
            }
            if self.packages.contains(pkg) {
                return Outcome::Resolved(Resolved {
                    dst_id: None,
                    confidence: "high",
                    resolution: "import",
                    external: false,
                });
            }
            return external("external_known");
        }
        if let Some(t) = self.fq(target) {
            return cap(hi(t, self, "import"), self.type_conf(t, "import"));
        }
        // `import static a.b.C.m`.
        if let Some((owner, member)) = target.rsplit_once('.') {
            if let Some(t) = self.fq(owner) {
                return match self.find_member(t, member, true, None) {
                    Member::Found(m, _, _) => {
                        cap(hi(m, self, "import"), self.type_conf(t, "import"))
                    }
                    _ => hi(t, self, "import"),
                };
            }
            if self.packages.contains(owner) {
                return undecided();
            }
        }
        external("external_known")
    }

    // ------------------------------------------------------------------ calls

    fn resolve_call(&self, e: &LinkEdge, src: Option<usize>, depth: u8) -> Outcome {
        let callee = leaf(&e.dst_name);
        let hint = e.hint.as_deref().unwrap_or_default();
        let mut tokens: Vec<&str> = hint.split_whitespace().collect();
        let args = match tokens.last() {
            Some(t) if t.starts_with('/') => {
                let n = t[1..].parse().ok();
                tokens.pop();
                n
            }
            _ => None,
        };
        let c = Call {
            callee,
            src,
            file: e.file.as_str(),
            args,
        };
        self.call_by_hint(&c, &tokens, e, depth)
    }

    fn call_by_hint(&self, c: &Call<'_>, tokens: &[&str], e: &LinkEdge, depth: u8) -> Outcome {
        let python = self.lang_key(c.file) == Some("python");
        let rust = self.lang_key(c.file) == Some("rust");
        let go = self.lang_key(c.file) == Some("go");
        match tokens {
            ["name", recv] if go => self.go_name(c, recv),
            ["bare"] | ["free"] if go => self.go_free(c),
            ["path", prefix] if rust => self.rs_path_call(c, prefix),
            ["path"] if rust => undecided(),
            ["name", recv] if rust => self.rs_name(c, recv),
            ["bare"] if rust => self.rs_bare(c),
            ["free"] if rust => self.rs_free(c),
            ["this"] => self.this_call(c, false),
            ["super"] => self.this_call(c, true),
            ["typed", via, ty] => self.typed_call(c, via, ty),
            ["name", recv] if self.is_script(c.file) => self.script_name(c, recv),
            ["name", recv] if python => self.py_name(c, recv),
            ["name", recv] => self.name_call(c, recv),
            ["bare"] if python => self.py_bare(c, true),
            ["free"] if python => self.py_bare(c, false),
            // Python declares no fields: an attribute's attribute is typed
            // by nothing (TASK-007).
            ["member", ..] if python => self.untyped(c),
            ["member", path, base @ ..] => self.member_call(c, path, base),
            ["anon", ty, inner @ ..] => self.anon_call(c, ty, inner),
            ["untyped"] | ["expr"] | ["chain"] => self.untyped(c),
            ["chain", prev, rest @ ..] if depth < 8 => self.chain_call(c, prev, rest, e, depth),
            ["chain", ..] => self.untyped(c),
            ["path"] => self.path_call(c, e),
            ["bare"] => self.any_bare(c),
            ["free"] => self.script_bare(c, false),
            // A row with no hint (written before hints): qualified → as a
            // name, bare → as a bare call.
            _ => match e.dst_name.rsplit_once('.') {
                Some((recv, _)) if self.is_script(c.file) => self.script_name(c, recv),
                Some((recv, _)) => self.name_call(c, recv),
                None => self.any_bare(c),
            },
        }
    }

    /// A member found in a type: `own` if in the type itself, `inherited`
    /// if in a supertype; `medium` when no overload took the arguments or
    /// the type is not sure (`type_conf`).
    pub(super) fn member_hit(
        &self,
        m: usize,
        inherited: bool,
        exact: bool,
        own: &'static str,
    ) -> Outcome {
        let res = if inherited { "inherited" } else { own };
        let out = hi(m, self, res);
        if exact {
            out
        } else {
            cap(out, "medium")
        }
    }

    /// `callee` looked up in the type `t`, resolved as `own`; missing: an
    /// external supertype makes it an inherited external, a Lombok accessor
    /// its field (`medium`), else undecided. Capped at `conf`.
    pub(super) fn in_type(
        &self,
        c: &Call<'_>,
        t: usize,
        own: &'static str,
        conf: &'static str,
    ) -> Outcome {
        let out = match self.find_member(t, c.callee, true, c.args) {
            Member::Found(m, inherited, exact) => self.member_hit(m, inherited, exact, own),
            Member::External => external("inherited"),
            Member::Missing => match self.accessor(t, c.callee) {
                Some(f) => medium(f, self, own),
                None if self.lang_key(&self.syms[t].file) == Some("rust") => {
                    self.rs_missing(c.callee)
                }
                None => undecided(),
            },
        };
        cap(out, conf)
    }

    /// `this.m()` / `super.m()` (rule 1).
    fn this_call(&self, c: &Call<'_>, sup: bool) -> Outcome {
        let Some(&k) = self.containers(c.src).first() else {
            return self.any_bare(c);
        };
        if !sup {
            return self.in_type(c, k, "self", "high");
        }
        let mut external_super = false;
        for r in self.supers.get(&self.syms[k].id).into_iter().flatten() {
            match *r {
                TypeRef::Repo(s, how) => {
                    if let Member::Found(m, _, exact) = self.find_member(s, c.callee, true, c.args)
                    {
                        let out = self.member_hit(m, true, exact, "inherited");
                        return cap(out, self.type_conf(s, how));
                    }
                }
                TypeRef::External => external_super = true,
                TypeRef::Unknown => {}
            }
        }
        if external_super {
            external("inherited")
        } else {
            undecided()
        }
    }

    /// A call without a receiver, by the file's language.
    fn any_bare(&self, c: &Call<'_>) -> Outcome {
        if self.is_script(c.file) {
            self.script_bare(c, true)
        } else {
            self.bare_call(c)
        }
    }

    /// `m()` (rule 6, then 7-9): a member of an enclosing type (or its
    /// supertypes), a function of an enclosing scope of the file, a static
    /// import; then by name.
    fn bare_call(&self, c: &Call<'_>) -> Outcome {
        let (callee, file, args) = (c.callee, c.file, c.args);
        let mut external_super = false;
        for anc in self.chain(c.src) {
            let s = &self.syms[anc];
            if is_type(&s.kind) {
                match self.find_member(anc, callee, true, args) {
                    Member::Found(m, inherited, exact) => {
                        return self.member_hit(m, inherited, exact, "self")
                    }
                    Member::External => external_super = true,
                    Member::Missing => {}
                }
                continue;
            }
            let local: Vec<usize> = self
                .children
                .get(&s.id)
                .into_iter()
                .flatten()
                .copied()
                .filter(|&k| self.syms[k].name == callee && is_callable(&self.syms[k].kind))
                .collect();
            if !local.is_empty() {
                let (m, exact) = self.pick_overload(&local, args);
                return self.member_hit(m, false, exact, "same_file");
            }
        }
        if c.src.is_none() {
            // No source symbol: the file's top level.
            let top: Vec<usize> = self
                .by_qualified
                .get(callee)
                .into_iter()
                .flatten()
                .copied()
                .filter(|&i| self.syms[i].file == file && is_callable(&self.syms[i].kind))
                .collect();
            if !top.is_empty() {
                return hi(top[0], self, "same_file");
            }
        }
        if self.is_java(file) {
            let imports = self
                .files
                .get(file)
                .map(|f| f.imports.as_slice())
                .unwrap_or_default();
            let mut static_external = false;
            for imp in imports {
                let owner = match imp.strip_suffix(".*") {
                    Some(o) => o,
                    None if leaf(imp) == callee => imp.rsplit_once('.').map_or("", |(o, _)| o),
                    None => continue,
                };
                match self.java_type(owner, file, None) {
                    TypeRef::Repo(t, how) => {
                        if let Member::Found(m, _, exact) = self.find_member(t, callee, true, args)
                        {
                            let out = self.member_hit(m, false, exact, "import");
                            return cap(out, self.type_conf(t, how));
                        }
                    }
                    TypeRef::External if !imp.ends_with(".*") => {
                        return external("external_known");
                    }
                    // `import static org.junit…Assertions.*`: a class's
                    // members, from outside the repository.
                    TypeRef::External => {
                        static_external |=
                            leaf(owner).chars().next().is_some_and(char::is_uppercase);
                    }
                    TypeRef::Unknown => {}
                }
            }
            // An unqualified Java call is a member or a static import: not
            // found in the repository, an external supertype or an external
            // static wildcard import has it.
            if external_super {
                return external("inherited");
            }
            if static_external {
                return external("external_known");
            }
        }
        self.by_name_only(callee, file, args)
    }

    /// `x.m()` with `x` typed by the scope (rules 2-4).
    fn typed_call(&self, c: &Call<'_>, via: &str, ty: &str) -> Outcome {
        let resolution: &'static str = match via {
            "field" => "field",
            "param" => "param",
            "ctor_inject" => "ctor_inject",
            _ => "local",
        };
        // `x = make()`: what calling `make` returns (TASK-007).
        if let Some(v) = self.value_of(ty, c.file, c.src) {
            return match v {
                ValueType::Repo(t, _, conf) => self.in_type(c, t, resolution, conf),
                ValueType::External(conf) => cap(external("external_known"), conf),
                ValueType::FromExternalCall => self.after_external(c),
                ValueType::Unknown => self.untyped(c),
            };
        }
        // `var x = T.of(…)`: `T` only if it has the method (DD-7).
        let statik = via == "static";
        match self.resolve_type(ty, c.file, c.src) {
            TypeRef::Repo(t, how) => {
                if statik
                    && !matches!(
                        self.find_member(t, c.callee, true, c.args),
                        Member::Found(..)
                    )
                {
                    return self.untyped(c);
                }
                self.in_type(c, t, resolution, self.type_conf(t, how))
            }
            TypeRef::External if !statik => external("external_known"),
            _ if statik => self.untyped(c),
            _ => self.by_name_only_low(c),
        }
    }

    /// The type a receiver hint names (`typed …`, `this`, `name …`), and
    /// the `resolution` and confidence cap of what it types.
    fn hint_type(
        &self,
        c: &Call<'_>,
        base: &[&str],
    ) -> Option<(TypeRef, &'static str, &'static str)> {
        match base {
            ["typed", via, ty] if *via != "static" => {
                let res: &'static str = match *via {
                    "field" => "field",
                    "param" => "param",
                    "ctor_inject" => "ctor_inject",
                    _ => "local",
                };
                if let Some(v) = self.value_of(ty, c.file, c.src) {
                    return Some(match v {
                        ValueType::Repo(t, how, conf) => (TypeRef::Repo(t, how), res, conf),
                        ValueType::External(conf) => (TypeRef::External, res, conf),
                        ValueType::FromExternalCall | ValueType::Unknown => {
                            (TypeRef::Unknown, res, "medium")
                        }
                    });
                }
                let r = self.resolve_type(ty, c.file, c.src);
                let conf = match r {
                    TypeRef::Repo(t, how) => self.type_conf(t, how),
                    _ => "high",
                };
                Some((r, res, conf))
            }
            ["this"] => self
                .containers(c.src)
                .first()
                .map(|&k| (TypeRef::Repo(k, "self"), "self", "high")),
            ["name", recv] => match self.resolve_type(recv, c.file, c.src) {
                TypeRef::Unknown => self.field_type(c, recv),
                r => Some((r, "import", "high")),
            },
            _ => None,
        }
    }

    /// The type of the field `name` of a type around the call (or of its
    /// supertypes), by its declaration.
    fn field_type(
        &self,
        c: &Call<'_>,
        name: &str,
    ) -> Option<(TypeRef, &'static str, &'static str)> {
        if !self.is_java(c.file) {
            return None;
        }
        for k in self.containers(c.src) {
            if let Member::Found(f, inherited, _) = self.find_member(k, name, false, None) {
                let fs = &self.syms[f];
                let ty = fs
                    .signature
                    .as_deref()
                    .and_then(|s| declared_type(s, name))?;
                let how = if inherited { "inherited" } else { "field" };
                let r = self.resolve_type(&ty, &fs.file, Some(f));
                let conf = match r {
                    TypeRef::Repo(t, h) => self.type_conf(t, h),
                    _ => "high",
                };
                return Some((r, how, conf));
            }
        }
        None
    }

    /// The type of the field `name` of the repository type `t`.
    fn member_type(&self, t: usize, name: &str) -> (TypeRef, &'static str) {
        match self.find_member(t, name, false, None) {
            Member::Found(f, _, _) => {
                let fs = &self.syms[f];
                let declared = match self.lang_key(&fs.file) {
                    Some("rust") => fs
                        .signature
                        .as_deref()
                        .and_then(crate::resolve::rust::field_type)
                        .map(|t| t.base),
                    Some("go") => fs
                        .signature
                        .as_deref()
                        .and_then(crate::resolve::go::field_type),
                    _ => fs.signature.as_deref().and_then(|s| declared_type(s, name)),
                };
                match declared {
                    Some(ty) => match self.resolve_type(&ty, &fs.file, Some(f)) {
                        TypeRef::Repo(n, how) => (TypeRef::Repo(n, how), self.type_conf(n, how)),
                        r => (r, "high"),
                    },
                    None => (TypeRef::Unknown, "high"),
                }
            }
            _ => (TypeRef::Unknown, "high"),
        }
    }

    /// `target.parent.m()`, `this.cfg.server.m()`: the first segment's type,
    /// then each field's declared type (PLAN-009 TASK-005 review).
    fn member_call(&self, c: &Call<'_>, path: &str, base: &[&str]) -> Outcome {
        if !self.is_java(c.file) && !matches!(self.lang_key(c.file), Some("rust" | "go")) {
            // TypeScript/JavaScript: as the dotted name it is (TASK-006).
            return self.untyped_or_low(c);
        }
        // Labelled by its base (`p.f.m()` with `p` a parameter is `param`);
        // Rust and Go `self.f.m()` by the field (rule 3).
        let Some((mut r, label, mut conf)) = self.hint_type(c, base) else {
            return self.untyped(c);
        };
        let label = if base == ["this"] && !self.is_java(c.file) {
            "field"
        } else {
            label
        };
        for seg in path.split('.') {
            match r {
                TypeRef::Repo(t, _) => {
                    let (next, cf) = self.member_type(t, seg);
                    r = next;
                    conf = min_conf(conf, cf);
                }
                // A field of an external type: its type is unknown here.
                _ => return self.untyped(c),
            }
        }
        match r {
            TypeRef::Repo(t, how) => {
                self.in_type(c, t, label, min_conf(conf, self.type_conf(t, how)))
            }
            TypeRef::External => cap(external("external_known"), conf),
            TypeRef::Unknown => self.untyped(c),
        }
    }

    /// A bare or `this` call in an anonymous class extending `ty`: its
    /// supertype first. A name the supertype may have (external) that the
    /// enclosing class also defines is undecided; then the enclosing scopes.
    fn anon_call(&self, c: &Call<'_>, ty: &str, inner: &[&str]) -> Outcome {
        let this = inner == ["this"];
        match self.resolve_type(ty, c.file, c.src) {
            TypeRef::Repo(t, how) => match self.find_member(t, c.callee, true, c.args) {
                Member::Found(m, _, exact) => cap(
                    self.member_hit(m, true, exact, "inherited"),
                    self.type_conf(t, how),
                ),
                Member::External => self.anon_external(c),
                Member::Missing if this => undecided(),
                Member::Missing => self.bare_call(c),
            },
            TypeRef::External => self.anon_external(c),
            TypeRef::Unknown if this => undecided(),
            TypeRef::Unknown => cap(self.bare_call(c), "medium"),
        }
    }

    /// The anonymous class's external supertype may have the name; if the
    /// enclosing scopes define it too, nothing decides which.
    fn anon_external(&self, c: &Call<'_>) -> Outcome {
        // The platform lists name types, not methods: only `Object`'s
        // methods say a name is surely the supertype's too.
        let objectish = self.is_java(c.file) && JAVA_OBJECT.contains(&c.callee);
        match self.bare_call(c) {
            // The supertype may have it too: no surer than `medium`, and
            // undecided for a name every object has.
            out @ Outcome::Resolved(Resolved {
                dst_id: Some(_), ..
            }) if !objectish => cap(out, "medium"),
            Outcome::Resolved(Resolved {
                dst_id: Some(_), ..
            }) => undecided(),
            _ => external("inherited"),
        }
    }

    /// `R.m()` with `R` bound by no scope (rule 5): a type, or an inherited
    /// field; else as untyped.
    fn name_call(&self, c: &Call<'_>, recv: &str) -> Outcome {
        match self.resolve_type(recv, c.file, c.src) {
            TypeRef::Repo(t, how) => return self.in_type(c, t, how, self.type_conf(t, how)),
            TypeRef::External => return external("external_known"),
            TypeRef::Unknown => {}
        }
        if self.is_java(c.file) {
            // `Config.INSTANCE.m()`: a type, then its (static) fields.
            if let Some((first, rest)) = recv.split_once('.') {
                if let TypeRef::Repo(mut t, how) = self.resolve_type(first, c.file, c.src) {
                    let mut conf = self.type_conf(t, how);
                    for seg in rest.split('.') {
                        match self.member_type(t, seg) {
                            (TypeRef::Repo(n, _), cf) => {
                                t = n;
                                conf = min_conf(conf, cf);
                            }
                            (TypeRef::External, _) => return external("external_known"),
                            (TypeRef::Unknown, _) => return self.untyped(c),
                        }
                    }
                    return self.in_type(c, t, how, conf);
                }
            }
            // A field of an enclosing type or its supertypes (`LOG` of a
            // base class): typed by its declaration.
            if let Some((r, how, conf)) = self.field_type(c, recv) {
                return match r {
                    TypeRef::Repo(t, _) => self.in_type(c, t, how, conf),
                    TypeRef::External => external("external_known"),
                    TypeRef::Unknown => self.untyped(c),
                };
            }
            return self.untyped(c);
        }
        let qualified = format!("{recv}.{}", c.callee);
        let exact: Vec<usize> = self
            .by_qualified
            .get(&qualified)
            .into_iter()
            .flatten()
            .copied()
            .collect();
        if exact.len() == 1 {
            return hi(exact[0], self, "unique_name");
        }
        if self.platform(c.file, recv) || self.platform(c.file, &qualified) {
            return external("external_known");
        }
        undecided()
    }

    /// `a.b().c()`: the previous call (itself resolved the same way, back
    /// along the chain) types the receiver — a repository method by its
    /// declared return type, without generics (DD-7, Java), never surer than
    /// that call. After an external call nothing types it: a name the
    /// repository defines, or shaped like an accessor, stays undecided (and
    /// reachable by name, DD-10); any other is `chain_external`, `medium`.
    fn chain_call(
        &self,
        c: &Call<'_>,
        prev: &str,
        prev_hint: &[&str],
        e: &LinkEdge,
        depth: u8,
    ) -> Outcome {
        let rust = self.lang_key(c.file) == Some("rust");
        // Rust `a.f().unwrap().g()`: `g` on what `f` returns, unwrapped, as
        // `a.f()?.g()` (TASK-007).
        if rust && crate::resolve::rust::UNWRAPS.contains(&prev) {
            if let ["chain", p2, rest @ ..] = prev_hint {
                let p2 = format!("{}?", p2.trim_end_matches('?'));
                return self.chain_call(c, &p2, rest, e, depth + 1);
            }
        }
        let (prev, unwrap) = match prev.strip_suffix('?') {
            Some(p) => (p, true),
            None => (prev, false),
        };
        let inner = LinkEdge {
            dst_name: prev.to_string(),
            hint: Some(prev_hint.join(" ")),
            ..e.clone()
        };
        let (p, prev_conf) = match self.resolve_call(&inner, c.src, depth + 1) {
            Outcome::Resolved(Resolved {
                dst_id: Some(p),
                confidence,
                ..
            }) => (p, confidence),
            Outcome::Resolved(Resolved { external: true, .. }) => return self.after_external(c),
            _ => return self.untyped(c),
        };
        let Some(&pi) = self.by_id.get(&p) else {
            return self.untyped(c);
        };
        let ps = &self.syms[pi];
        // A class called (Python): the instance it makes.
        if is_type(&ps.kind) {
            return self.in_type(c, pi, "return_type", prev_conf);
        }
        // The declared return type: a Java method's, a TypeScript
        // function's (`(): Observable<T>`).
        let sig = ps.signature.as_deref();
        let ret = if self.is_java(&ps.file) {
            sig.and_then(|s| declared_type(s, &ps.name))
        } else if self.is_script(&ps.file) {
            sig.and_then(crate::resolve::typescript::return_type)
        } else if self.lang_key(&ps.file) == Some("python") {
            sig.and_then(crate::resolve::python::return_type)
        } else if self.lang_key(&ps.file) == Some("rust") {
            let ret = sig.and_then(crate::resolve::rust::return_type);
            let ret = if unwrap {
                ret.and_then(crate::resolve::rust::unwrapped)
            } else {
                ret
            };
            ret.map(|t| t.base)
        } else if self.lang_key(&ps.file) == Some("go") {
            sig.and_then(crate::resolve::go::return_type)
        } else {
            None
        };
        let Some(ret) = ret else {
            return self.untyped(c);
        };
        match self.resolve_type(&ret, &ps.file, Some(pi)) {
            TypeRef::Repo(t, how) => {
                let conf = min_conf(prev_conf, self.type_conf(t, how));
                self.in_type(c, t, "return_type", conf)
            }
            // A declared external return type (`Uni<…>`): the method is
            // that type's.
            TypeRef::External => cap(external("external_known"), prev_conf),
            TypeRef::Unknown => self.untyped(c),
        }
    }

    /// After a call from outside the repository nothing types the receiver:
    /// a name the repository defines, or shaped like an accessor, stays
    /// undecided (and reachable by name, DD-10); any other is
    /// `chain_external`, `medium` (TASK-005 review, MAJOR 2).
    pub(super) fn after_external(&self, c: &Call<'_>) -> Outcome {
        let known = !self.candidates(c.callee, c.file, c.args).is_empty();
        if known || accessor_shaped(c.callee) {
            undecided()
        } else {
            external_medium("chain_external")
        }
    }

    /// The value a receiver typed `X()` (`x = make()`, TASK-007) holds:
    /// what calling `X` returns, by the file's language; `None` for a type
    /// as written (`Foo`).
    pub(super) fn value_of(&self, ty: &str, file: &str, ctx: Option<usize>) -> Option<ValueType> {
        let (path, unwrap) = match ty.strip_suffix("()?") {
            Some(p) => (p, true),
            None => (ty.strip_suffix("()")?, false),
        };
        Some(match self.lang_key(file) {
            Some("python") => self.py_value(path, file, ctx),
            Some("rust") => self.rs_value(path, unwrap, file, ctx),
            Some("go") => self.go_value(path, file),
            _ => ValueType::Unknown,
        })
    }

    /// A Rust path call (`Foo::bar()` → `Foo.bar`, `std::fs::read()`).
    fn path_call(&self, c: &Call<'_>, e: &LinkEdge) -> Outcome {
        let file = c.file;
        let exact: Vec<usize> = self
            .by_qualified
            .get(&e.dst_name)
            .into_iter()
            .flatten()
            .copied()
            .filter(|&i| is_callable(&self.syms[i].kind))
            .collect();
        if let Some(&same) = exact.iter().find(|&&i| self.syms[i].file == file) {
            return hi(same, self, "same_file");
        }
        if exact.len() == 1 {
            return hi(exact[0], self, "unique_name");
        }
        if self.platform(file, &e.dst_name) {
            return external("external_known");
        }
        if !exact.is_empty() {
            return undecided();
        }
        // `Type.m` with no such method: the type's import says where it is
        // from (`use std::time::Instant` → external); never a guess by `m`.
        if let Some((ty, _)) = e.dst_name.rsplit_once('.') {
            let repo_type = self
                .by_qualified
                .get(ty)
                .into_iter()
                .flatten()
                .any(|&i| is_type(&self.syms[i].kind));
            if repo_type {
                return undecided();
            }
            let imports = self
                .files
                .get(file)
                .map(|f| f.imports.as_slice())
                .unwrap_or_default();
            if let Some(imp) = imports.iter().find(|i| leaf(i) == ty) {
                let first = imp.split("::").next().unwrap_or_default();
                let local = matches!(first, "crate" | "super" | "self")
                    || self
                        .packages
                        .iter()
                        .any(|p| p.split("::").next() == Some(first));
                // Outside the workspace (std or another crate): external.
                return if local {
                    undecided()
                } else {
                    external("external_known")
                };
            }
            return undecided();
        }
        self.by_name_only_low(c)
    }
}

/// One call being resolved.
pub(super) struct Call<'a> {
    pub(super) callee: &'a str,
    pub(super) src: Option<usize>,
    pub(super) file: &'a str,
    pub(super) args: Option<usize>,
}

/// TypeScript, TSX and JavaScript share one resolver.
fn is_script_lang(l: Lang) -> bool {
    matches!(l.key(), "typescript" | "tsx" | "javascript")
}

fn rank(conf: &str) -> u8 {
    match conf {
        "high" => 2,
        "medium" => 1,
        _ => 0,
    }
}

/// The lower of two confidences.
pub(super) fn min_conf(a: &'static str, b: &'static str) -> &'static str {
    if rank(a) <= rank(b) {
        a
    } else {
        b
    }
}

/// `o` no surer than `conf` (a chain is no surer than its previous call, a
/// type found by its name alone no surer than `medium`).
pub(super) fn cap(o: Outcome, conf: &'static str) -> Outcome {
    match o {
        Outcome::Resolved(mut r) if rank(r.confidence) > rank(conf) => {
            r.confidence = conf;
            Outcome::Resolved(r)
        }
        o => o,
    }
}

/// `getX`/`isX`/`setX`: a name an accessor would have, generated or not.
fn accessor_shaped(name: &str) -> bool {
    ["get", "is", "set"].iter().any(|p| {
        name.strip_prefix(p)
            .and_then(|r| r.chars().next())
            .is_some_and(char::is_uppercase)
    })
}

/// The link pass's rows of one parsed file (ids assigned), the way the
/// indexer writes them: every symbol (the file's first) and every edge but
/// `contains`. For tests and tools that resolve without a store.
pub fn link_rows(file: &str, pf: &ParsedFile) -> (Vec<LinkSymbol>, Vec<LinkEdge>) {
    let package = pf.facts.package.clone();
    let symbols = std::iter::once(&pf.file_symbol)
        .chain(&pf.symbols)
        .map(|s| LinkSymbol {
            id: s.id,
            parent_id: s.parent_id,
            file: file.to_string(),
            kind: s.kind.clone(),
            name: s.name.clone(),
            qualified: s.qualified.clone(),
            package: package.clone(),
            signature: (!s.signature.is_empty()).then(|| s.signature.clone()),
            exported: s.exported,
            start_line: s.start_line as i32,
            end_line: s.end_line as i32,
        })
        .collect();
    let edge = |kind: &str, src_id: u64, dst: &str, hint: Option<String>, line: u32| LinkEdge {
        kind: kind.to_string(),
        src_id,
        dst_name: dst.to_string(),
        hint,
        file: file.to_string(),
        line: line as i32,
    };
    let mut edges: Vec<LinkEdge> = pf
        .edges
        .iter()
        .chain(&pf.module_edges)
        .map(|e| edge(&e.kind, e.src_id, &e.target, e.hint.clone(), e.line))
        .collect();
    let file_id = pf.file_symbol.id;
    let hint = |i: &crate::facts::ImportFact| match Lang::named(&pf.language) {
        Some(l) => resolver_for(l).import_hint(i),
        None => i.hint(),
    };
    edges.extend(
        pf.facts
            .imports
            .iter()
            .map(|i| edge(IMPORTS, file_id, &i.target, hint(i), i.line)),
    );
    edges.extend(
        pf.facts
            .inherits
            .iter()
            .map(|f| edge(&f.kind, f.src_id, &f.name, None, f.line)),
    );
    edges.extend(
        pf.facts
            .refs
            .iter()
            .map(|r| edge(&r.kind, r.src_id, &r.name, None, r.line)),
    );
    (symbols, edges)
}
