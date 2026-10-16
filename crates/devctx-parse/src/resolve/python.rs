//! Python (PLAN-009 TASK-007, DD-7): what a file declares about itself, the
//! type a binding takes from its value, how an annotation reads, and the link
//! pass's rules — modules by path and package (`__init__.py`, the repository
//! root, `src/`, a script's own directory), relative imports, re-exports
//! through a package, star imports, the standard library and the declared
//! distributions as external.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use tree_sitter::Node;

use super::link::{
    cap, external, hi, is_callable, is_type, medium, min_conf, undecided, Call, LinkEdge, Outcome,
    RepoIndex, Resolved, TypeRef, ValueType, IMPORT_WEAK,
};
use super::scope::{TypeText, Via};
use super::typescript::{dir_of, join};
use super::{strip_extension, LangResolver};
use crate::facts::ImportFact;

/// Python.
pub struct Python;

impl LangResolver for Python {
    /// `pkg/sub/mod.py` → `pkg.sub.mod`; a package's `__init__.py` is the
    /// package itself.
    fn package_from_path(&self, path: &str) -> Option<String> {
        let stem = strip_extension(path);
        let stem = stem
            .strip_suffix("/__init__")
            .or_else(|| (stem == "__init__").then_some(""))
            .unwrap_or(stem);
        Some(stem.replace('/', "."))
    }

    /// No leading `_` (a dunder is public), and not nested in a function:
    /// a decorator's `wrapper` is no name of its module.
    fn exported(&self, def: Node<'_>, name: &str, _bytes: &[u8]) -> Option<bool> {
        let mut cur = def.parent();
        while let Some(n) = cur {
            if n.kind() == "function_definition" {
                return Some(false);
            }
            cur = n.parent();
        }
        Some(!name.starts_with('_') || (name.starts_with("__") && name.ends_with("__")))
    }

    fn import_target(&self, imp: &ImportFact) -> String {
        let join = |tail: &str| {
            if imp.path.chars().all(|c| c == '.') {
                format!("{}{tail}", imp.path) // `from . import x` → `.x`
            } else {
                format!("{}.{tail}", imp.path)
            }
        };
        match (&imp.name, imp.wildcard) {
            (_, true) => join("*"),
            (Some(n), false) => join(n),
            (None, false) => imp.path.clone(),
        }
    }

    /// `from x import y` keeps `from`: `import a.b` and `from a import b`
    /// write the same destination and bind different names (`a`, `b`).
    fn import_hint(&self, imp: &ImportFact) -> Option<String> {
        let base = imp.hint();
        if imp.name.is_none() || imp.wildcard {
            return base;
        }
        Some(match base {
            Some(b) => format!("{b} from"),
            None => "from".to_string(),
        })
    }

    /// A bare name is never a member: `self.x` reaches it.
    fn implicit_this(&self) -> bool {
        false
    }

    fn fields_by_link(&self) -> bool {
        true
    }

    fn late_module_bindings(&self) -> bool {
        true
    }

    fn literal_type(&self, kind: &str) -> Option<&'static str> {
        Some(match kind {
            "string" | "concatenated_string" => "str",
            "list" | "list_comprehension" => "list",
            "dictionary" | "dictionary_comprehension" => "dict",
            "set" | "set_comprehension" => "set",
            "tuple" => "tuple",
            "integer" => "int",
            "float" => "float",
            "true" | "false" => "bool",
            _ => return None,
        })
    }

    /// `Optional[Foo]`, `Foo | None` and `"Foo"` are `Foo`; `list[Foo]` is
    /// `list` holding `Foo`.
    fn type_text(&self, node: Node<'_>, bytes: &[u8]) -> Option<TypeText> {
        type_text(node.utf8_text(bytes).ok()?)
    }

    /// `Foo(…)` / `mod.make(…)` is what calling it returns (`Foo()`, typed
    /// by the link pass: an instance of a class, a function's declared
    /// return type); a literal is of its builtin type.
    fn init_type(&self, init: Node<'_>, bytes: &[u8]) -> Option<(TypeText, Via)> {
        match init.kind() {
            "call" => {
                let f = init.child_by_field_name("function")?;
                if !matches!(f.kind(), "identifier" | "attribute") {
                    return None;
                }
                let text: String = f
                    .utf8_text(bytes)
                    .ok()?
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .collect();
                dotted(&text).then(|| {
                    (
                        TypeText {
                            base: format!("{text}()"),
                            elem: None,
                        },
                        Via::Local,
                    )
                })
            }
            "await" | "parenthesized_expression" => self.init_type(init.named_child(0)?, bytes),
            k => self.literal_type(k).map(|t| {
                (
                    TypeText {
                        base: t.to_string(),
                        elem: None,
                    },
                    Via::Local,
                )
            }),
        }
    }
}

/// A run of identifiers joined by dots.
fn dotted(text: &str) -> bool {
    !text.is_empty()
        && text.split('.').all(|p| {
            let mut c = p.chars();
            c.next().is_some_and(|f| f.is_alphabetic() || f == '_')
                && c.all(|ch| ch.is_alphanumeric() || ch == '_')
        })
}

/// An annotation as a type: quotes off, `Optional[X]`/`Union[X, None]`/`X |
/// None` → `X`, `Annotated[X, …]` → `X`, `X[Y]` → `X` holding `Y`.
pub fn type_text(text: &str) -> Option<TypeText> {
    let t: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let t = t.trim_matches(|c| c == '"' || c == '\'');
    if t.is_empty() {
        return None;
    }
    // `Any` and `object` type nothing (review P1).
    let short = t.rsplit('.').next().unwrap_or(t);
    if matches!(short, "Any" | "object") {
        return None;
    }
    let members: Vec<&str> = split_top(t, '|')
        .into_iter()
        .filter(|m| *m != "None")
        .collect();
    if members.len() > 1 {
        return None; // `A | B`: no one type
    }
    if members.len() == 1 && members[0] != t {
        return type_text(members[0]);
    }
    if let Some(open) = t.find('[') {
        let head = &t[..open];
        let inner = t[open + 1..].strip_suffix(']')?;
        let args: Vec<&str> = split_top(inner, ',')
            .into_iter()
            .filter(|a| *a != "None")
            .collect();
        let short = head.rsplit('.').next().unwrap_or(head);
        if matches!(
            short,
            "Optional" | "Annotated" | "Final" | "ClassVar" | "Required"
        ) || (short == "Union" && args.len() == 1)
            // `type[X]`: the class itself, whose members a call reaches.
            || matches!(short, "type" | "Type")
        {
            return type_text(args.first()?);
        }
        if short == "Union" {
            return None; // several types
        }
        let elem = args.first().and_then(|a| type_text(a)).map(|e| e.base);
        return Some(TypeText {
            base: head.to_string(),
            elem,
        });
    }
    Some(TypeText {
        base: t.to_string(),
        elem: None,
    })
}

/// `text` split at `sep` outside brackets.
fn split_top(text: &str, sep: char) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    for (i, c) in text.char_indices() {
        match c {
            '[' | '(' => depth += 1,
            ']' | ')' => depth = depth.saturating_sub(1),
            c if c == sep && depth == 0 => {
                out.push(&text[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    out.push(&text[start..]);
    out
}

/// The return type a Python signature declares (`def load(self) ->
/// "Item":` → `Item`, `-> Optional[Repo]` → `Repo`); `None` when it
/// declares none.
pub fn return_type(signature: &str) -> Option<String> {
    let close = signature.rfind(')')?;
    let rest = signature[close + 1..].trim();
    let ty = rest.strip_prefix("->")?.trim();
    let ty = ty.strip_suffix(':').unwrap_or(ty).trim();
    let t = type_text(ty)?;
    (dotted(&t.base)).then_some(t.base)
}

/// One name an `imports` row of a Python file binds, read back from its
/// destination (`a.b`, `.m.x`, `m.*`) and its hint (`as <local>`, `from`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PyImport {
    /// The module as written (`a.b`, `.m`, `..`).
    pub module: String,
    /// The imported name of a `from … import name`.
    pub name: Option<String>,
    /// The local name it binds; `None` for a star import.
    pub local: Option<String>,
    /// `from m import *`.
    pub wildcard: bool,
}

impl PyImport {
    /// From an `imports` row.
    pub fn from_row(dst_name: &str, hint: Option<&str>) -> Self {
        let tokens: Vec<&str> = hint.unwrap_or_default().split_whitespace().collect();
        let alias = tokens
            .iter()
            .position(|t| *t == "as")
            .and_then(|i| tokens.get(i + 1))
            .map(|a| a.to_string());
        let from = tokens.contains(&"from");
        let dots = dst_name.chars().take_while(|c| *c == '.').count();
        let (lead, rest) = dst_name.split_at(dots);
        if let Some(module) = rest.strip_suffix('*') {
            let module = module.strip_suffix('.').unwrap_or(module);
            return Self {
                module: format!("{lead}{module}"),
                name: None,
                local: None,
                wildcard: true,
            };
        }
        if from {
            let (module, name) = match rest.rsplit_once('.') {
                Some((m, n)) => (format!("{lead}{m}"), n.to_string()),
                None => (lead.to_string(), rest.to_string()),
            };
            return Self {
                module,
                local: Some(alias.unwrap_or_else(|| name.clone())),
                name: Some(name),
                wildcard: false,
            };
        }
        let local = alias.unwrap_or_else(|| rest.split('.').next().unwrap_or(rest).to_string());
        Self {
            module: dst_name.to_string(),
            name: None,
            local: Some(local),
            wildcard: false,
        }
    }

    /// The module the local name is bound to, for a plain `import`: the
    /// whole path with an alias (`import a.b as c`), its first segment
    /// without (`import a.b` binds `a`).
    fn bound_module(&self, alias: bool) -> &str {
        if alias {
            &self.module
        } else {
            self.module.split('.').next().unwrap_or(&self.module)
        }
    }
}

// ------------------------------------------------------------- the link pass

/// Where a Python module is.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum PyMod {
    /// A file of the repository (a module, or a package's `__init__.py`).
    File(String),
    /// A directory of the repository with no `__init__.py` (a namespace
    /// package).
    Dir(String),
    /// The standard library, or a declared distribution (DD-9).
    External,
    /// Not found, and nothing declares it.
    Unknown,
}

/// What a dotted name of a file reaches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PySym {
    /// A symbol of the repository, how it was found, and whether surely
    /// (`false`: through a star import shared with another, a re-export not
    /// followed to the end).
    Sym(usize, &'static str, bool),
    /// A module of the repository.
    Module(PyMod),
    /// From outside the repository, with evidence.
    External,
    /// Nothing says.
    Unknown,
}

/// What a module exports under a name.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PyExport {
    Found(usize, bool),
    Module(PyMod),
    External,
    Missing,
    Unknown,
}

/// What says a top-level module is from outside the repository (review P3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Evidence {
    /// A manifest declares it under that exact (normalised or aliased) name.
    Declared,
    /// Only a name guessed from a declared distribution.
    Guessed,
    /// The standard library.
    Stdlib,
    None,
}

/// Memo of module lookups and exports.
#[derive(Default)]
pub(super) struct PyCache {
    modules: Mutex<HashMap<(String, String), PyMod>>,
    exports: Mutex<HashMap<(PyMod, String, u8), PyExport>>,
}

/// How many re-exports deep a name is followed (`pkg/__init__.py` → `pkg/a.py`
/// → …).
const REEXPORT_DEPTH: u8 = 4;

/// Builtins that are types: calling one makes an instance of a known type
/// (`dict()`), not the value of an arbitrary function (`open()`).
const BUILTIN_TYPES: &[&str] = &[
    "str",
    "int",
    "float",
    "bool",
    "list",
    "dict",
    "set",
    "frozenset",
    "tuple",
    "bytes",
    "bytearray",
    "object",
    "complex",
];

impl RepoIndex {
    /// The file or directory `path` (a module path joined with `/`) names:
    /// `path.py`, `path/__init__.py`, `path.pyi`, or a directory.
    fn py_probe(&self, path: &str) -> Option<PyMod> {
        for p in [
            format!("{path}.py"),
            if path.is_empty() {
                "__init__.py".to_string()
            } else {
                format!("{path}/__init__.py")
            },
            format!("{path}.pyi"),
        ] {
            if self.files.contains_key(&p) {
                return Some(PyMod::File(p));
            }
        }
        (!path.is_empty() && self.py_dirs.contains(path)).then(|| PyMod::Dir(path.to_string()))
    }

    /// Where an absolute import written in `from` may start: the file's own
    /// directory when it is no package (a script: its directory is on
    /// `sys.path`), the repository root, `src/`, and the directory of each
    /// Python manifest above the file (and its `src/`).
    fn py_roots(&self, from: &str) -> Vec<String> {
        let d = dir_of(from);
        let mut roots = Vec::new();
        let in_package = self.files.contains_key(&join(d, "__init__.py"));
        if !in_package {
            roots.push(d.to_string());
        }
        roots.push(String::new());
        roots.push("src".to_string());
        for m in self.python.iter().filter(|m| contains(&m.dir, d)) {
            roots.push(m.dir.clone());
            roots.push(join(&m.dir, "src"));
        }
        let mut seen = HashSet::new();
        roots.retain(|r| seen.insert(r.clone()));
        roots
    }

    /// What says the top-level module `top`, imported in `from`, is from
    /// outside the repository: a distribution the root Python manifests or
    /// the nearest one declare under that exact name (or a known alias), a
    /// name only guessed from one (`python-utils` → `utils`), the standard
    /// library — and nothing when a manifest names the project itself so.
    fn py_external(&self, from: &str, top: &str) -> Evidence {
        if self.python.iter().any(|m| m.name.as_deref() == Some(top)) {
            return Evidence::None;
        }
        let d = dir_of(from);
        let nearest = self
            .python
            .iter()
            .filter(|m| contains(&m.dir, d))
            .map(|m| m.dir.len())
            .max();
        let mine: Vec<&crate::resolve::env::PyManifest> = self
            .python
            .iter()
            .filter(|m| m.dir.is_empty() || Some(m.dir.len()) == nearest && contains(&m.dir, d))
            .collect();
        let has = |list: &Vec<String>| list.binary_search_by(|x| x.as_str().cmp(top)).is_ok();
        if mine.iter().any(|m| has(&m.deps)) {
            return Evidence::Declared;
        }
        if mine.iter().any(|m| has(&m.guessed)) {
            return Evidence::Guessed;
        }
        if let Some(l) = self.lang_of(from) {
            if l.def().platform_modules.iter().any(|m| m == top) {
                return Evidence::Stdlib;
            }
        }
        Evidence::None
    }

    /// The module `spec` names when `from` imports it.
    pub(super) fn py_module(&self, from: &str, spec: &str) -> PyMod {
        let key = (dir_of(from).to_string(), spec.to_string());
        if let Some(m) = self
            .py_cache
            .modules
            .lock()
            .ok()
            .and_then(|c| c.get(&key).cloned())
        {
            return m;
        }
        let m = self.py_module_uncached(from, spec);
        if let Ok(mut c) = self.py_cache.modules.lock() {
            c.insert(key, m.clone());
        }
        m
    }

    fn py_module_uncached(&self, from: &str, spec: &str) -> PyMod {
        let dots = spec.chars().take_while(|c| *c == '.').count();
        let rest = &spec[dots..];
        let rel = rest.replace('.', "/");
        if dots > 0 {
            let mut base = dir_of(from).to_string();
            for _ in 1..dots {
                if base.is_empty() {
                    return PyMod::Unknown;
                }
                base = dir_of(&base).to_string();
            }
            // `from .. import x` must stay inside a package: past the top
            // one is no module (review).
            if dots > 1 && !self.files.contains_key(&join(&base, "__init__.py")) {
                return PyMod::Unknown;
            }
            return self.py_probe(&join(&base, &rel)).unwrap_or(PyMod::Unknown);
        }
        let top = rest.split('.').next().unwrap_or(rest);
        if top.is_empty() {
            return PyMod::Unknown;
        }
        let repo = self
            .py_roots(from)
            .iter()
            .find_map(|r| self.py_probe(&join(r, &rel)));
        match self.py_external(from, top) {
            Evidence::Declared => PyMod::External,
            // A guessed distribution name the repository has a module of:
            // the repository's (review P3).
            Evidence::Guessed if repo.is_some() => repo.unwrap_or(PyMod::Unknown),
            // A standard library name the repository also has (a script's
            // own directory shadows it, a package's import does not):
            // undecided.
            Evidence::Stdlib if repo.is_some() => PyMod::Unknown,
            Evidence::Guessed | Evidence::Stdlib => PyMod::External,
            Evidence::None => repo.unwrap_or(PyMod::Unknown),
        }
    }

    /// Every path an import of `from` may name, for the incremental
    /// selection (a file added or deleted under it counts).
    fn py_reach(&self, from: &str, imp: &PyImport) -> Vec<String> {
        let dots = imp.module.chars().take_while(|c| *c == '.').count();
        let rel = imp.module[dots..].replace('.', "/");
        let bases: Vec<String> = if dots > 0 {
            let mut base = dir_of(from).to_string();
            for _ in 1..dots {
                base = dir_of(&base).to_string();
            }
            vec![join(&base, &rel)]
        } else {
            self.py_roots(from).iter().map(|r| join(r, &rel)).collect()
        };
        let mut out = Vec::new();
        let base_of = |b: &str| -> String {
            // The root the module path was joined to.
            let n = rel.split('/').filter(|x| !x.is_empty()).count();
            let mut d = b.to_string();
            for _ in 0..n {
                d = dir_of(&d).to_string();
            }
            d
        };
        for b in bases {
            let mut push = |p: &str| {
                out.push(format!("{p}.py"));
                out.push(join(p, "__init__.py"));
                out.push(format!("{p}.pyi"));
            };
            push(&b);
            if let Some(n) = &imp.name {
                push(&join(&b, n));
            }
            // Every package on the way (`import pkg.sub` binds `pkg`, whose
            // `__init__.py` is what `pkg.f()` reads: review P2).
            let mut prefix = base_of(&b);
            for seg in rel.split('/').filter(|x| !x.is_empty()) {
                prefix = join(&prefix, seg);
                out.push(join(&prefix, "__init__.py"));
            }
        }
        out
    }

    /// Every path the imports of a Python file may name; a Python module
    /// re-exports whatever it imports (`from pkg import X` reads `pkg`'s
    /// imports). `None` for a file with no imports.
    pub(super) fn py_file_reach(&self, file: &str) -> Option<(Vec<String>, bool)> {
        let imports = self.py_imports(file);
        if imports.is_empty() {
            return None;
        }
        let mut paths: Vec<String> = imports
            .iter()
            .flat_map(|i| self.py_reach(file, i))
            .collect();
        paths.sort();
        paths.dedup();
        Some((paths, true))
    }

    /// The Python imports of `file`, read back.
    fn py_imports(&self, file: &str) -> &[PyImport] {
        self.files
            .get(file)
            .map(|f| f.py.as_slice())
            .unwrap_or_default()
    }

    /// The top-level symbols of `file` named `name`.
    fn py_top(&self, file: &str, name: &str) -> Vec<usize> {
        let Some(fid) = self.files.get(file).and_then(|f| f.id) else {
            return Vec::new();
        };
        self.by_qualified
            .get(name)
            .into_iter()
            .flatten()
            .copied()
            .filter(|&i| self.syms[i].file == file && self.syms[i].parent_id == Some(fid))
            .collect()
    }

    /// What module `m` exports as `name`, `depth` re-exports deep.
    fn py_export(&self, m: &PyMod, name: &str, depth: u8) -> PyExport {
        let key = (m.clone(), name.to_string(), depth);
        if let Some(e) = self
            .py_cache
            .exports
            .lock()
            .ok()
            .and_then(|c| c.get(&key).cloned())
        {
            return e;
        }
        let e = self.py_export_walk(m, name, depth);
        if let Ok(mut c) = self.py_cache.exports.lock() {
            c.insert(key, e.clone());
        }
        e
    }

    fn py_export_walk(&self, m: &PyMod, name: &str, depth: u8) -> PyExport {
        if depth > REEXPORT_DEPTH {
            return PyExport::Unknown;
        }
        let file = match m {
            PyMod::File(f) => f,
            PyMod::Dir(d) => {
                return match self.py_probe(&join(d, name)) {
                    Some(sub) => PyExport::Module(sub),
                    None => PyExport::Missing,
                }
            }
            PyMod::External => return PyExport::External,
            PyMod::Unknown => return PyExport::Unknown,
        };
        // The module's own definition; a redefinition is the last one.
        if let Some(&i) = self.py_top(file, name).last() {
            return PyExport::Found(i, true);
        }
        // A name the module imports is one of its attributes.
        if let Some(imp) = self
            .py_imports(file)
            .iter()
            .rev()
            .find(|i| i.local.as_deref() == Some(name))
        {
            let target = self.py_module(file, &imp.module);
            return match &imp.name {
                Some(n) => match self.py_export(&target, n, depth + 1) {
                    PyExport::Missing => PyExport::Unknown,
                    e => e,
                },
                None => {
                    let alias = imp.local.as_deref() != imp.module.split('.').next();
                    PyExport::Module(self.py_module(file, imp.bound_module(alias)))
                }
            };
        }
        let (mut found, mut ext, mut unk) = (Vec::new(), false, false);
        for imp in self.py_imports(file).iter().filter(|i| i.wildcard) {
            match self.py_export(&self.py_module(file, &imp.module), name, depth + 1) {
                PyExport::Found(i, sure) => {
                    if !found.iter().any(|(j, _)| *j == i) {
                        found.push((i, sure));
                    }
                }
                PyExport::External => ext = true,
                PyExport::Unknown => unk = true,
                PyExport::Module(_) | PyExport::Missing => {}
            }
        }
        match found.as_slice() {
            [(i, sure)] => return PyExport::Found(*i, *sure && !ext && !unk),
            [_, ..] => return PyExport::Found(found.last().unwrap().0, false),
            // A star import that cannot be followed may have it: before an
            // external one (review).
            [] if unk => return PyExport::Unknown,
            [] if ext => return PyExport::External,
            [] => {}
        }
        // A package's submodule is an attribute once imported.
        if file.ends_with("__init__.py") {
            if let Some(sub) = self.py_probe(&join(dir_of(file), name)) {
                return PyExport::Module(sub);
            }
        }
        PyExport::Missing
    }

    /// What the local name `local` of `file` is bound to by an import.
    fn py_binding(&self, file: &str, local: &str) -> Option<PySym> {
        let imp = self
            .py_imports(file)
            .iter()
            .rev()
            .find(|i| i.local.as_deref() == Some(local))?;
        // Bound by two imports (`try: … except ImportError: …`): neither is
        // sure (review).
        let twice = self
            .py_imports(file)
            .iter()
            .filter(|i| i.local.as_deref() == Some(local))
            .count()
            > 1;
        Some(match self.py_binding_of(file, imp) {
            PySym::Sym(i, how, _) if twice => PySym::Sym(i, how, false),
            other => other,
        })
    }

    fn py_binding_of(&self, file: &str, imp: &PyImport) -> PySym {
        match &imp.name {
            Some(n) => match self.py_module(file, &imp.module) {
                PyMod::External => PySym::External,
                PyMod::Unknown => PySym::Unknown,
                m => match self.py_export(&m, n, 0) {
                    PyExport::Found(i, sure) => PySym::Sym(i, "import", sure),
                    PyExport::Module(m) => PySym::Module(m),
                    PyExport::External => PySym::External,
                    PyExport::Missing | PyExport::Unknown => PySym::Unknown,
                },
            },
            None => {
                let alias = imp.local.as_deref() != imp.module.split('.').next();
                match self.py_module(file, imp.bound_module(alias)) {
                    PyMod::External => PySym::External,
                    PyMod::Unknown => PySym::Unknown,
                    m => PySym::Module(m),
                }
            }
        }
    }

    /// A name no scope binds, as `file` sees it: a definition of the file (a
    /// class nested in the containers around `ctx` first), an import, a
    /// star import, a builtin.
    fn py_first(&self, name: &str, file: &str, ctx: Option<usize>) -> PySym {
        if let Some(t) = self.same_file_type(name, file, ctx) {
            return PySym::Sym(t, "same_file", true);
        }
        if let Some(&i) = self.py_top(file, name).last() {
            return PySym::Sym(i, "same_file", true);
        }
        if let Some(b) = self.py_binding(file, name) {
            return b;
        }
        let (mut found, mut ext, mut unk) = (Vec::new(), false, false);
        for imp in self.py_imports(file).iter().filter(|i| i.wildcard) {
            match self.py_export(&self.py_module(file, &imp.module), name, 0) {
                PyExport::Found(i, sure) => {
                    if !found.iter().any(|(j, _)| *j == i) {
                        found.push((i, sure));
                    }
                }
                PyExport::External => ext = true,
                PyExport::Unknown => unk = true,
                PyExport::Module(_) | PyExport::Missing => {}
            }
        }
        match found.as_slice() {
            [(i, sure)] => return PySym::Sym(*i, "import", *sure && !ext && !unk),
            [_, ..] => return PySym::Sym(found.last().unwrap().0, "import", false),
            [] if ext || unk => return PySym::Unknown,
            [] => {}
        }
        if self.platform(file, name) {
            return PySym::External;
        }
        PySym::Unknown
    }

    /// A dotted name (`a`, `mod.Cls`, `os.path`) as `file` sees it.
    pub(super) fn py_symbol(&self, path: &str, file: &str, ctx: Option<usize>) -> PySym {
        let mut segs = path.split('.');
        let first = segs.next().unwrap_or(path);
        let mut cur = self.py_first(first, file, ctx);
        for seg in segs {
            cur = match cur {
                PySym::Sym(t, how, sure) if is_type(&self.syms[t].kind) => {
                    let nested = format!("{}.{seg}", self.syms[t].qualified);
                    let inner = self
                        .by_qualified
                        .get(&nested)
                        .into_iter()
                        .flatten()
                        .copied()
                        .find(|&i| self.syms[i].file == self.syms[t].file);
                    match inner {
                        Some(i) => PySym::Sym(i, how, sure),
                        None => PySym::Unknown,
                    }
                }
                PySym::Sym(..) => PySym::Unknown,
                PySym::Module(m) => match self.py_export(&m, seg, 0) {
                    PyExport::Found(i, sure) => PySym::Sym(i, "import", sure),
                    PyExport::Module(m) => PySym::Module(m),
                    PyExport::External => PySym::External,
                    PyExport::Missing | PyExport::Unknown => PySym::Unknown,
                },
                PySym::External => PySym::External,
                PySym::Unknown => PySym::Unknown,
            };
        }
        cur
    }

    /// An annotation's type as `file` sees it (DD-7): a class of the file,
    /// one imported (through re-exports and star imports), a builtin or a
    /// module from outside. No type by its name alone: a name Python cannot
    /// see is no type of it.
    pub(super) fn py_type(&self, name: &str, file: &str, ctx: Option<usize>) -> TypeRef {
        let name = name.trim_matches(|c| c == '"' || c == '\'');
        // `-> Self`: the class around (review P1).
        if name == "Self" || name == "typing.Self" {
            return match self.containers(ctx).first() {
                Some(&t) => TypeRef::Repo(t, "self"),
                None => TypeRef::Unknown,
            };
        }
        match self.py_symbol(name, file, ctx) {
            PySym::Sym(t, how, sure) if is_type(&self.syms[t].kind) => {
                TypeRef::Repo(t, if sure { how } else { IMPORT_WEAK })
            }
            PySym::External => TypeRef::External,
            _ => TypeRef::Unknown,
        }
    }

    /// What calling `path` returns (`Foo()`, `make()`, `mod.factory()`):
    /// an instance of a class, a function's declared return type; the value
    /// of a call from outside the repository.
    pub(super) fn py_value(&self, path: &str, file: &str, ctx: Option<usize>) -> ValueType {
        match self.py_symbol(path, file, ctx) {
            PySym::Sym(t, how, sure) if is_type(&self.syms[t].kind) => {
                ValueType::Repo(t, how, if sure { "high" } else { "medium" })
            }
            PySym::Sym(f, _, sure) if is_callable(&self.syms[f].kind) => {
                let fs = &self.syms[f];
                let Some(ret) = fs.signature.as_deref().and_then(return_type) else {
                    return ValueType::Unknown;
                };
                let conf = if sure { "high" } else { "medium" };
                match self.py_type(&ret, &fs.file, Some(f)) {
                    TypeRef::Repo(t, how) => {
                        ValueType::Repo(t, "return_type", min_conf(conf, self.type_conf(t, how)))
                    }
                    TypeRef::External => ValueType::External(conf),
                    TypeRef::Unknown => ValueType::Unknown,
                }
            }
            PySym::External => {
                let last = path.rsplit('.').next().unwrap_or(path);
                if !path.contains('.') && BUILTIN_TYPES.contains(&last) {
                    ValueType::External("high")
                } else {
                    ValueType::FromExternalCall
                }
            }
            _ => ValueType::Unknown,
        }
    }

    /// `f()` in Python: a function nested in an enclosing function (hint
    /// `bare`), a definition of the module, an import (a class: its
    /// instantiation), a star import, a builtin. A name nothing makes
    /// visible is undecided, never a homonym by name (TASK-007, lesson 2).
    pub(super) fn py_bare(&self, c: &Call<'_>, scoped: bool) -> Outcome {
        if scoped {
            for anc in self.chain(c.src) {
                let s = &self.syms[anc];
                if is_type(&s.kind) || s.kind == devctx_core::symbol_id::FILE_KIND {
                    continue;
                }
                let local: Vec<usize> = self
                    .children_of(s.id)
                    .filter(|&k| self.syms[k].name == c.callee && is_callable(&self.syms[k].kind))
                    .collect();
                if let Some(&m) = local.last() {
                    let out = hi(m, self, "same_file");
                    return if local.len() > 1 {
                        cap(out, "medium")
                    } else {
                        out
                    };
                }
            }
        }
        let top: Vec<usize> = self
            .py_top(c.file, c.callee)
            .into_iter()
            .filter(|&i| is_callable(&self.syms[i].kind) || is_type(&self.syms[i].kind))
            .collect();
        if let Some(&t) = top.last() {
            let out = hi(t, self, "same_file");
            return if top.len() > 1 {
                cap(out, "medium")
            } else {
                out
            };
        }
        match self.py_first(c.callee, c.file, None) {
            PySym::Sym(i, how, sure)
                if is_callable(&self.syms[i].kind) || is_type(&self.syms[i].kind) =>
            {
                if sure {
                    hi(i, self, how)
                } else {
                    medium(i, self, how)
                }
            }
            PySym::External => external("external_known"),
            _ => undecided(),
        }
    }

    /// `r.f()` with `r` bound by no scope: a module's function or class, a
    /// class's method (a static or class method), something from outside.
    pub(super) fn py_name(&self, c: &Call<'_>, recv: &str) -> Outcome {
        match self.py_symbol(recv, c.file, c.src) {
            PySym::Sym(t, how, sure) if is_type(&self.syms[t].kind) => {
                let conf = if sure {
                    self.type_conf(t, how)
                } else {
                    "medium"
                };
                self.in_type(c, t, how, conf)
            }
            PySym::Module(m) => match self.py_export(&m, c.callee, 0) {
                PyExport::Found(i, sure)
                    if is_callable(&self.syms[i].kind) || is_type(&self.syms[i].kind) =>
                {
                    cap(hi(i, self, "import"), if sure { "high" } else { "medium" })
                }
                PyExport::External => external("external_known"),
                _ => undecided(),
            },
            PySym::External => external("external_known"),
            _ => undecided(),
        }
    }

    /// An `imports` row of a Python file: the symbol it names, a module of
    /// the repository, or something from outside.
    pub(super) fn py_import(&self, e: &LinkEdge) -> Outcome {
        let imp = PyImport::from_row(&e.dst_name, e.hint.as_deref());
        let module_only = Outcome::Resolved(Resolved {
            dst_id: None,
            confidence: "high",
            resolution: "import",
            external: false,
        });
        let target = self.py_module(&e.file, &imp.module);
        match (&target, &imp.name) {
            (PyMod::External, _) => external("external_known"),
            (PyMod::Unknown, _) => undecided(),
            (_, None) => module_only,
            (m, Some(n)) => match self.py_export(m, n, 0) {
                PyExport::Found(i, true) => hi(i, self, "import"),
                PyExport::Found(i, false) => medium(i, self, "import"),
                PyExport::Module(_) => module_only,
                PyExport::External => external("external_known"),
                PyExport::Missing | PyExport::Unknown => undecided(),
            },
        }
    }
}

/// Whether the repository directory `dir` contains `d` (itself included).
fn contains(dir: &str, d: &str) -> bool {
    dir.is_empty() || d == dir || d.starts_with(&format!("{dir}/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annotations_read_as_types() {
        let t = |s: &str| type_text(s).map(|t| (t.base, t.elem));
        assert_eq!(t("Optional[Foo]"), Some(("Foo".into(), None)));
        assert_eq!(t("\"Foo\""), Some(("Foo".into(), None)));
        assert_eq!(t("Foo | None"), Some(("Foo".into(), None)));
        assert_eq!(t("list[Bar]"), Some(("list".into(), Some("Bar".into()))));
        assert_eq!(t("mod.Cls"), Some(("mod.Cls".into(), None)));
        assert_eq!(t("typing.Optional[a.B]"), Some(("a.B".into(), None)));
        assert_eq!(t("Union[Foo, None]"), Some(("Foo".into(), None)));
    }

    #[test]
    fn return_types_from_signatures() {
        assert_eq!(
            return_type("def load(self) -> \"Item\":").as_deref(),
            Some("Item")
        );
        assert_eq!(
            return_type("def get_logger(name: str) -> logging.Logger:").as_deref(),
            Some("logging.Logger")
        );
        assert_eq!(return_type("def f(a, b):"), None);
        assert_eq!(
            return_type("async def f() -> Optional[Repo]:").as_deref(),
            Some("Repo")
        );
    }

    #[test]
    fn import_rows_read_back() {
        let i = PyImport::from_row(".models.Line", Some("as L from"));
        assert_eq!(
            (i.module.as_str(), i.name.as_deref(), i.local.as_deref()),
            (".models", Some("Line"), Some("L"))
        );
        let i = PyImport::from_row("..x", Some("from"));
        assert_eq!((i.module.as_str(), i.name.as_deref()), ("..", Some("x")));
        let i = PyImport::from_row("a.b", None);
        assert_eq!((i.module.as_str(), i.local.as_deref()), ("a.b", Some("a")));
        let i = PyImport::from_row("a.b", Some("as c"));
        assert_eq!(i.local.as_deref(), Some("c"));
        let i = PyImport::from_row(".m.*", None);
        assert!(i.wildcard && i.module == ".m");
        let i = PyImport::from_row(".*", None);
        assert!(i.wildcard && i.module == ".");
    }
}
