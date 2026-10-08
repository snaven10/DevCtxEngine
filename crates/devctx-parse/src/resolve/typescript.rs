//! TypeScript, TSX and JavaScript (PLAN-009 TASK-006, DD-7): how a module
//! specifier names a file of the repository (relative paths, `tsconfig`
//! `baseUrl`/`paths` aliases, directory `index` files), the `tsconfig` reader,
//! the declared return type of a function, and how an `imports` edge says
//! what it binds.

use serde_json::Value;
use tree_sitter::Node;

use super::scope::{TypeText, Via};

/// What the link pass needs of a workspace's `tsconfig`: where non-relative
/// specifiers start (`baseUrl`) and its aliases (`paths`), as repository
/// paths (`""` is the root).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TsConfig {
    /// `compilerOptions.baseUrl`, joined to the file's directory.
    pub base_url: Option<String>,
    /// `compilerOptions.paths`, sorted by pattern (the JSON map is read
    /// without its order): pattern (`@acme/auth`, `@acme/ui/*`) → targets,
    /// joined to the directory `paths` are relative to (`baseUrl`, else the
    /// file's). Resolution takes the longest match, so order only breaks a
    /// tie between two wildcard patterns of equal prefix (the first by
    /// pattern wins; TypeScript takes the first in the file).
    pub paths: Vec<(String, Vec<String>)>,
}

/// A `package.json` of the repository: the evidence that a specifier names a
/// package (DD-9), and the name a workspace package takes (`@acme/ui` of
/// `libs/ui/package.json` is the repository's, not npm's).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Manifest {
    /// Its directory (`""` for the root).
    pub dir: String,
    /// `name`.
    pub name: Option<String>,
    /// `dependencies`, `devDependencies`, `peerDependencies` and
    /// `optionalDependencies`, sorted.
    pub deps: Vec<String>,
}

impl Manifest {
    /// Read a `package.json` in the repository directory `dir`; `None` when
    /// it does not parse.
    pub fn parse(text: &str, dir: &str) -> Option<Self> {
        let v: Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()?;
        let mut deps: Vec<String> = [
            "dependencies",
            "devDependencies",
            "peerDependencies",
            "optionalDependencies",
        ]
        .iter()
        .filter_map(|k| v.get(*k).and_then(Value::as_object))
        .flat_map(|m| m.keys().cloned())
        .collect();
        deps.sort();
        deps.dedup();
        Some(Self {
            dir: dir.to_string(),
            name: v.get("name").and_then(Value::as_str).map(str::to_string),
            deps,
        })
    }
}

/// What the link pass reads of a TypeScript/JavaScript workspace besides its
/// sources: the root `tsconfig` and the `package.json` files.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ScriptEnv {
    /// The root `tsconfig` (aliases, `baseUrl`).
    pub tsconfig: Option<TsConfig>,
    /// Every `package.json` of the branch, by directory.
    pub manifests: Vec<Manifest>,
}

impl ScriptEnv {
    /// A stable digest of what resolution reads: a change re-links the
    /// branch in full (DD-6).
    pub fn fingerprint(&self) -> String {
        let text = self.to_json();
        format!("{:016x}", devctx_core::symbol_id::fnv1a64(&[&text]))
    }

    /// As stored in `index_meta` (the last one that could be read).
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Back from [`ScriptEnv::to_json`].
    pub fn from_json(text: &str) -> Option<Self> {
        serde_json::from_str(text).ok()
    }
}

impl TsConfig {
    /// Read a `tsconfig` (JSON with comments and trailing commas) that sits
    /// in the repository directory `dir` (`""` for the root). `None` when it
    /// does not parse.
    pub fn parse(text: &str, dir: &str) -> Option<Self> {
        let v: Value = serde_json::from_str(&strip_jsonc(text)).ok()?;
        let opts = v.get("compilerOptions");
        let base_url = opts
            .and_then(|o| o.get("baseUrl"))
            .and_then(Value::as_str)
            .map(|b| join(dir, b));
        let paths_dir = base_url.clone().unwrap_or_else(|| dir.to_string());
        let mut paths = Vec::new();
        if let Some(map) = opts.and_then(|o| o.get("paths")).and_then(Value::as_object) {
            for (k, targets) in map {
                let list: Vec<String> = targets
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(|t| join(&paths_dir, t))
                    .collect();
                paths.push((k.clone(), list));
            }
        }
        Some(Self { base_url, paths })
    }

    /// The `extends` of a `tsconfig` text, in order: a string or (TypeScript
    /// 5) an array; entries naming a file (`./base.json`,
    /// `../tsconfig.base.json`) as written, a package (`@tsconfig/node20`)
    /// apart — not followed.
    pub fn extends_of(text: &str) -> (Vec<String>, Vec<String>) {
        let Ok(v) = serde_json::from_str::<Value>(&strip_jsonc(text)) else {
            return (Vec::new(), Vec::new());
        };
        let list: Vec<String> = match v.get("extends") {
            Some(Value::String(e)) => vec![e.clone()],
            Some(Value::Array(a)) => a
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
            _ => Vec::new(),
        };
        list.into_iter().partition(|e| e.starts_with('.'))
    }

    /// `self` over `base` (an `extends`): what `self` sets wins, whole.
    pub fn over(self, base: TsConfig) -> TsConfig {
        TsConfig {
            base_url: self.base_url.or(base.base_url),
            paths: if self.paths.is_empty() {
                base.paths
            } else {
                self.paths
            },
        }
    }

    /// A stable digest of what resolution reads: a change re-links the
    /// branch in full (DD-6).
    pub fn fingerprint(&self) -> String {
        let mut s = format!("base={}", self.base_url.as_deref().unwrap_or("-"));
        for (k, v) in &self.paths {
            s.push('\u{1f}');
            s.push_str(k);
            s.push('=');
            s.push_str(&v.join(","));
        }
        format!("{:016x}", devctx_core::symbol_id::fnv1a64(&[&s]))
    }
}

/// JSON with comments (`//`, `/* */`) and trailing commas, as `tsconfig`
/// allows, made plain JSON. Strings are left alone.
fn strip_jsonc(text: &str) -> String {
    // A UTF-8 byte-order mark (editors on Windows write one) is no JSON.
    let text = text.trim_start_matches('\u{feff}');
    let b: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut in_str = false;
    while i < b.len() {
        let c = b[i];
        if in_str {
            out.push(c);
            if c == '\\' && i + 1 < b.len() {
                out.push(b[i + 1]);
                i += 2;
                continue;
            }
            if c == '"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        match (c, b.get(i + 1)) {
            ('"', _) => {
                in_str = true;
                out.push(c);
                i += 1;
            }
            ('/', Some('/')) => {
                while i < b.len() && b[i] != '\n' {
                    i += 1;
                }
            }
            ('/', Some('*')) => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == '*' && b[i + 1] == '/') {
                    i += 1;
                }
                i += 2;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    // Trailing commas: a `,` followed (past whitespace) by `}` or `]`.
    let chars: Vec<char> = out.chars().collect();
    let mut clean = String::with_capacity(out.len());
    let mut in_str = false;
    for (i, &c) in chars.iter().enumerate() {
        if in_str {
            clean.push(c);
            if c == '"' && !escaped(&chars, i) {
                in_str = false;
            }
            continue;
        }
        if c == '"' {
            in_str = true;
        }
        if c == ',' {
            let next = chars[i + 1..].iter().find(|c| !c.is_whitespace());
            if matches!(next, Some('}') | Some(']')) {
                continue;
            }
        }
        clean.push(c);
    }
    clean
}

/// Whether the `"` at `i` is escaped (an odd run of `\` before it).
fn escaped(chars: &[char], i: usize) -> bool {
    chars[..i].iter().rev().take_while(|c| **c == '\\').count() % 2 == 1
}

/// `rel` against the repository directory `dir`, normalised (`.` and `..`
/// folded, no leading `./`). `""` is the root.
pub fn join(dir: &str, rel: &str) -> String {
    let mut parts: Vec<&str> = if rel.starts_with('/') {
        Vec::new()
    } else {
        dir.split('/').filter(|p| !p.is_empty()).collect()
    };
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

/// The directory of a repository path (`a/b/c.ts` → `a/b`, `c.ts` → `""`).
pub fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

/// The type a declaration without one written takes from its initializer:
/// `new T(…)` and `x as T` → `T`; Angular's `inject(T)` (or `inject<T>(…)`)
/// → `T`, injected (DD-7 rule 4). `None`: nothing local says.
pub fn init_type(init: Node<'_>, bytes: &[u8]) -> Option<(TypeText, Via)> {
    let text = |n: Node<'_>| n.utf8_text(bytes).ok();
    match init.kind() {
        "new_expression" => {
            let c = init.child_by_field_name("constructor")?;
            matches!(c.kind(), "identifier" | "member_expression")
                .then(|| TypeText::parse(text(c)?))
                .flatten()
                .map(|t| (t, Via::Local))
        }
        "as_expression" | "satisfies_expression" => {
            let ty = init.named_child(1)?;
            Some((TypeText::parse(text(ty)?)?, Via::Local))
        }
        "parenthesized_expression" | "non_null_expression" => {
            init_type(init.named_child(0)?, bytes)
        }
        "call_expression" => {
            let f = init.child_by_field_name("function")?;
            if f.kind() != "identifier" || text(f)? != "inject" {
                return None;
            }
            if let Some(targs) = init.child_by_field_name("type_arguments") {
                let first = targs.named_child(0)?;
                return Some((TypeText::parse(text(first)?)?, Via::CtorInject));
            }
            let args = init.child_by_field_name("arguments")?;
            let first = args.named_child(0)?;
            matches!(first.kind(), "identifier" | "member_expression")
                .then(|| TypeText::parse(text(first)?))
                .flatten()
                .map(|t| (t, Via::CtorInject))
        }
        _ => None,
    }
}

/// The return type a TypeScript signature declares, generic arguments
/// dropped: `login(u: string): Observable<Session>` → `Observable`, `const f
/// = (n: string): string =>` → `string`. `None` when it declares none.
pub fn return_type(signature: &str) -> Option<String> {
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
    let rest = signature[close? + 1..].trim_start();
    let ty = rest.strip_prefix(':')?.trim();
    let ty = ty.strip_suffix("=>").unwrap_or(ty).trim();
    let t = TypeText::parse(ty)?;
    (!t.base.is_empty()).then_some(t.base)
}

/// One name an `imports` row of a TypeScript/JavaScript file binds or
/// re-exports, read back from its destination (`./x#Name`, `pkg#*`, `./x`)
/// and its hint (`export`, `as Local`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TsImport {
    /// The module specifier as written.
    pub spec: String,
    /// The imported name (`default` for a default import); `None` for a
    /// side-effect import or a wildcard.
    pub name: Option<String>,
    /// The name it is bound to (or re-exported as).
    pub local: Option<String>,
    /// `* as ns` / `export * from`.
    pub wildcard: bool,
    /// An `export … from`: binds nothing in the file, re-exports.
    pub reexport: bool,
}

impl TsImport {
    /// From an `imports` row.
    pub fn from_row(dst_name: &str, hint: Option<&str>) -> Self {
        let (spec, tail) = match dst_name.rsplit_once('#') {
            Some((s, t)) => (s.to_string(), Some(t)),
            None => (dst_name.to_string(), None),
        };
        let wildcard = tail == Some("*");
        let name = tail.filter(|t| *t != "*").map(str::to_string);
        let tokens: Vec<&str> = hint.unwrap_or_default().split_whitespace().collect();
        let reexport = tokens.first() == Some(&"export");
        let alias = tokens
            .iter()
            .position(|t| *t == "as")
            .and_then(|i| tokens.get(i + 1))
            .map(|a| a.to_string());
        let local = alias.or_else(|| name.clone().filter(|n| n != "default"));
        Self {
            spec,
            name,
            local,
            wildcard,
            reexport,
        }
    }
}

/// Extensions a specifier without one may name, in TypeScript's order.
pub const SCRIPT_EXTENSIONS: &[&str] =
    &["ts", "tsx", "d.ts", "js", "jsx", "mjs", "cjs", "mts", "cts"];

/// Every repository path a module path (`libs/auth/src/x`, already joined)
/// may name, in the order a resolver tries them: as written, a `.js` written
/// for its `.ts` source, with each extension, its directory's `index`.
pub fn probes(path: &str) -> Vec<String> {
    let mut out = vec![path.to_string()];
    for (js, ts) in [
        (".js", ".ts"),
        (".jsx", ".tsx"),
        (".mjs", ".mts"),
        (".cjs", ".cts"),
    ] {
        if let Some(stem) = path.strip_suffix(js) {
            out.push(format!("{stem}{ts}"));
        }
    }
    for ext in SCRIPT_EXTENSIONS {
        out.push(format!("{path}.{ext}"));
    }
    for ext in ["ts", "tsx", "js", "jsx", "mjs"] {
        out.push(if path.is_empty() {
            format!("index.{ext}")
        } else {
            format!("{path}/index.{ext}")
        });
    }
    out
}

/// How a specifier is to be looked up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Specifier {
    /// Repository paths to probe ([`probes`] of each), in order: a relative
    /// path, a `paths` alias, a `baseUrl` path. `alias`: a `paths` pattern
    /// matched (the repository's even when nothing is found).
    Paths { bases: Vec<String>, alias: bool },
    /// A package (npm, or `node:`): from outside the repository.
    Package,
}

impl TsConfig {
    /// How `spec`, written in `from`, is looked up: a relative path; else
    /// the longest matching `paths` pattern (an exact one first); else a
    /// `baseUrl` path then a package.
    pub fn specifier(cfg: Option<&TsConfig>, from: &str, spec: &str) -> Specifier {
        if spec.starts_with("./") || spec.starts_with("../") || spec == "." || spec == ".." {
            return Specifier::Paths {
                bases: vec![join(dir_of(from), spec)],
                alias: false,
            };
        }
        let Some(cfg) = cfg else {
            return Specifier::Package;
        };
        let mut best: Option<(usize, Vec<String>)> = None;
        for (pattern, targets) in &cfg.paths {
            let matched: Option<(usize, String)> = match pattern.split_once('*') {
                None if pattern == spec => Some((usize::MAX, String::new())),
                None => None,
                Some((pre, post)) => spec
                    .strip_prefix(pre)
                    .and_then(|r| r.strip_suffix(post))
                    .map(|star| (pre.len(), star.to_string())),
            };
            if let Some((rank, star)) = matched {
                if best.as_ref().is_none_or(|(r, _)| rank > *r) {
                    let bases = targets.iter().map(|t| t.replacen('*', &star, 1)).collect();
                    best = Some((rank, bases));
                }
            }
        }
        if let Some((_, bases)) = best {
            return Specifier::Paths { bases, alias: true };
        }
        match &cfg.base_url {
            Some(base) => Specifier::Paths {
                bases: vec![join(base, spec)],
                alias: false,
            },
            None => Specifier::Package,
        }
    }
}

// ------------------------------------------------------------- the link pass

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use super::link::{
    cap, external, hi, is_callable, is_type, leaf, medium, undecided, Call, LinkEdge, Outcome,
    RepoIndex, Resolved, TypeRef, IMPORT_WEAK,
};

/// Where a specifier leads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Module {
    /// A file of the repository.
    Repo(String),
    /// A package a `package.json` declares, or a Node builtin: from
    /// outside the repository (DD-9).
    External,
    /// No file found and no evidence of a package: a relative path or an
    /// alias with no file, an alias of a `tsconfig` that is not read (an
    /// app's own), a module no manifest declares.
    Unknown,
}

/// Node's builtin modules (also as `node:x`): packages without a manifest.
const NODE_BUILTINS: &[&str] = &[
    "assert",
    "async_hooks",
    "buffer",
    "child_process",
    "cluster",
    "console",
    "constants",
    "crypto",
    "dgram",
    "diagnostics_channel",
    "dns",
    "domain",
    "events",
    "fs",
    "fs/promises",
    "http",
    "http2",
    "https",
    "inspector",
    "module",
    "net",
    "os",
    "path",
    "perf_hooks",
    "process",
    "punycode",
    "querystring",
    "readline",
    "repl",
    "stream",
    "string_decoder",
    "test",
    "timers",
    "tls",
    "trace_events",
    "tty",
    "url",
    "util",
    "v8",
    "vm",
    "wasi",
    "worker_threads",
    "zlib",
];

/// The package a bare specifier names: `@scope/name/sub` → `@scope/name`,
/// `name/sub` → `name`.
pub fn package_name(spec: &str) -> &str {
    let mut parts = spec.splitn(3, '/');
    let first = parts.next().unwrap_or(spec);
    if first.starts_with('@') {
        match parts.next() {
            Some(second) => &spec[..first.len() + 1 + second.len()],
            None => first,
        }
    } else {
        first
    }
}

/// What a module exports under a name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Export {
    /// A symbol; `false` when another branch could not be followed (a
    /// package's `export *`, a module not found) or barrels nest too deep.
    Found(usize, bool),
    /// Only a package's `export *` can have it.
    External,
    /// Every branch followed, none has it.
    Missing,
    /// A branch could not be followed.
    Unknown,
}

/// What a local name of a file is bound to by its imports.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Imported {
    /// A symbol; `false`: not surely that one (a default import matched by
    /// its local name, a barrel branch not followed).
    Sym(usize, bool),
    /// `import * as ns` of a repository module.
    Module(String),
    /// From a package, and no repository file defines the name (DD-9).
    External,
    /// Imported, but where to cannot be followed.
    Unknown,
}

/// Memo of what a specifier names and what a module exports: every call
/// through an Nx alias walks the same barrels.
#[derive(Default)]
pub(super) struct TsCache {
    /// (directory of the importing file, specifier) → module.
    modules: Mutex<HashMap<(String, String), Module>>,
    /// (file, name, depth) → what it exports under it, from that depth.
    exports: Mutex<HashMap<(String, String, u8), Export>>,
}

/// How many re-exports deep a barrel is followed (`index.ts` → `lib/index.ts`
/// → …).
const BARREL_DEPTH: u8 = 4;

impl RepoIndex {
    /// The file `spec` (written in `from`) names, or where it is from.
    pub(super) fn module(&self, from: &str, spec: &str) -> Module {
        let key = (dir_of(from).to_string(), spec.to_string());
        if let Some(m) = self
            .ts_cache
            .modules
            .lock()
            .ok()
            .and_then(|c| c.get(&key).cloned())
        {
            return m;
        }
        let m = self.module_uncached(from, spec);
        if let Ok(mut c) = self.ts_cache.modules.lock() {
            c.insert(key, m.clone());
        }
        m
    }

    fn module_uncached(&self, from: &str, spec: &str) -> Module {
        let package = || {
            if self.is_package(from, spec) {
                Module::External
            } else {
                Module::Unknown
            }
        };
        match TsConfig::specifier(self.ts.as_ref(), from, spec) {
            Specifier::Package => package(),
            Specifier::Paths { bases, alias } => {
                for b in &bases {
                    if let Some(p) = probes(b).into_iter().find(|p| self.files.contains_key(p)) {
                        return Module::Repo(p);
                    }
                }
                // A relative path or an alias is the repository's, just not
                // found; a miss under `baseUrl` may be a package.
                if alias || spec.starts_with('.') {
                    Module::Unknown
                } else {
                    package()
                }
            }
        }
    }

    /// Whether a bare specifier written in `from` names a package, with
    /// evidence (DD-9): a Node builtin, or a dependency of the root
    /// `package.json` or of the nearest one above `from` — and no workspace
    /// package of the repository has that name.
    fn is_package(&self, from: &str, spec: &str) -> bool {
        if let Some(rest) = spec.strip_prefix("node:") {
            return !rest.is_empty();
        }
        let name = package_name(spec);
        if NODE_BUILTINS.contains(&spec) || NODE_BUILTINS.contains(&name) {
            return true;
        }
        if self
            .manifests
            .iter()
            .any(|m| m.name.as_deref() == Some(name))
        {
            return false;
        }
        let dir = dir_of(from);
        let nearest = self
            .manifests
            .iter()
            .filter(|m| m.dir.is_empty() || dir == m.dir || dir.starts_with(&format!("{}/", m.dir)))
            .max_by_key(|m| m.dir.len());
        let declares = |m: &Manifest| m.deps.binary_search_by(|d| d.as_str().cmp(name)).is_ok();
        self.manifests
            .iter()
            .filter(|m| m.dir.is_empty())
            .chain(nearest)
            .any(declares)
    }

    /// Every path a specifier may name (for the incremental selection).
    fn spec_paths(&self, from: &str, spec: &str) -> Vec<String> {
        match TsConfig::specifier(self.ts.as_ref(), from, spec) {
            Specifier::Package => Vec::new(),
            Specifier::Paths { bases, .. } => bases.iter().flat_map(|b| probes(b)).collect(),
        }
    }

    /// The top-level symbols of `file` named `name`.
    fn top_level(&self, file: &str, name: &str) -> Vec<usize> {
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

    /// What `file` exports as `name`: its own top-level symbol, a named
    /// re-export, or one of its `export *` (TypeScript's rules: a name two
    /// `export *` give is not exported).
    fn export_of(&self, file: &str, name: &str, depth: u8) -> Export {
        // Memoised by depth too: a walk cut by the depth cap answers only
        // for its depth, and with it a fan-out of `export *` costs each
        // (file, name, depth) once instead of F^depth.
        let key = (file.to_string(), name.to_string(), depth);
        if let Some(e) = self
            .ts_cache
            .exports
            .lock()
            .ok()
            .and_then(|c| c.get(&key).copied())
        {
            return e;
        }
        let e = self.export_walk(file, name, depth);
        if let Ok(mut c) = self.ts_cache.exports.lock() {
            c.insert(key, e);
        }
        e
    }

    fn export_walk(&self, file: &str, name: &str, depth: u8) -> Export {
        if depth > BARREL_DEPTH {
            return Export::Unknown;
        }
        let own = self.top_level(file, name);
        if !own.is_empty() {
            // A class merged with an interface of the same name: the class.
            let pick = own
                .iter()
                .copied()
                .find(|&i| self.syms[i].kind != "interface")
                .unwrap_or(own[0]);
            return Export::Found(pick, true);
        }
        let Some(info) = self.files.get(file) else {
            return Export::Unknown;
        };
        for imp in info.script.iter().filter(|i| i.reexport && !i.wildcard) {
            if imp.local.as_deref() != Some(name) {
                continue;
            }
            return match (self.module(file, &imp.spec), imp.name.as_deref()) {
                (Module::Repo(f), Some(n)) => match self.export_of(&f, n, depth + 1) {
                    Export::Missing => Export::Unknown,
                    e => e,
                },
                (Module::External, _) => Export::External,
                _ => Export::Unknown,
            };
        }
        let (mut found, mut ext, mut unk) = (Vec::new(), false, false);
        for imp in info
            .script
            .iter()
            .filter(|i| i.reexport && i.wildcard && i.local.is_none())
        {
            match self.module(file, &imp.spec) {
                Module::Repo(f) => match self.export_of(&f, name, depth + 1) {
                    Export::Found(i, sure) => {
                        if !found.iter().any(|(j, _)| *j == i) {
                            found.push((i, sure));
                        }
                    }
                    Export::External => ext = true,
                    Export::Unknown => unk = true,
                    Export::Missing => {}
                },
                Module::External => ext = true,
                Module::Unknown => unk = true,
            }
        }
        match found.as_slice() {
            [(i, sure)] => Export::Found(*i, *sure && !ext && !unk),
            [] if ext => Export::External,
            [] if unk => Export::Unknown,
            [] => Export::Missing,
            _ => Export::Unknown,
        }
    }

    /// What the local name `local` of `file` is bound to by an import;
    /// `None`: no import binds it.
    fn imported(&self, file: &str, local: &str) -> Option<Imported> {
        let info = self.files.get(file)?;
        let imp = info
            .script
            .iter()
            .find(|i| !i.reexport && i.local.as_deref() == Some(local))?;
        let name = imp.name.as_deref();
        // A package the manifests declare (or a Node builtin) is the
        // evidence: external, whatever the repository defines under the same
        // name (TASK-006 second review, mi2).
        Some(match self.module(file, &imp.spec) {
            Module::External => Imported::External,
            Module::Unknown => Imported::Unknown,
            Module::Repo(f) if imp.wildcard => Imported::Module(f),
            Module::Repo(f) => match name {
                Some("default") => match self.export_of(&f, "default", 0) {
                    Export::Found(i, sure) => Imported::Sym(i, sure),
                    // `export default class Store`: the symbol is not marked
                    // default; its local name, by convention (medium).
                    _ => match self.top_level(&f, local).as_slice() {
                        [i] => Imported::Sym(*i, false),
                        _ => Imported::Unknown,
                    },
                },
                Some(n) => match self.export_of(&f, n, 0) {
                    Export::Found(i, sure) => Imported::Sym(i, sure),
                    Export::External => Imported::External,
                    Export::Missing | Export::Unknown => Imported::Unknown,
                },
                None => Imported::Unknown,
            },
        })
    }

    /// A type name as written in a TypeScript/JavaScript file: this file's,
    /// an import's (through aliases and barrels), a platform global; then a
    /// unique type of that name (`unique_name`, medium).
    pub(super) fn script_type(&self, name: &str, file: &str, ctx: Option<usize>) -> TypeRef {
        if let Some((first, rest)) = name.split_once('.') {
            // `ns.Type` of an `import * as ns`.
            return match self.imported(file, first) {
                Some(Imported::Module(f)) => match self.export_of(&f, leaf(rest), 0) {
                    Export::Found(i, sure) if is_type(&self.syms[i].kind) => {
                        TypeRef::Repo(i, if sure { "import" } else { IMPORT_WEAK })
                    }
                    Export::External => TypeRef::External,
                    _ => TypeRef::Unknown,
                },
                Some(Imported::External) => TypeRef::External,
                _ if self.platform(file, first) => TypeRef::External,
                _ => TypeRef::Unknown,
            };
        }
        if let Some(t) = self.same_file_type(name, file, ctx) {
            return TypeRef::Repo(t, "same_file");
        }
        match self.imported(file, name) {
            Some(Imported::Sym(i, sure)) if is_type(&self.syms[i].kind) => {
                return TypeRef::Repo(i, if sure { "import" } else { IMPORT_WEAK });
            }
            Some(Imported::External) => return TypeRef::External,
            _ => {}
        }
        if self.platform(file, name) {
            return TypeRef::External;
        }
        let types: Vec<usize> = self
            .by_name
            .get(name)
            .into_iter()
            .flatten()
            .copied()
            .filter(|&i| is_type(&self.syms[i].kind) && self.syms[i].qualified == name)
            .collect();
        match types.as_slice() {
            [t] => TypeRef::Repo(*t, "unique_name"),
            _ => TypeRef::Unknown,
        }
    }

    /// `f()` in TypeScript/JavaScript: a function of an enclosing scope of
    /// the file (never a class member: there is no implicit `this`), an
    /// import, a platform global; then by name (rules 7-9). `scoped`: the
    /// parse saw a function of that name in scope (hint `bare`); without it
    /// (hint `free`) no enclosing function's own is looked at — a function
    /// bound in a sibling block is not in scope.
    pub(super) fn script_bare(&self, c: &Call<'_>, scoped: bool) -> Outcome {
        let ancestors = if scoped {
            self.chain(c.src)
        } else {
            Vec::new()
        };
        for anc in ancestors {
            let s = &self.syms[anc];
            if is_type(&s.kind) {
                continue;
            }
            let local: Vec<usize> = self
                .by_qualified
                .get(&if s.kind == devctx_core::symbol_id::FILE_KIND {
                    c.callee.to_string()
                } else {
                    format!("{}.{}", s.qualified, c.callee)
                })
                .into_iter()
                .flatten()
                .copied()
                .filter(|&k| {
                    self.syms[k].file == c.file
                        && self.syms[k].parent_id == Some(s.id)
                        && is_callable(&self.syms[k].kind)
                })
                .collect();
            if !local.is_empty() {
                let (m, exact) = self.pick_overload(&local, c.args);
                let out = self.member_hit(m, false, exact, "same_file");
                // Two blocks of the function each bind the name: the symbols
                // cannot say which one is in scope.
                return if local.len() > 1 {
                    cap(out, "medium")
                } else {
                    out
                };
            }
        }
        if c.src.is_none() || !scoped {
            if let Some(&t) = self
                .top_level(c.file, c.callee)
                .iter()
                .find(|&&i| is_callable(&self.syms[i].kind))
            {
                return hi(t, self, "same_file");
            }
        }
        match self.imported(c.file, c.callee) {
            Some(Imported::Sym(i, sure)) if is_callable(&self.syms[i].kind) => {
                return if sure {
                    hi(i, self, "import")
                } else {
                    medium(i, self, "import")
                };
            }
            Some(Imported::External) => return external("external_known"),
            // Imported from somewhere that cannot be followed (or from a
            // package, of a name a repository file also defines): never a
            // guess by name (TASK-006 review, M1).
            Some(Imported::Sym(..)) | Some(Imported::Module(_)) | Some(Imported::Unknown) => {
                return undecided()
            }
            None => {}
        }
        self.by_name_only(c.callee, c.file, c.args)
    }

    /// `r.f()` with `r` bound by no scope: a type or an object of this file,
    /// an import (a class, an object, a namespace), a platform global; then
    /// by name.
    pub(super) fn script_name(&self, c: &Call<'_>, recv: &str) -> Outcome {
        let (file, callee) = (c.file, c.callee);
        if let Some((first, _)) = recv.split_once('.') {
            // `window.location.reload()`, `pkg.a.b()`: where the first
            // segment is from, if it says.
            return match self.imported(file, first) {
                Some(Imported::External) => external("external_known"),
                _ if self.platform(file, first) => external("external_known"),
                _ => self.by_name_only_low(c),
            };
        }
        if let Some(t) = self.same_file_type(recv, file, c.src) {
            return self.in_type(c, t, "same_file", "high");
        }
        if let Some(&o) = self.top_level(file, recv).first() {
            return self.object_member(c, o, "same_file", "high");
        }
        match self.imported(file, recv) {
            Some(Imported::Sym(i, sure)) => {
                let conf = if sure { "high" } else { "medium" };
                return if is_type(&self.syms[i].kind) {
                    self.in_type(c, i, "import", conf)
                } else {
                    self.object_member(c, i, "import", conf)
                };
            }
            Some(Imported::Module(f)) => {
                return match self.export_of(&f, callee, 0) {
                    Export::Found(i, sure) if is_callable(&self.syms[i].kind) => {
                        cap(hi(i, self, "import"), if sure { "high" } else { "medium" })
                    }
                    Export::External => external("external_known"),
                    _ => undecided(),
                };
            }
            Some(Imported::External) => return external("external_known"),
            Some(Imported::Unknown) => return undecided(),
            None => {}
        }
        // `Klass.make()` with `const Klass = class { static make() }`, or
        // an object literal's `api.run()`: the file's only callable named
        // `recv.callee` (the binding is the file's; no import has the name).
        let own: Vec<usize> = self
            .by_qualified
            .get(&format!("{recv}.{callee}"))
            .into_iter()
            .flatten()
            .copied()
            .filter(|&m| self.syms[m].file == file && is_callable(&self.syms[m].kind))
            .collect();
        if let [m] = own.as_slice() {
            return hi(*m, self, "same_file");
        }
        if self.platform(file, recv) {
            return external("external_known");
        }
        self.by_name_only_low(c)
    }

    /// `o.f()` with `o` a non-type symbol (an object literal's `const`): its
    /// method `o.f`, else undecided.
    fn object_member(
        &self,
        c: &Call<'_>,
        o: usize,
        how: &'static str,
        conf: &'static str,
    ) -> Outcome {
        let os = &self.syms[o];
        let q = format!("{}.{}", os.qualified, c.callee);
        match self
            .by_qualified
            .get(&q)
            .into_iter()
            .flatten()
            .copied()
            .find(|&m| self.syms[m].file == os.file && is_callable(&self.syms[m].kind))
        {
            Some(m) => cap(hi(m, self, how), conf),
            None => undecided(),
        }
    }

    /// An `imports` row of a TypeScript/JavaScript file: the symbol it
    /// names, a repository module, or a package.
    pub(super) fn script_import(&self, e: &LinkEdge) -> Outcome {
        let imp = TsImport::from_row(&e.dst_name, e.hint.as_deref());
        let name = imp.name.as_deref();
        let module_only = Outcome::Resolved(Resolved {
            dst_id: None,
            confidence: "high",
            resolution: "import",
            external: false,
        });
        match self.module(&e.file, &imp.spec) {
            Module::External => external("external_known"),
            Module::Unknown => undecided(),
            Module::Repo(f) => match name {
                None => module_only,
                Some(n) => match self.export_of(&f, n, 0) {
                    Export::Found(i, true) => hi(i, self, "import"),
                    Export::Found(i, false) => medium(i, self, "import"),
                    Export::External => external("external_known"),
                    _ if n == "default" => {
                        match imp.local.as_deref().map(|l| self.top_level(&f, l)) {
                            Some(v) if v.len() == 1 => medium(v[0], self, "import"),
                            _ => cap(module_only, "medium"),
                        }
                    }
                    _ => undecided(),
                },
            },
        }
    }

    /// The TypeScript/JavaScript files whose imports may resolve differently
    /// after `written` changed: those importing a written file, and
    /// through barrels that re-export one, theirs (DD-6, the incremental
    /// pass). A specifier is matched by every path it may name, so a file
    /// added or deleted under it counts.
    pub fn importers_of(&self, written: &HashSet<String>) -> HashSet<String> {
        let mut out: HashSet<String> = HashSet::new();
        if written.is_empty() {
            return out;
        }
        // Every path each file's specifiers may name, computed once: the
        // rounds below only look them up.
        let reach: Vec<(&String, Vec<String>, bool)> = self
            .files
            .iter()
            .filter(|(_, info)| !info.script.is_empty())
            .map(|(file, info)| {
                let mut paths: Vec<String> = info
                    .script
                    .iter()
                    .flat_map(|imp| self.spec_paths(file, &imp.spec))
                    .collect();
                paths.sort();
                paths.dedup();
                (file, paths, info.script.iter().any(|i| i.reexport))
            })
            .collect();
        let mut frontier: HashSet<String> = written.clone();
        for _ in 0..=BARREL_DEPTH {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bom_and_an_extends_array_are_read() {
        let cfg = TsConfig::parse(
            "\u{feff}{ \"compilerOptions\": { \"baseUrl\": \".\" } }",
            "",
        )
        .expect("a BOM is skipped");
        assert_eq!(cfg.base_url.as_deref(), Some(""));
        let (files, packages) =
            TsConfig::extends_of(r#"{ "extends": ["./a.json", "@tsconfig/node20", "../b"] }"#);
        assert_eq!(files, ["./a.json", "../b"]);
        assert_eq!(packages, ["@tsconfig/node20"]);
        assert_eq!(package_name("@scope/name/sub"), "@scope/name");
        assert_eq!(package_name("rxjs/operators"), "rxjs");
    }

    #[test]
    fn jsonc_is_made_json() {
        let t = "{ // c\n \"a\": \"x//y\", /* b */ \"b\": [1, 2,], }";
        let v: Value = serde_json::from_str(&strip_jsonc(t)).unwrap();
        assert_eq!(v["a"], "x//y");
        assert_eq!(v["b"][1], 2);
    }

    #[test]
    fn return_types_from_signatures() {
        assert_eq!(
            return_type("login(user: string): Observable<Session>").as_deref(),
            Some("Observable")
        );
        assert_eq!(
            return_type("const f = (n: string): string =>").as_deref(),
            Some("string")
        );
        assert_eq!(return_type("const f: Fn = (req, next) =>"), None);
        assert_eq!(return_type("constructor(private a: A)"), None);
        assert_eq!(return_type("find(): Foo | null").as_deref(), Some("Foo"));
    }

    #[test]
    fn import_rows_read_back() {
        let i = TsImport::from_row("./x#Thing", Some("export as Alias"));
        assert_eq!(
            (
                i.spec.as_str(),
                i.name.as_deref(),
                i.local.as_deref(),
                i.reexport
            ),
            ("./x", Some("Thing"), Some("Alias"), true)
        );
        let i = TsImport::from_row("./s#default", Some("as Store"));
        assert_eq!(i.local.as_deref(), Some("Store"));
        let i = TsImport::from_row("rxjs#*", Some("as rx"));
        assert!(i.wildcard && i.name.is_none() && !i.reexport);
        let i = TsImport::from_row("./all#*", Some("export"));
        assert!(i.wildcard && i.reexport && i.local.is_none());
    }

    #[test]
    fn specifiers_by_relative_alias_base_and_package() {
        let cfg = TsConfig::parse(
            r#"{"compilerOptions":{"baseUrl":".","paths":{"@a/x":["libs/x/src/index.ts"],"@a/*":["libs/*/src"]}}}"#,
            "",
        )
        .unwrap();
        let c = Some(&cfg);
        let paths = |bases: &[&str], alias| Specifier::Paths {
            bases: bases.iter().map(|b| b.to_string()).collect(),
            alias,
        };
        assert_eq!(
            TsConfig::specifier(c, "apps/a/b.ts", "../c"),
            paths(&["apps/c"], false)
        );
        assert_eq!(
            TsConfig::specifier(c, "z.ts", "@a/x"),
            paths(&["libs/x/src/index.ts"], true)
        );
        assert_eq!(
            TsConfig::specifier(c, "z.ts", "@a/y"),
            paths(&["libs/y/src"], true)
        );
        assert_eq!(
            TsConfig::specifier(c, "z.ts", "rxjs"),
            paths(&["rxjs"], false)
        );
        assert_eq!(
            TsConfig::specifier(None, "z.ts", "rxjs"),
            Specifier::Package
        );
        assert!(probes("a/b.js").contains(&"a/b.ts".to_string()));
        assert!(probes("a/b").contains(&"a/b/index.ts".to_string()));
    }

    #[test]
    fn paths_join_and_fold() {
        assert_eq!(join("a/b", "../c/./d.ts"), "a/c/d.ts");
        assert_eq!(join("", "./x"), "x");
        assert_eq!(join("a", "."), "a");
        assert_eq!(dir_of("a/b/c.ts"), "a/b");
        assert_eq!(dir_of("c.ts"), "");
    }
}
