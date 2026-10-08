//! Rust (PLAN-009 TASK-007, DD-7): what a file declares about itself, how a
//! `use` tree expands, the type a binding takes from its declaration or its
//! value, and the link pass's rules — crates by their `Cargo.toml`, modules
//! by path and inline `mod`, `use` (groups, `self`, aliases, globs, `pub
//! use` re-exports) scoped to the module that declares it, `crate::`,
//! `self::`, `super::` and `Self::`, a type's `impl`s in any file of its
//! crate, `impl Trait for T`, and `std`/`core`/`alloc` and the declared
//! dependencies as external.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use tree_sitter::Node;

use super::link::{
    cap, external, external_medium, hi, is_callable, is_type, undecided, Call, LinkEdge, Outcome,
    RepoIndex, Resolved, TypeRef, ValueType, IMPORT_WEAK,
};
use super::scope::{TypeText, Via};
use super::typescript::join;
use super::{compact, strip_extension, LangResolver};
use crate::facts::ImportFact;

/// Rust.
pub struct Rust;

impl LangResolver for Rust {
    /// `crates/my-crate/src/a/b.rs` → `my_crate::a::b`; `src/lib.rs`,
    /// `main.rs` and `mod.rs` name their directory. A file outside any
    /// `src/` (a test, an example) is its stem.
    fn package_from_path(&self, path: &str) -> Option<String> {
        let stem = strip_extension(path);
        let parts: Vec<&str> = stem.split('/').collect();
        let Some(src) = parts.iter().rposition(|p| *p == "src") else {
            return Some(parts.last().copied().unwrap_or(stem).replace('-', "_"));
        };
        let krate = match src {
            0 => "crate".to_string(),
            i => parts[i - 1].replace('-', "_"),
        };
        let mut module: Vec<&str> = parts[src + 1..].to_vec();
        if matches!(module.last(), Some(&"lib" | &"main" | &"mod")) {
            module.pop();
        }
        let mut out = krate;
        for m in module {
            out.push_str("::");
            out.push_str(m);
        }
        Some(out)
    }

    /// Any `pub` (`pub(crate)` included); an `impl` has no visibility.
    fn exported(&self, def: Node<'_>, _name: &str, _bytes: &[u8]) -> Option<bool> {
        if def.kind() == "impl_item" {
            return None;
        }
        let mut cursor = def.walk();
        let public = def
            .children(&mut cursor)
            .any(|c| c.kind() == "visibility_modifier");
        Some(public)
    }

    fn expand_import_tree(&self, tree: Node<'_>, bytes: &[u8]) -> Vec<ImportFact> {
        let mut out = Vec::new();
        expand_use(tree, "", bytes, &mut out);
        out
    }

    fn import_target(&self, imp: &ImportFact) -> String {
        if imp.wildcard {
            format!("{}::*", imp.path)
        } else {
            imp.path.clone()
        }
    }

    /// A bare name is never a method: `self.m()` and `Self::m()` reach one.
    fn implicit_this(&self) -> bool {
        false
    }

    /// Fields are declared on the type, apart from the `impl`s.
    fn fields_by_link(&self) -> bool {
        true
    }

    fn type_text(&self, node: Node<'_>, bytes: &[u8]) -> Option<TypeText> {
        type_of_node(node, bytes)
    }

    /// `T::f(…)` / `f(…)` is what the call returns (`T.f()`), `T::f(…)?` and
    /// `T::f(…).unwrap()` unwrapped (`T.f()?`); a struct literal is its type;
    /// `vec![…]` a `Vec`, `format!(…)` a `String`.
    fn init_type(&self, init: Node<'_>, bytes: &[u8]) -> Option<(TypeText, Via)> {
        let typed = |base: String| Some((TypeText { base, elem: None }, Via::Local));
        match init.kind() {
            "call_expression" => {
                let f = init.child_by_field_name("function")?;
                // `T::f(…).unwrap()`: the call it unwraps.
                if f.kind() == "field_expression" {
                    let method = f.child_by_field_name("field")?.utf8_text(bytes).ok()?;
                    let value = f.child_by_field_name("value")?;
                    if UNWRAPS.contains(&method) && value.kind() == "call_expression" {
                        let (base, _) = self.init_type(value, bytes)?;
                        return typed(format!("{}?", base.base));
                    }
                    return None;
                }
                callee_path(f, bytes).and_then(|p| typed(format!("{p}()")))
            }
            "try_expression" => {
                let inner = init.named_child(0)?;
                if inner.kind() != "call_expression" {
                    return None;
                }
                let (base, _) = self.init_type(inner, bytes)?;
                if base.base.ends_with("()") {
                    typed(format!("{}?", base.base))
                } else {
                    None
                }
            }
            "await_expression" | "parenthesized_expression" | "reference_expression" => {
                let inner = init
                    .child_by_field_name("value")
                    .or_else(|| init.named_child(0))?;
                self.init_type(inner, bytes)
            }
            "struct_expression" => {
                let name = init.child_by_field_name("name")?;
                let text = name
                    .child_by_field_name("type")
                    .map_or_else(|| compact(name, bytes), |t| compact(t, bytes));
                normalize_path(&text).and_then(typed)
            }
            "macro_invocation" => {
                let m = init.child_by_field_name("macro")?.utf8_text(bytes).ok()?;
                match m {
                    "vec" => typed("Vec".into()),
                    "format" => typed("String".into()),
                    _ => None,
                }
            }
            k => self.literal_type(k).and_then(|t| typed(t.into())),
        }
    }
}

/// Methods that unwrap a `Result`/`Option` (what `?` does, for typing).
pub(super) const UNWRAPS: &[&str] = &[
    "unwrap",
    "expect",
    "unwrap_or_default",
    "unwrap_or",
    "unwrap_or_else",
];

/// Methods any type may have from a derive or a blanket `impl` (`Clone`,
/// `ToString`, `Into`…): missing in a repository type, they are its
/// standard library's (`medium`: nothing declares it).
const BLANKET: &[&str] = &[
    "clone",
    "clone_from",
    "to_owned",
    "to_string",
    "into",
    "try_into",
    "from",
    "try_from",
    "default",
    "eq",
    "ne",
    "cmp",
    "partial_cmp",
    "lt",
    "le",
    "gt",
    "ge",
    "hash",
    "fmt",
    "borrow",
    "borrow_mut",
    "as_ref",
    "as_mut",
    "type_id",
];

/// `prefix::segment`, where `self` names the prefix itself.
fn use_join(prefix: &str, segment: &str) -> String {
    match (prefix.is_empty(), segment) {
        (true, s) => s.to_string(),
        (false, "self") => prefix.to_string(),
        (false, s) => format!("{prefix}::{s}"),
    }
}

/// Flatten a Rust `use` tree: `a::{b::C, d as e, f::*}` is `a::b::C`,
/// `a::d` (alias `e`) and `a::f` (wildcard).
fn expand_use(node: Node<'_>, prefix: &str, bytes: &[u8], out: &mut Vec<ImportFact>) {
    let fact = |path: String, alias: Option<String>, wildcard: bool| ImportFact {
        path,
        alias,
        wildcard,
        ..Default::default()
    };
    match node.kind() {
        "use_as_clause" => {
            let path = node
                .child_by_field_name("path")
                .map(|p| compact(p, bytes))
                .unwrap_or_default();
            let alias = node.child_by_field_name("alias").map(|a| compact(a, bytes));
            out.push(fact(use_join(prefix, &path), alias, false));
        }
        "use_wildcard" => {
            let inner = node
                .named_child(0)
                .map(|p| compact(p, bytes))
                .unwrap_or_default();
            out.push(fact(use_join(prefix, &inner), None, true));
        }
        "use_list" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                expand_use(child, prefix, bytes, out);
            }
        }
        "scoped_use_list" => {
            let path = node
                .child_by_field_name("path")
                .map(|p| compact(p, bytes))
                .unwrap_or_default();
            let prefix = use_join(prefix, &path);
            if let Some(list) = node.child_by_field_name("list") {
                expand_use(list, &prefix, bytes, out);
            }
        }
        k if k.contains("comment") => {}
        _ => out.push(fact(use_join(prefix, &compact(node, bytes)), None, false)),
    }
}

/// A path as written (`crate::a::Foo`, `Vec::<u8>`, `Self`), without
/// generic arguments, its segments joined by `.`; `None` for one that is no
/// plain run of names (`<T as Trait>`).
pub fn normalize_path(text: &str) -> Option<String> {
    let mut flat = String::with_capacity(text.len());
    let mut depth = 0usize;
    for c in text.chars().filter(|c| !c.is_whitespace()) {
        match c {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => flat.push(c),
            _ => {}
        }
    }
    let flat = flat.trim_end_matches("::");
    let segs: Vec<&str> = flat.split("::").collect();
    let ok = !segs.is_empty()
        && segs.iter().all(|s| {
            let mut c = s.chars();
            c.next().is_some_and(|f| f.is_alphabetic() || f == '_')
                && c.all(|ch| ch.is_alphanumeric() || ch == '_')
        });
    ok.then(|| segs.join("."))
}

/// The callee of a call as a path (`Store::open` → `Store.open`, `f` →
/// `f`, `f::<T>` → `f`).
fn callee_path(f: Node<'_>, bytes: &[u8]) -> Option<String> {
    match f.kind() {
        "identifier" | "scoped_identifier" => normalize_path(&compact(f, bytes)),
        "generic_function" => callee_path(f.child_by_field_name("function")?, bytes),
        _ => None,
    }
}

/// Smart pointers whose methods are their content's (auto-deref).
const POINTERS: &[&str] = &["Box", "Rc", "Arc", "Cow"];

/// A type as written in a declaration: no reference, pointer, lifetime,
/// `mut`, `dyn` or `impl`; a smart pointer is its content; a type parameter
/// is its first bound (`T: Store` → `Store`); a path joined by `.`; generic
/// arguments as the element.
fn type_of_node(node: Node<'_>, bytes: &[u8]) -> Option<TypeText> {
    match node.kind() {
        "reference_type" | "pointer_type" => type_of_node(node.child_by_field_name("type")?, bytes),
        "abstract_type" | "dynamic_type" => type_of_node(node.child_by_field_name("trait")?, bytes),
        "bounded_type" => type_of_node(node.named_child(0)?, bytes),
        "generic_type" => {
            let base = node.child_by_field_name("type")?;
            let path = normalize_path(&compact(base, bytes))?;
            let args = node.child_by_field_name("type_arguments");
            let first = args.and_then(|a| {
                let mut c = a.walk();
                let found = a
                    .named_children(&mut c)
                    .find(|n| !matches!(n.kind(), "lifetime" | "comment"));
                found
            });
            let last = path.rsplit('.').next().unwrap_or(&path);
            if POINTERS.contains(&last) {
                return type_of_node(first?, bytes);
            }
            Some(TypeText {
                base: path,
                elem: first.and_then(|f| type_of_node(f, bytes)).map(|t| t.base),
            })
        }
        "type_identifier" => {
            let name = node.utf8_text(bytes).ok()?;
            if let Some(bound) = type_param_bound(node, name, bytes) {
                return type_of_node(bound, bytes);
            }
            if declares_type_param(node, name, bytes) {
                return None;
            }
            Some(TypeText {
                base: name.to_string(),
                elem: None,
            })
        }
        "scoped_type_identifier" | "primitive_type" => Some(TypeText {
            base: normalize_path(&compact(node, bytes))?,
            elem: None,
        }),
        _ => None,
    }
}

/// The first trait bound of the type parameter `name` declared by an item
/// around `node` (`<T: Store>`, `where T: Store`).
fn type_param_bound<'t>(node: Node<'t>, name: &str, bytes: &[u8]) -> Option<Node<'t>> {
    let mut cur = node.parent();
    while let Some(n) = cur {
        if let Some(params) = n.child_by_field_name("type_parameters") {
            let mut c = params.walk();
            for p in params.named_children(&mut c) {
                let pname = p
                    .child_by_field_name("name")
                    .or_else(|| p.child_by_field_name("left"))
                    .and_then(|x| x.utf8_text(bytes).ok());
                if pname == Some(name) {
                    if let Some(b) = p.child_by_field_name("bounds") {
                        let mut bc = b.walk();
                        let first = b.named_children(&mut bc).find(|x| x.kind() != "lifetime");
                        return first;
                    }
                }
            }
        }
        let mut c = n.walk();
        let where_clause = n
            .named_children(&mut c)
            .find(|x| x.kind() == "where_clause");
        if let Some(w) = where_clause {
            let mut wc = w.walk();
            for pred in w.named_children(&mut wc) {
                let left = pred
                    .child_by_field_name("left")
                    .and_then(|x| x.utf8_text(bytes).ok());
                if left == Some(name) {
                    let b = pred.child_by_field_name("bounds")?;
                    let mut bc = b.walk();
                    let first = b.named_children(&mut bc).find(|x| x.kind() != "lifetime");
                    return first;
                }
            }
        }
        cur = n.parent();
    }
    None
}

/// Whether an item around `node` declares the type parameter `name`.
fn declares_type_param(node: Node<'_>, name: &str, bytes: &[u8]) -> bool {
    let mut cur = node.parent();
    while let Some(n) = cur {
        if let Some(params) = n.child_by_field_name("type_parameters") {
            let mut c = params.walk();
            let found = params.named_children(&mut c).any(|p| {
                p.child_by_field_name("name")
                    .and_then(|x| x.utf8_text(bytes).ok())
                    == Some(name)
            });
            if found {
                return true;
            }
        }
        cur = n.parent();
    }
    false
}

/// A type as a signature writes it (a field's, a return type), reduced the
/// same way from text: `&'a mut Foo`, `Arc<dyn Foo>`, `impl Foo` → `Foo`;
/// `Result<Self>` → `Result` holding `Self`.
pub fn type_of_text(text: &str) -> Option<TypeText> {
    let mut t: &str = text.trim();
    loop {
        let next = t
            .strip_prefix('&')
            .or_else(|| t.strip_prefix('*'))
            .or_else(|| t.strip_prefix("mut "))
            .or_else(|| t.strip_prefix("const "))
            .or_else(|| t.strip_prefix("dyn "))
            .or_else(|| t.strip_prefix("impl "))
            .or_else(|| {
                let rest = t.strip_prefix('\'')?;
                let end = rest
                    .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                    .unwrap_or(rest.len());
                Some(&rest[end..])
            })
            .map(str::trim_start);
        match next {
            Some(rest) => t = rest,
            None => break,
        }
    }
    // `dyn A + Send`: the first bound.
    let t = top_level_first(t, '+').trim();
    let (head, args) = match t.find('<') {
        Some(i) => (&t[..i], Some(&t[i + 1..t.rfind('>')?])),
        None => (t, None),
    };
    let base = normalize_path(head)?;
    let first = args.map(|a| top_level_first(a, ','));
    let last = base.rsplit('.').next().unwrap_or(&base);
    if POINTERS.contains(&last) {
        return type_of_text(first?);
    }
    Some(TypeText {
        elem: first.and_then(type_of_text).map(|e| e.base),
        base,
    })
}

/// `text` up to the first `sep` outside `<…>` and `(…)`.
fn top_level_first(text: &str, sep: char) -> &str {
    let mut depth = 0usize;
    for (i, c) in text.char_indices() {
        match c {
            '<' | '(' | '[' => depth += 1,
            '>' | ')' | ']' => depth = depth.saturating_sub(1),
            c if c == sep && depth == 0 => return &text[..i],
            _ => {}
        }
    }
    text
}

/// The return type a Rust signature declares (`pub fn open(p: &Path) ->
/// Result<Self>` → `Result` holding `Self`); `None` when it declares none.
pub fn return_type(signature: &str) -> Option<TypeText> {
    let open = signature.find('(')?;
    let mut depth = 0usize;
    let mut close = None;
    for (i, c) in signature[open..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(open + i);
                    break;
                }
            }
            _ => {}
        }
    }
    let rest = signature[close? + 1..].trim();
    let ty = rest.strip_prefix("->")?;
    let ty = ty.split(" where").next().unwrap_or(ty);
    let ty = ty.trim().trim_end_matches(';').trim();
    type_of_text(ty)
}

/// A field's declared type (`pub items: HashMap<String, u32>`).
pub fn field_type(signature: &str) -> Option<TypeText> {
    let (_, ty) = signature.split_once(':')?;
    type_of_text(ty.trim().trim_end_matches(','))
}

/// `T` unwrapped once by `?` (a `Result`/`Option`): its first argument.
pub fn unwrapped(t: TypeText) -> Option<TypeText> {
    let last = t.base.rsplit('.').next().unwrap_or(&t.base);
    if matches!(last, "Result" | "Option") {
        return type_of_text(&t.elem?);
    }
    Some(t)
}

// ------------------------------------------------------------- the link pass

/// What a module is made of.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RsMod {
    /// A file (its top-level items).
    File(String),
    /// An inline `mod x { … }` (its children).
    Inline(usize),
}

/// One name a `use` brings in, scoped to the module that declares it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RsUse {
    segs: Vec<String>,
    /// The name it binds; `None` for a glob.
    local: Option<String>,
    glob: bool,
    reexport: bool,
    /// The inline `mod` it is in; `None`: the file's own module.
    scope: Option<usize>,
}

/// What a path reaches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RsRes {
    /// A symbol, and whether surely.
    Item(usize, bool),
    /// A module, by path (`krate::a::b`).
    Module(String),
    /// From outside the repository, with evidence.
    External,
    /// Followed, not there.
    Missing,
    /// Nothing says.
    Unknown,
}

/// The modules and `use`s of a branch's Rust files.
#[derive(Default)]
pub struct RsIndex {
    /// File → its module path.
    file_module: HashMap<String, String>,
    /// Inline `mod` symbol → its module path.
    inline_path: HashMap<usize, String>,
    /// Module path → what it is made of.
    modules: HashMap<String, Vec<RsMod>>,
    /// File → its `use`s.
    uses: HashMap<String, Vec<RsUse>>,
    /// The workspace's crates (code names).
    krates: HashSet<String>,
    items: Mutex<HashMap<(String, String, u8), RsRes>>,
}

/// How many `use`s deep a name is followed.
const USE_DEPTH: u8 = 4;

impl RepoIndex {
    /// The crate manifest a Rust file belongs to: the deepest with a
    /// `[package]` above it.
    fn rs_manifest(&self, file: &str) -> Option<&crate::resolve::env::CargoManifest> {
        self.cargo
            .iter()
            .filter(|m| m.krate.is_some())
            .filter(|m| m.dir.is_empty() || file.starts_with(&format!("{}/", m.dir)))
            .max_by_key(|m| m.dir.len())
    }

    /// The module path of a Rust file: `krate::a::b` under its crate's
    /// `src/` (`lib.rs`, `main.rs`, `mod.rs` naming their directory); a
    /// test, an example, a bench or a `src/bin/` file is a crate root of its
    /// own (`krate#path`). Without a manifest, the path-derived package.
    fn rs_module_of_file(&self, file: &str) -> String {
        let Some(m) = self.rs_manifest(file) else {
            return self
                .files
                .get(file)
                .and_then(|f| f.package.clone())
                .unwrap_or_else(|| format!("crate#{file}"));
        };
        let krate = m.krate.clone().unwrap_or_default();
        let rel = if m.dir.is_empty() {
            file
        } else {
            &file[m.dir.len() + 1..]
        };
        let stem = strip_extension(rel);
        let Some(inner) = stem.strip_prefix("src/") else {
            return format!("{krate}#{file}");
        };
        if inner.starts_with("bin/") {
            return format!("{krate}#{file}");
        }
        let mut segs: Vec<&str> = inner.split('/').collect();
        if matches!(segs.last(), Some(&"lib" | &"main" | &"mod")) {
            segs.pop();
        }
        let mut out = krate;
        for s in segs {
            out.push_str("::");
            out.push_str(s);
        }
        out
    }

    /// Index the Rust files' modules and `use`s, give every `impl` in a file
    /// other than its type's (or with no type beside it) its type as owner,
    /// and make it a child of that type (DD-6: what a type's methods are).
    pub(super) fn rs_build(&mut self, facts: &[LinkEdge]) {
        let rust: Vec<String> = self
            .files
            .iter()
            .filter(|(_, f)| f.lang.is_some_and(|l| l.key() == "rust"))
            .map(|(k, _)| k.clone())
            .collect();
        if rust.is_empty() {
            return;
        }
        let mut rs = RsIndex {
            krates: self.cargo.iter().filter_map(|m| m.krate.clone()).collect(),
            ..Default::default()
        };
        for f in &rust {
            let m = self.rs_module_of_file(f);
            rs.modules
                .entry(m.clone())
                .or_default()
                .push(RsMod::File(f.clone()));
            rs.file_module.insert(f.clone(), m);
        }
        // Inline modules, outermost first (a parent's path before its
        // children's).
        let mut inline: Vec<usize> = (0..self.syms.len())
            .filter(|&i| {
                self.syms[i].kind == "module"
                    && rs.file_module.contains_key(&self.syms[i].file)
                    && !self.syms[i]
                        .signature
                        .as_deref()
                        .is_some_and(|s| s.trim_end().ends_with(';'))
            })
            .collect();
        inline.sort_by_key(|&i| self.chain(Some(i)).len());
        for i in inline {
            let s = &self.syms[i];
            let parent = self
                .chain(Some(i))
                .into_iter()
                .skip(1)
                .find_map(|a| rs.inline_path.get(&a).cloned())
                .unwrap_or_else(|| rs.file_module[&s.file].clone());
            let path = format!("{parent}::{}", s.name);
            rs.modules
                .entry(path.clone())
                .or_default()
                .push(RsMod::Inline(i));
            rs.inline_path.insert(i, path);
        }
        for e in facts.iter().filter(|e| e.kind == crate::facts::IMPORTS) {
            if !rs.file_module.contains_key(&e.file) {
                continue;
            }
            let tokens: Vec<&str> = e
                .hint
                .as_deref()
                .unwrap_or_default()
                .split_whitespace()
                .collect();
            let alias = tokens
                .iter()
                .position(|t| *t == "as")
                .and_then(|i| tokens.get(i + 1))
                .map(|a| a.to_string());
            let (path, glob) = match e.dst_name.strip_suffix("::*") {
                Some(p) => (p, true),
                None => (e.dst_name.as_str(), false),
            };
            let segs: Vec<String> = path.split("::").map(str::to_string).collect();
            let local = if glob {
                None
            } else {
                alias.or_else(|| segs.last().cloned())
            };
            // The innermost inline `mod` around the `use`.
            let scope = rs
                .inline_path
                .keys()
                .copied()
                .filter(|&m| {
                    let s = &self.syms[m];
                    s.file == e.file && s.start_line <= e.line && e.line <= s.end_line
                })
                .max_by_key(|&m| self.syms[m].start_line);
            rs.uses.entry(e.file.clone()).or_default().push(RsUse {
                segs,
                local,
                glob,
                reexport: tokens.first() == Some(&"export"),
                scope,
            });
        }
        self.rs = rs;
        // Owners of `impl`s whose type is not their parent.
        let impls: Vec<usize> = (0..self.syms.len())
            .filter(|&i| self.syms[i].kind == "impl")
            .filter(|&i| {
                !self.syms[i]
                    .parent_id
                    .and_then(|p| self.by_id.get(&p))
                    .is_some_and(|&p| is_type(&self.syms[p].kind))
            })
            .filter(|&i| self.rs.file_module.contains_key(&self.syms[i].file))
            .collect();
        for i in impls {
            let name = self.syms[i].qualified.clone();
            let file = self.syms[i].file.clone();
            let owner = match self.rs_type(&name, &file, Some(i)) {
                TypeRef::Repo(t, _) => Some(t),
                // Rust puts an inherent `impl` in its type's crate: a type
                // of that name, alone in the crate.
                _ => {
                    let krate = crate_root(&self.rs.file_module[&file]).to_string();
                    let found: Vec<usize> = self
                        .by_name
                        .get(&name)
                        .into_iter()
                        .flatten()
                        .copied()
                        .filter(|&t| is_type(&self.syms[t].kind))
                        .filter(|&t| {
                            self.rs
                                .file_module
                                .get(&self.syms[t].file)
                                .is_some_and(|m| crate_root(m) == krate)
                        })
                        .collect();
                    (found.len() == 1).then(|| found[0])
                }
            };
            if let Some(t) = owner {
                self.owners.insert(i, t);
                let id = self.syms[t].id;
                self.add_child(id, i);
            }
        }
    }

    /// The module the symbol `ctx` of `file` is in: its innermost inline
    /// `mod`, or the file's; and that `mod`.
    fn rs_module_at(&self, file: &str, ctx: Option<usize>) -> (String, Option<usize>) {
        for a in self.chain(ctx) {
            if let Some(p) = self.rs.inline_path.get(&a) {
                return (p.clone(), Some(a));
            }
        }
        (
            self.rs
                .file_module
                .get(file)
                .cloned()
                .unwrap_or_else(|| format!("crate#{file}")),
            None,
        )
    }

    /// Whether `name` names a crate from outside, as `file` sees it: `std`,
    /// `core`, `alloc`…, or a dependency its crate's manifest declares.
    fn rs_external_crate(&self, file: &str, name: &str) -> bool {
        if self
            .lang_of(file)
            .is_some_and(|l| l.def().platform_modules.iter().any(|m| m == name))
        {
            return true;
        }
        self.rs_manifest(file)
            .into_iter()
            .chain(self.cargo.iter().filter(|m| m.krate.is_none()))
            .any(|m| m.deps.binary_search_by(|d| d.as_str().cmp(name)).is_ok())
    }

    /// What module `module` has as `name`: an item, a child module, a name a
    /// `use` of it brings in (a re-export), a glob's.
    fn rs_item(&self, module: &str, name: &str, depth: u8) -> RsRes {
        let key = (module.to_string(), name.to_string(), depth);
        if let Some(r) = self.rs.items.lock().ok().and_then(|c| c.get(&key).cloned()) {
            return r;
        }
        let r = self.rs_item_uncached(module, name, depth);
        if let Ok(mut c) = self.rs.items.lock() {
            c.insert(key, r.clone());
        }
        r
    }

    fn rs_item_uncached(&self, module: &str, name: &str, depth: u8) -> RsRes {
        if depth > USE_DEPTH {
            return RsRes::Unknown;
        }
        let parts = self.rs.modules.get(module).cloned().unwrap_or_default();
        let mut found: Vec<usize> = Vec::new();
        for p in &parts {
            let items: Vec<usize> = match p {
                RsMod::File(f) => match self.files.get(f).and_then(|i| i.id) {
                    Some(fid) => self.children_of(fid).collect(),
                    None => Vec::new(),
                },
                RsMod::Inline(m) => self.children_of(self.syms[*m].id).collect(),
            };
            found.extend(
                items
                    .into_iter()
                    .filter(|&i| self.syms[i].name == name && self.syms[i].kind != "impl"),
            );
        }
        if let Some(&i) = found.iter().find(|&&i| self.syms[i].kind != "module") {
            return RsRes::Item(i, true);
        }
        let child = format!("{module}::{name}");
        if !found.is_empty() || self.rs.modules.contains_key(&child) {
            return RsRes::Module(child);
        }
        let (mut globbed, mut ext, mut unk) = (Vec::new(), false, false);
        for p in &parts {
            let (file, scope) = match p {
                RsMod::File(f) => (f.as_str(), None),
                RsMod::Inline(m) => (self.syms[*m].file.as_str(), Some(*m)),
            };
            let uses: Vec<&RsUse> = self
                .rs
                .uses
                .get(file)
                .into_iter()
                .flatten()
                .filter(|u| u.scope == scope)
                .collect();
            if let Some(u) = uses.iter().rev().find(|u| u.local.as_deref() == Some(name)) {
                return self.rs_path_in(&u.segs, module, file, scope, depth + 1);
            }
            for u in uses.iter().filter(|u| u.glob) {
                match self.rs_path_in(&u.segs, module, file, scope, depth + 1) {
                    RsRes::Module(m) => match self.rs_item(&m, name, depth + 1) {
                        RsRes::Item(i, sure) => globbed.push((i, sure)),
                        RsRes::External => ext = true,
                        RsRes::Unknown => unk = true,
                        _ => {}
                    },
                    RsRes::External => ext = true,
                    RsRes::Unknown => unk = true,
                    _ => {}
                }
            }
        }
        match globbed.as_slice() {
            [(i, sure)] => RsRes::Item(*i, *sure && !ext && !unk),
            [(i, _), ..] => RsRes::Item(*i, false),
            [] if ext || unk => RsRes::Unknown,
            [] => RsRes::Missing,
        }
    }

    /// A path written in `module` (of `file`, inside the inline `mod`
    /// `scope`): `crate::`, `self::`, `super::`, an item or child module of
    /// `module`, a name its `use`s bring in, a workspace crate, a crate from
    /// outside.
    fn rs_path_in(
        &self,
        segs: &[String],
        module: &str,
        file: &str,
        _scope: Option<usize>,
        depth: u8,
    ) -> RsRes {
        if depth > USE_DEPTH || segs.is_empty() {
            return RsRes::Unknown;
        }
        let first = segs[0].as_str();
        let mut cur = match first {
            "crate" => RsRes::Module(crate_root(module).to_string()),
            "self" => RsRes::Module(module.to_string()),
            "super" => match module.rsplit_once("::") {
                Some((parent, _)) => RsRes::Module(parent.to_string()),
                None => RsRes::Unknown,
            },
            name => match self.rs_item(module, name, depth) {
                RsRes::Missing | RsRes::Unknown => {
                    if self.rs.krates.contains(name) && self.rs.modules.contains_key(name) {
                        RsRes::Module(name.to_string())
                    } else if self.rs_external_crate(file, name) || self.platform(file, name) {
                        RsRes::External
                    } else {
                        RsRes::Unknown
                    }
                }
                r => r,
            },
        };
        for seg in &segs[1..] {
            if seg == "super" {
                cur = match cur {
                    RsRes::Module(m) => match m.rsplit_once("::") {
                        Some((parent, _)) => RsRes::Module(parent.to_string()),
                        None => RsRes::Unknown,
                    },
                    other => other,
                };
                continue;
            }
            cur = self.rs_step(cur, seg, depth);
        }
        cur
    }

    /// One more segment of a path.
    fn rs_step(&self, cur: RsRes, seg: &str, depth: u8) -> RsRes {
        match cur {
            RsRes::Module(m) => self.rs_item(&m, seg, depth),
            RsRes::Item(t, sure) if is_type(&self.syms[t].kind) => {
                let found: Vec<usize> = self
                    .children_of(self.syms[t].id)
                    .flat_map(|c| {
                        if self.syms[c].kind == "impl" {
                            self.children_of(self.syms[c].id).collect::<Vec<_>>()
                        } else {
                            vec![c]
                        }
                    })
                    .filter(|&m| self.syms[m].name == seg)
                    .collect();
                match found.first() {
                    Some(&m) => RsRes::Item(m, sure),
                    None => RsRes::Missing,
                }
            }
            RsRes::Item(..) | RsRes::Missing => RsRes::Missing,
            RsRes::External => RsRes::External,
            RsRes::Unknown => RsRes::Unknown,
        }
    }

    /// A path as written at the symbol `ctx` of `file` (`Self::` is the type
    /// of the `impl` around it).
    fn rs_path(&self, segs: &[String], file: &str, ctx: Option<usize>) -> RsRes {
        if segs.first().map(String::as_str) == Some("Self") {
            let Some(&t) = self.containers(ctx).first() else {
                return RsRes::Unknown;
            };
            let mut cur = RsRes::Item(t, true);
            for seg in &segs[1..] {
                cur = self.rs_step(cur, seg, 0);
            }
            return cur;
        }
        let (module, scope) = self.rs_module_at(file, ctx);
        self.rs_path_in(segs, &module, file, scope, 0)
    }

    /// How a symbol reached from `file` is labelled.
    fn rs_how(&self, i: usize, file: &str, self_path: bool) -> &'static str {
        if self_path {
            "self"
        } else if self.syms[i].file == file {
            "same_file"
        } else {
            "import"
        }
    }

    /// A type as written in a Rust file (`Foo`, `a.b.Foo`, `a::Foo`,
    /// `Self`), seen from `ctx`.
    pub(super) fn rs_type(&self, name: &str, file: &str, ctx: Option<usize>) -> TypeRef {
        let segs = split_path(name);
        let self_path = segs.first().map(String::as_str) == Some("Self");
        match self.rs_path(&segs, file, ctx) {
            RsRes::Item(t, sure) if is_type(&self.syms[t].kind) => {
                let how = self.rs_how(t, file, self_path);
                TypeRef::Repo(t, if sure { how } else { IMPORT_WEAK })
            }
            RsRes::External => TypeRef::External,
            _ => TypeRef::Unknown,
        }
    }

    /// A member missing from a repository type: a derive's or a blanket
    /// `impl`'s method is the standard library's (`medium`); else
    /// undecided.
    pub(super) fn rs_missing(&self, callee: &str) -> Outcome {
        if BLANKET.contains(&callee) {
            external_medium("inherited")
        } else {
            undecided()
        }
    }

    /// `P::f()` (hint `path P`): a type's associated function (an enum's
    /// variant: the enum), a module's function, something from outside.
    pub(super) fn rs_path_call(&self, c: &Call<'_>, prefix: &str) -> Outcome {
        let segs = split_path(prefix);
        let self_path = segs.first().map(String::as_str) == Some("Self");
        match self.rs_path(&segs, c.file, c.src) {
            RsRes::Item(t, sure) if is_type(&self.syms[t].kind) => {
                let how = self.rs_how(t, c.file, self_path);
                let conf = if sure {
                    self.type_conf(t, how)
                } else {
                    "medium"
                };
                let variant = self.syms[t].kind == "enum"
                    && c.callee.chars().next().is_some_and(char::is_uppercase);
                if variant {
                    return cap(hi(t, self, how), conf);
                }
                self.in_type(c, t, how, conf)
            }
            RsRes::Module(m) => match self.rs_item(&m, c.callee, 0) {
                RsRes::Item(i, sure)
                    if is_callable(&self.syms[i].kind) || is_type(&self.syms[i].kind) =>
                {
                    let how = self.rs_how(i, c.file, false);
                    cap(hi(i, self, how), if sure { "high" } else { "medium" })
                }
                RsRes::External => external("external_known"),
                _ => undecided(),
            },
            RsRes::External => external("external_known"),
            _ => undecided(),
        }
    }

    /// `f()` no enclosing function binds (hint `free`): an item of the
    /// current module (a tuple struct: the type), a name a `use` brings in,
    /// a glob's, the prelude. Nothing visible: undecided, never a homonym.
    pub(super) fn rs_free(&self, c: &Call<'_>) -> Outcome {
        let (module, _) = self.rs_module_at(c.file, c.src);
        match self.rs_item(&module, c.callee, 0) {
            RsRes::Item(i, sure)
                if is_callable(&self.syms[i].kind) || is_type(&self.syms[i].kind) =>
            {
                let how = self.rs_how(i, c.file, false);
                cap(hi(i, self, how), if sure { "high" } else { "medium" })
            }
            RsRes::External => external("external_known"),
            RsRes::Missing if self.platform(c.file, c.callee) => external("external_known"),
            _ => undecided(),
        }
    }

    /// `f()` bound to a function nested in an enclosing one (hint `bare`).
    pub(super) fn rs_bare(&self, c: &Call<'_>) -> Outcome {
        for anc in self.chain(c.src) {
            let s = &self.syms[anc];
            if is_type(&s.kind) || s.kind == devctx_core::symbol_id::FILE_KIND {
                continue;
            }
            let local: Vec<usize> = self
                .children_of(s.id)
                .filter(|&k| self.syms[k].name == c.callee && is_callable(&self.syms[k].kind))
                .collect();
            if let Some(&m) = local.first() {
                let out = hi(m, self, "same_file");
                return if local.len() > 1 {
                    cap(out, "medium")
                } else {
                    out
                };
            }
        }
        self.rs_free(c)
    }

    /// `r.f()` with `r` bound by no scope: a `static`/`const` (by its
    /// declared type), a unit struct, something from outside.
    pub(super) fn rs_name(&self, c: &Call<'_>, recv: &str) -> Outcome {
        let segs = split_path(recv);
        match self.rs_path(&segs, c.file, c.src) {
            RsRes::Item(t, sure) if is_type(&self.syms[t].kind) => {
                let how = self.rs_how(t, c.file, false);
                self.in_type(c, t, how, if sure { "high" } else { "medium" })
            }
            RsRes::Item(k, sure) if self.syms[k].kind == "const" => {
                let ks = &self.syms[k];
                let Some(ty) = ks.signature.as_deref().and_then(field_type) else {
                    return self.untyped(c);
                };
                match self.rs_type(&ty.base, &ks.file, Some(k)) {
                    TypeRef::Repo(t, how) => {
                        let conf = if sure {
                            self.type_conf(t, how)
                        } else {
                            "medium"
                        };
                        self.in_type(c, t, "import", conf)
                    }
                    TypeRef::External => external("external_known"),
                    TypeRef::Unknown => self.untyped(c),
                }
            }
            RsRes::External => external("external_known"),
            _ => self.untyped(c),
        }
    }

    /// What calling `path` returns (`Store.open`, `make`): a function's
    /// declared return type (`Self` its `impl`'s, unwrapped once by `?`), a
    /// tuple struct; an associated function of a type from outside its type
    /// (`HashMap::new()`, `medium`), any other call from outside a value
    /// nothing types.
    pub(super) fn rs_value(
        &self,
        path: &str,
        unwrap: bool,
        file: &str,
        ctx: Option<usize>,
    ) -> ValueType {
        let segs = split_path(path);
        let res = if segs.len() == 1 {
            let (module, _) = self.rs_module_at(file, ctx);
            self.rs_item(&module, &segs[0], 0)
        } else {
            self.rs_path(&segs, file, ctx)
        };
        match res {
            RsRes::Item(f, sure) if is_callable(&self.syms[f].kind) => {
                let fs = &self.syms[f];
                let Some(ret) = fs.signature.as_deref().and_then(return_type) else {
                    return ValueType::Unknown;
                };
                let ret = if unwrap {
                    match unwrapped(ret) {
                        Some(t) => t,
                        None => return ValueType::Unknown,
                    }
                } else {
                    ret
                };
                let conf = if sure { "high" } else { "medium" };
                match self.rs_type(&ret.base, &fs.file, Some(f)) {
                    TypeRef::Repo(t, how) => ValueType::Repo(
                        t,
                        "return_type",
                        super::link::min_conf(conf, self.type_conf(t, how)),
                    ),
                    TypeRef::External => ValueType::External(conf),
                    TypeRef::Unknown => ValueType::Unknown,
                }
            }
            RsRes::Item(t, sure) if is_type(&self.syms[t].kind) => {
                let how = self.rs_how(t, file, false);
                ValueType::Repo(t, how, if sure { "high" } else { "medium" })
            }
            RsRes::External => {
                let typeish = segs.len() >= 2
                    && segs[segs.len() - 2]
                        .chars()
                        .next()
                        .is_some_and(char::is_uppercase);
                if typeish && !unwrap {
                    ValueType::External("medium")
                } else {
                    ValueType::FromExternalCall
                }
            }
            _ => ValueType::Unknown,
        }
    }

    /// An `imports` row of a Rust file (a `use`): the item it names, a
    /// module, something from outside.
    pub(super) fn rs_import(&self, e: &LinkEdge) -> Outcome {
        let Some(u) = self.rs_use_of(e) else {
            return undecided();
        };
        let module_only = Outcome::Resolved(Resolved {
            dst_id: None,
            confidence: "high",
            resolution: "import",
            external: false,
        });
        let (module, scope) = match u.scope {
            Some(m) => (self.rs.inline_path[&m].clone(), Some(m)),
            None => (
                self.rs
                    .file_module
                    .get(&e.file)
                    .cloned()
                    .unwrap_or_default(),
                None,
            ),
        };
        match self.rs_path_in(&u.segs, &module, &e.file, scope, 0) {
            RsRes::Item(i, true) => hi(i, self, "import"),
            RsRes::Item(i, false) => cap(hi(i, self, "import"), "medium"),
            RsRes::Module(_) => module_only,
            RsRes::External => external("external_known"),
            RsRes::Missing | RsRes::Unknown => undecided(),
        }
    }

    /// The `use` an `imports` row stands for.
    fn rs_use_of(&self, e: &LinkEdge) -> Option<RsUse> {
        let (path, glob) = match e.dst_name.strip_suffix("::*") {
            Some(p) => (p, true),
            None => (e.dst_name.as_str(), false),
        };
        let segs: Vec<String> = path.split("::").map(str::to_string).collect();
        self.rs
            .uses
            .get(&e.file)?
            .iter()
            .find(|u| u.segs == segs && u.glob == glob)
            .cloned()
    }

    /// Every path the `use`s of a Rust file may name (each prefix as a
    /// module: its files and where a new one would go), and whether it
    /// re-exports (`pub use`). `None` for a file with no `use`.
    pub(super) fn rs_file_reach(&self, file: &str) -> Option<(Vec<String>, bool)> {
        let uses = self.rs.uses.get(file)?;
        if uses.is_empty() {
            return None;
        }
        let mut out: Vec<String> = Vec::new();
        for u in uses {
            let module = match u.scope {
                Some(m) => self.rs.inline_path.get(&m).cloned().unwrap_or_default(),
                None => self.rs.file_module.get(file).cloned().unwrap_or_default(),
            };
            let mut bases: Vec<String> = Vec::new();
            let first = u.segs[0].as_str();
            match first {
                "crate" => bases.push(crate_root(&module).to_string()),
                "self" => bases.push(module.clone()),
                "super" => bases.push(
                    module
                        .rsplit_once("::")
                        .map_or(module.clone(), |(p, _)| p.to_string()),
                ),
                name => {
                    bases.push(format!("{module}::{name}"));
                    bases.push(name.to_string());
                    bases.push(module.clone());
                }
            }
            let rest = &u.segs[1..];
            for b in bases {
                let mut m = b;
                out.extend(self.rs_module_files(&m));
                for seg in rest {
                    m = format!("{m}::{seg}");
                    out.extend(self.rs_module_files(&m));
                }
            }
        }
        out.sort();
        out.dedup();
        Some((out, uses.iter().any(|u| u.reexport)))
    }

    /// The files a module path is made of, and where a file for it would
    /// go (`src/a/b.rs`, `src/a/b/mod.rs`).
    fn rs_module_files(&self, module: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for p in self.rs.modules.get(module).into_iter().flatten() {
            out.push(match p {
                RsMod::File(f) => f.clone(),
                RsMod::Inline(m) => self.syms[*m].file.clone(),
            });
        }
        let mut segs = module.split("::");
        let krate = segs.next().unwrap_or_default();
        if let Some(m) = self
            .cargo
            .iter()
            .find(|m| m.krate.as_deref() == Some(krate))
        {
            let src = join(&m.dir, "src");
            let rest: Vec<&str> = segs.collect();
            if rest.is_empty() {
                out.push(join(&src, "lib.rs"));
                out.push(join(&src, "main.rs"));
            } else {
                let p = join(&src, &rest.join("/"));
                out.push(format!("{p}.rs"));
                out.push(join(&p, "mod.rs"));
            }
        }
        out
    }
}

/// A path written with `.` or `::` as its segments.
fn split_path(path: &str) -> Vec<String> {
    path.replace("::", ".")
        .split('.')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// The crate a module path is in (`krate::a::b` → `krate`).
fn crate_root(module: &str) -> &str {
    module.split("::").next().unwrap_or(module)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_types_read_back() {
        let t = |s: &str| return_type(s).map(|t| (t.base, t.elem));
        assert_eq!(
            t("pub fn open(path: &Path) -> Result<Self>"),
            Some(("Result".into(), Some("Self".into())))
        );
        assert_eq!(
            t("fn get(&self) -> Result<Arc<dyn EmbeddingProvider>>"),
            Some(("Result".into(), Some("EmbeddingProvider".into())))
        );
        assert_eq!(
            t("pub fn resolver_for(lang: Lang) -> &'static dyn LangResolver"),
            Some(("LangResolver".into(), None))
        );
        assert_eq!(t("fn f()"), None);
        assert_eq!(
            t("fn g<T>(x: T) -> std::io::Result<Vec<u8>> where T: Copy"),
            Some(("std.io.Result".into(), Some("Vec".into())))
        );
        let f = field_type("pub embedder: Arc<dyn EmbeddingProvider>").unwrap();
        assert_eq!(f.base, "EmbeddingProvider");
        let f = field_type("store: &'a Store").unwrap();
        assert_eq!(f.base, "Store");
        assert_eq!(normalize_path("Vec::<u8>").as_deref(), Some("Vec"));
        assert_eq!(normalize_path("<T as Tr>::f"), None);
        assert_eq!(normalize_path("crate::a::B").as_deref(), Some("crate.a.B"));
    }
}
