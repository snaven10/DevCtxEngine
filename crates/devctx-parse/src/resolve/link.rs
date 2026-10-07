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

fn hi(dst: usize, idx: &RepoIndex, resolution: &'static str) -> Outcome {
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

fn external(resolution: &'static str) -> Outcome {
    Outcome::Resolved(Resolved {
        dst_id: None,
        confidence: "high",
        resolution,
        external: true,
    })
}

/// External, but only as far as the receiver goes (an untyped one's
/// `toString`, a repository name after an external call): `medium`.
fn external_medium(resolution: &'static str) -> Outcome {
    Outcome::Resolved(Resolved {
        dst_id: None,
        confidence: "medium",
        resolution,
        external: true,
    })
}

/// A destination found by convention, not by a definition (a Lombok
/// accessor of a field): `medium`.
fn medium(dst: usize, idx: &RepoIndex, resolution: &'static str) -> Outcome {
    Outcome::Resolved(Resolved {
        dst_id: Some(idx.syms[dst].id),
        confidence: "medium",
        resolution,
        external: false,
    })
}

fn undecided() -> Outcome {
    Outcome::Resolved(Resolved {
        dst_id: None,
        confidence: "low",
        resolution: "name_only",
        external: false,
    })
}

/// How a type name resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TypeRef {
    /// A type of the repository, and how it was found.
    Repo(usize, &'static str),
    /// From outside the repository, with evidence.
    External,
    /// Nothing says.
    Unknown,
}

/// A member lookup through the supertypes.
enum Member {
    /// Found, in the type itself (`false`) or a supertype (`true`).
    Found(usize, bool),
    /// Not in the repository; some supertype is external (Panache,
    /// `Object`, an enum's `Enum`).
    External,
    /// Not found, and nothing says it is external.
    Missing,
}

#[derive(Default)]
struct FileInfo {
    lang: Option<Lang>,
    package: Option<String>,
    imports: Vec<String>,
}

/// What the link pass knows of a branch: its symbols, by id, qualified name,
/// bare name and container; each file's package and imports; each type's
/// supertypes, resolved.
pub struct RepoIndex {
    syms: Vec<LinkSymbol>,
    by_id: HashMap<u64, usize>,
    by_qualified: HashMap<String, Vec<usize>>,
    by_name: HashMap<String, Vec<usize>>,
    children: HashMap<u64, Vec<usize>>,
    files: HashMap<String, FileInfo>,
    packages: HashSet<String>,
    /// Java: `package.Qualified` of every type.
    fq_types: HashMap<String, Vec<usize>>,
    supers: HashMap<u64, Vec<TypeRef>>,
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

fn is_type(kind: &str) -> bool {
    kind_class(kind) == "type"
}

fn is_callable(kind: &str) -> bool {
    kind_class(kind) == "callable"
}

/// The last segment of a destination (`a.b.C.m` → `m`, `x::y` → `y`).
fn leaf(name: &str) -> &str {
    let after_colons = name.rsplit("::").next().unwrap_or(name);
    after_colons.rsplit('.').next().unwrap_or(after_colons)
}

impl RepoIndex {
    /// Index `symbols`; `facts` brings each file's `imports` and each type's
    /// `inherits`/`implements` (every other edge in it is ignored).
    pub fn new(symbols: Vec<LinkSymbol>, facts: &[LinkEdge]) -> Self {
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
        for e in facts.iter().filter(|e| e.kind == IMPORTS) {
            if let Some(info) = idx.files.get_mut(&e.file) {
                info.imports.push(e.dst_name.clone());
            }
        }
        let mut supers: HashMap<u64, Vec<TypeRef>> = HashMap::new();
        for e in facts
            .iter()
            .filter(|e| e.kind == INHERITS || e.kind == IMPLEMENTS)
        {
            let Some(&src) = idx.by_id.get(&e.src_id) else {
                continue;
            };
            // A Rust `impl Trait for T` gives `T` its supertype.
            let owner = if idx.syms[src].kind == "impl" {
                idx.syms[src]
                    .parent_id
                    .and_then(|p| idx.by_id.get(&p).copied())
                    .filter(|&p| is_type(&idx.syms[p].kind))
                    .unwrap_or(src)
            } else {
                src
            };
            let r = idx.resolve_type(&e.dst_name, &e.file, Some(src));
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

    fn lang_of(&self, file: &str) -> Option<Lang> {
        self.files.get(file).and_then(|f| f.lang)
    }

    fn is_java(&self, file: &str) -> bool {
        self.lang_of(file).is_some_and(|l| l.key() == "java")
    }

    fn platform(&self, file: &str, name: &str) -> bool {
        self.lang_of(file)
            .is_some_and(|l| resolver_for(l).is_platform(l.def(), name))
    }

    /// `src` and every symbol around it, innermost first (file symbol last).
    fn chain(&self, src: Option<usize>) -> Vec<usize> {
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

    /// The types around `src`, innermost first.
    fn containers(&self, src: Option<usize>) -> Vec<usize> {
        self.chain(src)
            .into_iter()
            .filter(|&i| is_type(&self.syms[i].kind))
            .collect()
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
                return Member::Found(self.pick_overload(&found, args), inherited);
            }
            for r in self.supers.get(&self.syms[cur].id).into_iter().flatten() {
                match *r {
                    TypeRef::Repo(s, _) => queue.push_back((s, true)),
                    TypeRef::External => external_super = true,
                    TypeRef::Unknown => {}
                }
            }
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
        .any(|a| sig.split_whitespace().any(|w| w.starts_with(a)));
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
            Member::Found(f, _) => Some(f),
            _ => None,
        }
    }

    /// Of several overloads, the first whose arity takes `args`.
    fn pick_overload(&self, found: &[usize], args: Option<usize>) -> usize {
        let fits = |m: usize| -> bool {
            let (Some(n), Some(sig)) = (args, self.syms[m].signature.as_deref()) else {
                return true;
            };
            match arity(sig) {
                Some((k, varargs)) => k == n || (varargs && n + 1 >= k),
                None => true,
            }
        };
        found.iter().copied().find(|&m| fits(m)).unwrap_or(found[0])
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
    fn by_name_only(&self, callee: &str, file: &str, args: Option<usize>) -> Outcome {
        let c = self.candidates(callee, file, args);
        match c.len() {
            1 => hi(c[0], self, "unique_name"),
            0 if self.platform(file, callee) => external("external_known"),
            _ => undecided(),
        }
    }

    /// A receiver nothing types: the name must exist in the repository to
    /// be kept, and then nothing decides it.
    fn untyped(&self, callee: &str, file: &str, args: Option<usize>) -> Outcome {
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

    // ------------------------------------------------------------------ types

    /// Resolve a type name as written in `file`, seen from `ctx`.
    fn resolve_type(&self, name: &str, file: &str, ctx: Option<usize>) -> TypeRef {
        if self.is_java(file) {
            return self.java_type(name, file, ctx);
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
    fn same_file_type(&self, name: &str, file: &str, ctx: Option<usize>) -> Option<usize> {
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
            let pkg = name.rsplit_once('.').map_or("", |(p, _)| p);
            let lower = first.chars().next().is_some_and(char::is_lowercase);
            if lower && !self.packages.contains(pkg) && !self.packages.contains(first) {
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
            TypeRef::Repo(t, how) => hi(t, self, how),
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
            return hi(t, self, "import");
        }
        // `import static a.b.C.m`.
        if let Some((owner, member)) = target.rsplit_once('.') {
            if let Some(t) = self.fq(owner) {
                return match self.find_member(t, member, true, None) {
                    Member::Found(m, _) => hi(m, self, "import"),
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
        let file = e.file.as_str();
        match tokens.as_slice() {
            ["this"] => self.this_call(callee, src, file, args, false),
            ["super"] => self.this_call(callee, src, file, args, true),
            ["typed", via, ty] => self.typed_call(callee, via, ty, src, file, args),
            ["name", recv] => self.name_call(callee, recv, src, file, args),
            ["untyped"] | ["expr"] | ["chain"] => self.untyped(callee, file, args),
            ["chain", prev, rest @ ..] if depth < 8 => {
                self.chain_call(callee, prev, rest, e, src, args, depth)
            }
            ["chain", ..] => self.untyped(callee, file, args),
            ["path"] => self.path_call(e, file, args),
            ["bare"] => self.bare_call(callee, src, file, args),
            // A row with no hint (written before hints): qualified → as a
            // name, bare → as a bare call.
            _ => match e.dst_name.rsplit_once('.') {
                Some((recv, _)) => self.name_call(callee, recv, src, file, args),
                None => self.bare_call(callee, src, file, args),
            },
        }
    }

    /// `this.m()` / `super.m()` (rule 1).
    fn this_call(
        &self,
        callee: &str,
        src: Option<usize>,
        file: &str,
        args: Option<usize>,
        sup: bool,
    ) -> Outcome {
        let Some(&c) = self.containers(src).first() else {
            return self.bare_call(callee, src, file, args);
        };
        if sup {
            let mut external_super = false;
            for r in self.supers.get(&self.syms[c].id).into_iter().flatten() {
                match *r {
                    TypeRef::Repo(s, _) => {
                        if let Member::Found(m, _) = self.find_member(s, callee, true, args) {
                            return hi(m, self, "inherited");
                        }
                    }
                    TypeRef::External => external_super = true,
                    TypeRef::Unknown => {}
                }
            }
            return if external_super {
                external("inherited")
            } else {
                undecided()
            };
        }
        match self.find_member(c, callee, true, args) {
            Member::Found(m, false) => hi(m, self, "self"),
            Member::Found(m, true) => hi(m, self, "inherited"),
            Member::External => external("inherited"),
            Member::Missing => undecided(),
        }
    }

    /// `m()` (rule 6, then 7-9): a member of an enclosing type (or its
    /// supertypes), a function of an enclosing scope of the file, a static
    /// import; then by name.
    fn bare_call(
        &self,
        callee: &str,
        src: Option<usize>,
        file: &str,
        args: Option<usize>,
    ) -> Outcome {
        let mut external_super = false;
        for anc in self.chain(src) {
            let s = &self.syms[anc];
            if is_type(&s.kind) {
                match self.find_member(anc, callee, true, args) {
                    Member::Found(m, false) => return hi(m, self, "self"),
                    Member::Found(m, true) => return hi(m, self, "inherited"),
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
                .filter(|&c| self.syms[c].name == callee && is_callable(&self.syms[c].kind))
                .collect();
            if !local.is_empty() {
                return hi(self.pick_overload(&local, args), self, "same_file");
            }
        }
        if src.is_none() {
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
                    TypeRef::Repo(t, _) => {
                        if let Member::Found(m, _) = self.find_member(t, callee, true, args) {
                            return hi(m, self, "import");
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
                    _ => {}
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
    fn typed_call(
        &self,
        callee: &str,
        via: &str,
        ty: &str,
        src: Option<usize>,
        file: &str,
        args: Option<usize>,
    ) -> Outcome {
        let resolution: &'static str = match via {
            "field" => "field",
            "param" => "param",
            "ctor_inject" => "ctor_inject",
            _ => "local",
        };
        let statik = via == "static";
        match self.resolve_type(ty, file, src) {
            TypeRef::Repo(t, _) => match self.find_member(t, callee, true, args) {
                Member::Found(m, false) => hi(m, self, resolution),
                Member::Found(m, true) => hi(m, self, "inherited"),
                Member::External if !statik => external("inherited"),
                _ if statik => self.untyped(callee, file, args),
                _ => match self.accessor(t, callee) {
                    Some(f) => medium(f, self, resolution),
                    None => undecided(),
                },
            },
            TypeRef::External if !statik => external("external_known"),
            _ if statik => self.untyped(callee, file, args),
            _ => self.by_name_only_low(callee, file, args),
        }
    }

    /// A receiver whose type is named but unknown: never discarded, never
    /// more than medium.
    fn by_name_only_low(&self, callee: &str, file: &str, args: Option<usize>) -> Outcome {
        let c = self.candidates(callee, file, args);
        if c.len() == 1 {
            hi(c[0], self, "unique_name")
        } else {
            undecided()
        }
    }

    /// `R.m()` with `R` bound by no scope (rule 5): a type, or an inherited
    /// field; else as untyped.
    fn name_call(
        &self,
        callee: &str,
        recv: &str,
        src: Option<usize>,
        file: &str,
        args: Option<usize>,
    ) -> Outcome {
        match self.resolve_type(recv, file, src) {
            TypeRef::Repo(t, how) => {
                return match self.find_member(t, callee, true, args) {
                    Member::Found(m, false) => hi(m, self, how),
                    Member::Found(m, true) => hi(m, self, "inherited"),
                    Member::External => external("inherited"),
                    Member::Missing => match self.accessor(t, callee) {
                        Some(f) => medium(f, self, how),
                        None => undecided(),
                    },
                }
            }
            TypeRef::External => return external("external_known"),
            TypeRef::Unknown => {}
        }
        if self.is_java(file) {
            // A field of an enclosing type or its supertypes (`LOG` of a
            // base class): typed by its declaration.
            for c in self.containers(src) {
                if let Member::Found(f, inherited) = self.find_member(c, recv, false, None) {
                    let fs = &self.syms[f];
                    let Some(ty) = fs.signature.as_deref().and_then(|s| declared_type(s, recv))
                    else {
                        break;
                    };
                    let how = if inherited { "inherited" } else { "field" };
                    return match self.resolve_type(&ty, &fs.file, Some(f)) {
                        TypeRef::Repo(t, _) => match self.find_member(t, callee, true, args) {
                            Member::Found(m, _) => hi(m, self, how),
                            Member::External => external("inherited"),
                            Member::Missing => undecided(),
                        },
                        TypeRef::External => external("external_known"),
                        TypeRef::Unknown => self.untyped(callee, file, args),
                    };
                }
            }
            return self.untyped(callee, file, args);
        }
        let qualified = format!("{recv}.{callee}");
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
        if self.platform(file, recv) || self.platform(file, &qualified) {
            return external("external_known");
        }
        undecided()
    }

    /// `a.b().c()`: the previous call (itself resolved the same way, back
    /// along the chain) types the receiver — a repository method by its
    /// declared return type, without generics (DD-7, Java); an external one
    /// makes this one external too (a repository name keeps it `medium`:
    /// an external call can return a repository type). Else untyped.
    #[allow(clippy::too_many_arguments)]
    fn chain_call(
        &self,
        callee: &str,
        prev: &str,
        prev_hint: &[&str],
        e: &LinkEdge,
        src: Option<usize>,
        args: Option<usize>,
        depth: u8,
    ) -> Outcome {
        let file = e.file.as_str();
        let inner = LinkEdge {
            dst_name: prev.to_string(),
            hint: Some(prev_hint.join(" ")),
            ..e.clone()
        };
        let p = match self.resolve_call(&inner, src, depth + 1) {
            Outcome::Resolved(Resolved {
                dst_id: Some(p), ..
            }) => p,
            Outcome::Resolved(Resolved { external: true, .. }) => {
                return if self.candidates(callee, file, args).is_empty() {
                    external("return_type")
                } else {
                    external_medium("return_type")
                };
            }
            _ => return self.untyped(callee, file, args),
        };
        if !self.is_java(file) {
            return self.untyped(callee, file, args);
        }
        let Some(&pi) = self.by_id.get(&p) else {
            return self.untyped(callee, file, args);
        };
        let ps = &self.syms[pi];
        let Some(ret) = ps
            .signature
            .as_deref()
            .and_then(|s| declared_type(s, &ps.name))
        else {
            return self.untyped(callee, file, args);
        };
        match self.resolve_type(&ret, &ps.file, Some(pi)) {
            TypeRef::Repo(t, _) => match self.find_member(t, callee, true, args) {
                Member::Found(m, _) => hi(m, self, "return_type"),
                Member::External => external("return_type"),
                Member::Missing => match self.accessor(t, callee) {
                    Some(f) => medium(f, self, "return_type"),
                    None => undecided(),
                },
            },
            TypeRef::External => external("return_type"),
            TypeRef::Unknown => self.untyped(callee, file, args),
        }
    }

    /// A Rust path call (`Foo::bar()` → `Foo.bar`, `std::fs::read()`).
    fn path_call(&self, e: &LinkEdge, file: &str, args: Option<usize>) -> Outcome {
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
        self.by_name_only_low(leaf(&e.dst_name), file, args)
    }
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
    edges.extend(
        pf.facts
            .imports
            .iter()
            .map(|i| edge(IMPORTS, file_id, &i.target, None, i.line)),
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
