//! What the link pass reads of a workspace besides its sources (PLAN-009
//! TASK-007, DD-6, DD-9): the manifests that say which modules are the
//! repository's and which packages are declared dependencies.
//!
//! - Rust: every `Cargo.toml` — a `[package]` is a crate of the workspace
//!   (its code name: `[lib] name`, else `name`, `-` as `_`), and the keys of
//!   its dependency tables are the crates it may name from outside.
//! - Go: every `go.mod` — its `module` path is the repository's import
//!   prefix, its `require`s are declared, a `replace` to a local path is the
//!   repository's.
//! - Python: `pyproject.toml` (PEP 621 and Poetry), `setup.cfg`
//!   (`install_requires`) and `requirements*.txt` — the import names of the
//!   distributions they declare, best effort.
//!
//! TypeScript/JavaScript keep their own ([`ScriptEnv`]); [`LinkEnv`] is the
//! whole of it, stored in `index_meta` as the last environment that could be
//! read, with one fingerprint: another environment relinks the branch in
//! full. Readers are pure: they take a file's text and its repository path,
//! and answer `None` when it cannot be read (the caller keeps the last good
//! one, per file).

use serde::{Deserialize, Serialize};

use super::typescript::{dir_of, join, ScriptEnv};

/// Everything the link pass reads besides the sources.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkEnv {
    /// TypeScript/JavaScript: the root `tsconfig` and the `package.json`
    /// files. Flattened: an environment stored before TASK-007 (only this)
    /// reads back as a [`LinkEnv`] with nothing else.
    #[serde(flatten)]
    pub script: ScriptEnv,
    /// Every `Cargo.toml`, by path.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cargo: Vec<CargoManifest>,
    /// Every `go.mod`, by path.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub go: Vec<GoModule>,
    /// Every Python manifest (`pyproject.toml`, `setup.cfg`,
    /// `requirements*.txt`), by path.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub python: Vec<PyManifest>,
}

impl LinkEnv {
    /// A stable digest of what resolution reads: a change re-links the
    /// branch in full (DD-6). An environment with only TypeScript parts has
    /// the fingerprint [`ScriptEnv::fingerprint`] gives it.
    pub fn fingerprint(&self) -> String {
        format!(
            "{:016x}",
            devctx_core::symbol_id::fnv1a64(&[&self.to_json()])
        )
    }

    /// As stored in `index_meta`.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Back from [`LinkEnv::to_json`] (or a [`ScriptEnv`]'s).
    pub fn from_json(text: &str) -> Option<Self> {
        serde_json::from_str(text).ok()
    }

    /// Put every list in path order, so the same files give the same
    /// fingerprint whatever order they were read in.
    pub fn sort(&mut self) {
        self.script.manifests.sort_by(|a, b| a.dir.cmp(&b.dir));
        self.cargo.sort_by(|a, b| a.path.cmp(&b.path));
        self.go.sort_by(|a, b| a.path.cmp(&b.path));
        self.python.sort_by(|a, b| a.path.cmp(&b.path));
    }
}

/// Which reader a repository path is for, by its file name; `None` for a
/// file that is no manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestKind {
    /// `package.json`.
    Npm,
    /// `Cargo.toml`.
    Cargo,
    /// `go.mod`.
    GoMod,
    /// `pyproject.toml`.
    PyProject,
    /// `setup.cfg`.
    SetupCfg,
    /// `requirements*.txt`.
    Requirements,
}

impl ManifestKind {
    /// The kind of `path`, by its file name.
    pub fn of(path: &str) -> Option<Self> {
        let name = path.rsplit('/').next().unwrap_or(path);
        Some(match name {
            "package.json" => Self::Npm,
            "Cargo.toml" => Self::Cargo,
            "go.mod" => Self::GoMod,
            "pyproject.toml" => Self::PyProject,
            "setup.cfg" => Self::SetupCfg,
            n if n.starts_with("requirements") && n.ends_with(".txt") => Self::Requirements,
            _ => return None,
        })
    }
}

/// A `Cargo.toml` of the repository.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CargoManifest {
    /// Its repository path.
    pub path: String,
    /// Its directory (`""` for the root).
    pub dir: String,
    /// The crate's code name — `[lib] name`, else `[package] name` — with
    /// `-` as `_` (what a `use` writes); `None` for a manifest with no
    /// `[package]` (a virtual workspace root).
    pub krate: Option<String>,
    /// The keys of its dependency tables (`[dependencies]`,
    /// `[dev-dependencies]`, `[build-dependencies]`, per target, and
    /// `[workspace.dependencies]`), `-` as `_`, sorted: the names its code
    /// may `use` from outside. A renamed dependency (`foo = { package =
    /// "bar" }`) is named by its key, as the code does.
    pub deps: Vec<String>,
    /// Of those, the path dependencies: code name → the repository
    /// directory of the crate it names (`foo = { path = "../bar", package =
    /// "bar" }` → `foo` → `crates/bar`), sorted (TASK-007 review, R1).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub local: Vec<(String, String)>,
    /// Of those, the ones inherited from the workspace (`foo.workspace =
    /// true`): the workspace root's entry says what they are.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inherited: Vec<String>,
}

impl CargoManifest {
    /// Read a `Cargo.toml` at the repository path `path`; `None` when it
    /// does not parse.
    pub fn parse(text: &str, path: &str) -> Option<Self> {
        let entries = toml_entries(text)?;
        let dir = dir_of(path).to_string();
        let (mut name, mut lib) = (None, None);
        let mut deps = Vec::new();
        let mut local: Vec<(String, String)> = Vec::new();
        let mut inherited: Vec<String> = Vec::new();
        let mut note = |dep: &str, key: &[&str], value: &TomlValue| {
            let code = dep.replace('-', "_");
            deps.push(code.clone());
            // `foo = { path = … }` / `foo.path = …` / `[dependencies.foo]`
            // `path = …`; `workspace = true` likewise.
            let mut fields: Vec<(String, TomlValue)> = Vec::new();
            match (key, value) {
                ([], TomlValue::Table(t)) => fields.extend(t.iter().cloned()),
                ([k], v) => fields.push((k.to_string(), v.clone())),
                _ => {}
            }
            for (k, v) in fields {
                match (k.as_str(), &v) {
                    ("path", TomlValue::Str(p)) => local.push((code.clone(), join(&dir, p))),
                    ("workspace", TomlValue::Bool(true)) => inherited.push(code.clone()),
                    _ => {}
                }
            }
        };
        for e in &entries {
            let table: Vec<&str> = e.table.iter().map(String::as_str).collect();
            let key: Vec<&str> = e.key.iter().map(String::as_str).collect();
            match (table.as_slice(), key.as_slice()) {
                (["package"], ["name"]) => name = e.value.string(),
                (["lib"], ["name"]) => lib = e.value.string(),
                // `[dependencies]` `foo = "1"`, `foo.workspace = true`.
                (t, [dep, rest @ ..]) if is_dependency_table(t) => note(dep, rest, &e.value),
                // `[dependencies.foo]` `version = "1"`.
                ([t @ .., dep], k) if is_dependency_table(t) => note(dep, k, &e.value),
                _ => {}
            }
        }
        // A `[dependencies.foo]` header with no key under it is a dependency
        // too.
        for t in toml_tables(text) {
            let t: Vec<&str> = t.iter().map(String::as_str).collect();
            if let [head @ .., dep] = t.as_slice() {
                if is_dependency_table(head) {
                    deps.push(dep.replace('-', "_"));
                }
            }
        }
        deps.sort();
        deps.dedup();
        local.sort();
        local.dedup();
        inherited.sort();
        inherited.dedup();
        let krate = lib.or(name).map(|n| n.replace('-', "_"));
        Some(Self {
            path: path.to_string(),
            dir,
            krate,
            deps,
            local,
            inherited,
        })
    }
}

/// `[dependencies]`, `[dev-dependencies]`, `[build-dependencies]`,
/// `[target.<cfg>.dependencies]` (and the others under a target),
/// `[workspace.dependencies]`.
fn is_dependency_table(t: &[&str]) -> bool {
    let deps = |s: &str| {
        matches!(
            s,
            "dependencies" | "dev-dependencies" | "build-dependencies"
        )
    };
    match t {
        [d] => deps(d),
        ["workspace", "dependencies"] => true,
        ["target", _, d] => deps(d),
        _ => false,
    }
}

/// A `go.mod` of the repository.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoModule {
    /// Its repository path.
    pub path: String,
    /// Its directory (`""` for the root): where the module's packages are.
    pub dir: String,
    /// The `module` path: the import prefix of its packages.
    pub module: Option<String>,
    /// The module paths it `require`s, sorted.
    pub requires: Vec<String>,
    /// `replace` directives to a local directory: the module path and the
    /// repository directory it is served from.
    pub local: Vec<(String, String)>,
}

impl GoModule {
    /// Read a `go.mod` at the repository path `path`; `None` when it does
    /// not parse.
    pub fn parse(text: &str, path: &str) -> Option<Self> {
        let dir = dir_of(path).to_string();
        let mut out = Self {
            path: path.to_string(),
            dir: dir.clone(),
            ..Default::default()
        };
        let mut block: Option<String> = None;
        for raw in text.trim_start_matches('\u{feff}').lines() {
            let line = raw.split("//").next().unwrap_or_default().trim();
            if line.is_empty() {
                continue;
            }
            if conflict_marker(line) {
                return None;
            }
            if let Some(b) = block.clone() {
                if line == ")" {
                    block = None;
                    continue;
                }
                out.directive(&b, line, &dir)?;
                continue;
            }
            let (word, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
            let rest = rest.trim();
            if rest == "(" {
                block = Some(word.to_string());
                continue;
            }
            out.directive(word, rest, &dir)?;
        }
        if block.is_some() {
            return None;
        }
        out.requires.sort();
        out.requires.dedup();
        out.local.sort();
        Some(out)
    }

    /// One directive (`module x`, `require x v1`, `replace a => ./b`).
    fn directive(&mut self, word: &str, rest: &str, dir: &str) -> Option<()> {
        let first = rest.split_whitespace().next().map(unquote_go);
        match word {
            "module" => self.module = Some(first?.to_string()),
            "require" => self.requires.push(first?.to_string()),
            "replace" => {
                let (from, to) = rest.split_once("=>")?;
                let from = from.split_whitespace().next().map(unquote_go)?;
                let to = to.split_whitespace().next().map(unquote_go)?;
                if to.starts_with("./") || to.starts_with("../") || to == "." {
                    self.local.push((from.to_string(), join(dir, to)));
                }
            }
            "go" | "toolchain" | "exclude" | "retract" | "godebug" | "tool" | "ignore" => {}
            _ => return None,
        }
        Some(())
    }
}

fn unquote_go(s: &str) -> &str {
    s.trim_matches(|c| c == '"' || c == '`')
}

/// A Python manifest of the repository: the distributions it declares, by
/// the name their modules are imported under (best effort), and the
/// project's own name (a module of the repository, not a dependency).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PyManifest {
    /// Its repository path.
    pub path: String,
    /// Its directory (`""` for the root).
    pub dir: String,
    /// `[project] name` / `[tool.poetry] name` / `[metadata] name`, as an
    /// import name.
    pub name: Option<String>,
    /// The import names of the declared distributions, sorted.
    pub deps: Vec<String>,
}

/// Distributions whose top-level module is not their normalised name.
const PY_IMPORT_NAMES: &[(&str, &str)] = &[
    ("attrs", "attr"),
    ("beautifulsoup4", "bs4"),
    ("google_api_python_client", "googleapiclient"),
    ("msgpack_python", "msgpack"),
    ("mysqlclient", "MySQLdb"),
    ("opencv_contrib_python", "cv2"),
    ("opencv_python", "cv2"),
    ("opencv_python_headless", "cv2"),
    ("pillow", "PIL"),
    ("protobuf", "google"),
    ("pycryptodome", "Crypto"),
    ("pyjwt", "jwt"),
    ("pymupdf", "fitz"),
    ("pyopenssl", "OpenSSL"),
    ("python_dateutil", "dateutil"),
    ("python_dotenv", "dotenv"),
    ("pyyaml", "yaml"),
    ("scikit_image", "skimage"),
    ("scikit_learn", "sklearn"),
];

/// The import names a distribution is known by: its name normalised
/// (lowercase, `-`/`.` as `_`), a known alias (`PyYAML` → `yaml`), and the
/// name without a `python_` prefix or a `_binary` suffix
/// (`psycopg2-binary` → `psycopg2`).
pub fn py_import_names(dist: &str) -> Vec<String> {
    let norm: String = dist
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c == '-' || c == '.' { '_' } else { c })
        .collect();
    if norm.is_empty() {
        return Vec::new();
    }
    let mut out = vec![norm.clone()];
    for (d, m) in PY_IMPORT_NAMES {
        if *d == norm {
            out.push(m.to_string());
        }
    }
    if let Some(rest) = norm.strip_prefix("python_") {
        out.push(rest.to_string());
    }
    if let Some(rest) = norm.strip_suffix("_binary") {
        out.push(rest.to_string());
    }
    out
}

/// The distribution a PEP 508 requirement names (`requests[socks]>=2; …` →
/// `requests`); `None` for an option line, a path or a URL.
fn requirement_name(line: &str) -> Option<&str> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('-') || line.starts_with('.') || line.contains("://") {
        return None;
    }
    let end = line
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
        .unwrap_or(line.len());
    let name = &line[..end];
    (!name.is_empty()
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric()))
    .then_some(name)
}

impl PyManifest {
    /// Read a Python manifest at the repository path `path` (its kind by
    /// file name); `None` when it does not parse.
    pub fn parse(text: &str, path: &str) -> Option<Self> {
        let mut out = Self {
            path: path.to_string(),
            dir: dir_of(path).to_string(),
            ..Default::default()
        };
        let mut dists: Vec<String> = Vec::new();
        match ManifestKind::of(path)? {
            ManifestKind::Requirements => {
                for raw in text.trim_start_matches('\u{feff}').lines() {
                    let line = raw.split(" #").next().unwrap_or_default();
                    let line = if line.trim_start().starts_with('#') {
                        ""
                    } else {
                        line
                    };
                    if conflict_marker(line.trim()) {
                        return None;
                    }
                    dists.extend(requirement_name(line).map(str::to_string));
                }
            }
            ManifestKind::PyProject => {
                let entries = toml_entries(text)?;
                for e in &entries {
                    let table: Vec<&str> = e.table.iter().map(String::as_str).collect();
                    let key: Vec<&str> = e.key.iter().map(String::as_str).collect();
                    match (table.as_slice(), key.as_slice()) {
                        (["project"], ["name"]) | (["tool", "poetry"], ["name"]) => {
                            out.name = e.value.string();
                        }
                        (["project"], ["dependencies"])
                        | (["project", "optional-dependencies"], [_])
                        | (["dependency-groups"], [_]) => {
                            for r in e.value.strings() {
                                dists.extend(requirement_name(&r).map(str::to_string));
                            }
                        }
                        (["tool", "poetry", "dependencies"], [d, ..])
                        | (["tool", "poetry", "dev-dependencies"], [d, ..])
                        | (["tool", "poetry", "group", _, "dependencies"], [d, ..])
                            if *d != "python" =>
                        {
                            dists.push(d.to_string());
                        }
                        _ => {}
                    }
                }
            }
            ManifestKind::SetupCfg => {
                let mut section = String::new();
                let mut in_list = false;
                for raw in text.trim_start_matches('\u{feff}').lines() {
                    let trimmed = raw.trim();
                    if conflict_marker(trimmed) {
                        return None;
                    }
                    if trimmed.starts_with('#') || trimmed.starts_with(';') {
                        continue;
                    }
                    if trimmed.starts_with('[') && trimmed.ends_with(']') {
                        section = trimmed[1..trimmed.len() - 1].trim().to_string();
                        in_list = false;
                        continue;
                    }
                    let indented = raw.starts_with(' ') || raw.starts_with('\t');
                    if in_list && indented {
                        dists.extend(requirement_name(trimmed).map(str::to_string));
                        continue;
                    }
                    in_list = false;
                    let Some((k, v)) = trimmed.split_once('=') else {
                        continue;
                    };
                    let (k, v) = (k.trim(), v.trim());
                    match (section.as_str(), k) {
                        ("metadata", "name") => out.name = Some(v.to_string()),
                        ("options", "install_requires") => {
                            in_list = true;
                            dists.extend(requirement_name(v).map(str::to_string));
                        }
                        ("options.extras_require", _) => {
                            in_list = true;
                            dists.extend(requirement_name(v).map(str::to_string));
                        }
                        _ => {}
                    }
                }
            }
            _ => return None,
        }
        out.name = out
            .name
            .take()
            .and_then(|n| py_import_names(&n).into_iter().next());
        let mut deps: Vec<String> = dists.iter().flat_map(|d| py_import_names(d)).collect();
        deps.sort();
        deps.dedup();
        out.deps = deps;
        Some(out)
    }
}

/// A merge conflict left in the file.
fn conflict_marker(line: &str) -> bool {
    line.starts_with("<<<<<<<") || line.starts_with(">>>>>>>") || line == "======="
}

// ------------------------------------------------------- a TOML subset

/// A value as far as the manifests need it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum TomlValue {
    Str(String),
    /// The strings of an array (other elements dropped).
    Array(Vec<String>),
    Bool(bool),
    /// An inline table's keys (dotted ones joined by `.`) and values.
    Table(Vec<(String, TomlValue)>),
    /// A number, a date.
    Other,
}

impl TomlValue {
    fn string(&self) -> Option<String> {
        match self {
            TomlValue::Str(s) => Some(s.clone()),
            _ => None,
        }
    }
    fn strings(&self) -> Vec<String> {
        match self {
            TomlValue::Array(a) => a.clone(),
            TomlValue::Str(s) => vec![s.clone()],
            _ => Vec::new(),
        }
    }
}

/// One `key = value` and the table it is in.
#[derive(Debug, Clone)]
struct TomlEntry {
    table: Vec<String>,
    key: Vec<String>,
    value: TomlValue,
}

/// The tables a TOML text opens (`[a.b]`, `[[c]]`), in order.
fn toml_tables(text: &str) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    let mut lexer = Lexer::new(text);
    while let Some(line) = lexer.next_logical() {
        if let Some(t) = table_header(&line) {
            out.push(t);
        }
    }
    out
}

/// Every `key = value` of a TOML text, with its table: enough of TOML for a
/// manifest (strings, arrays over several lines, inline tables, dotted and
/// quoted keys, comments). `None` when a line is none of these — the file
/// is not read rather than half read.
fn toml_entries(text: &str) -> Option<Vec<TomlEntry>> {
    let mut out = Vec::new();
    let mut table: Vec<String> = Vec::new();
    let mut lexer = Lexer::new(text);
    while let Some(line) = lexer.next_logical() {
        if lexer.broken {
            return None;
        }
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if conflict_marker(t) {
            return None;
        }
        if t.starts_with('[') {
            table = table_header(t)?;
            continue;
        }
        let (key, value) = split_key_value(t)?;
        out.push(TomlEntry {
            table: table.clone(),
            key: parse_key(key)?,
            value: parse_value(value)?,
        });
    }
    if lexer.broken {
        return None;
    }
    Some(out)
}

/// Joins a TOML text into logical lines: comments out, an array or an inline
/// table that spans lines joined into one, a multi-line string kept whole.
struct Lexer<'a> {
    lines: std::str::Lines<'a>,
    broken: bool,
}

impl<'a> Lexer<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            lines: text.trim_start_matches('\u{feff}').lines(),
            broken: false,
        }
    }

    fn next_logical(&mut self) -> Option<String> {
        let mut acc = String::new();
        let mut depth = 0i32;
        let mut multi: Option<&'static str> = None;
        loop {
            let Some(raw) = self.lines.next() else {
                if !acc.is_empty() && (depth != 0 || multi.is_some()) {
                    self.broken = true;
                }
                return (!acc.is_empty()).then_some(acc);
            };
            let (clean, d, m) = strip_line(raw, multi);
            depth += d;
            multi = m;
            if !acc.is_empty() {
                acc.push(' ');
            }
            acc.push_str(&clean);
            if depth <= 0 && multi.is_none() {
                return Some(acc);
            }
        }
    }
}

/// A line without its comment, the change of bracket depth it makes
/// outside strings, and whether a multi-line string is still open after it
/// (its delimiter).
fn strip_line(raw: &str, mut multi: Option<&'static str>) -> (String, i32, Option<&'static str>) {
    let mut out = String::with_capacity(raw.len());
    let mut depth = 0;
    let chars: Vec<char> = raw.chars().collect();
    let mut i = 0;
    let mut quote: Option<char> = None;
    while i < chars.len() {
        let c = chars[i];
        if let Some(delim) = multi {
            let d: Vec<char> = delim.chars().collect();
            if chars[i..].starts_with(&d) {
                multi = None;
                out.push_str(delim);
                i += 3;
            } else {
                // Content of a multi-line string: nothing a manifest reads.
                i += 1;
            }
            continue;
        }
        if let Some(q) = quote {
            out.push(c);
            if c == '\\' && q == '"' && i + 1 < chars.len() {
                out.push(chars[i + 1]);
                i += 2;
                continue;
            }
            if c == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        match c {
            '#' => break,
            '"' | '\'' => {
                let triple: String = chars[i..chars.len().min(i + 3)].iter().collect();
                if triple == "\"\"\"" || triple == "'''" {
                    multi = Some(if c == '"' { "\"\"\"" } else { "'''" });
                    out.push_str(&triple);
                    i += 3;
                    continue;
                }
                quote = Some(c);
                out.push(c);
            }
            '[' | '{' => {
                depth += 1;
                out.push(c);
            }
            ']' | '}' => {
                depth -= 1;
                out.push(c);
            }
            _ => out.push(c),
        }
        i += 1;
    }
    // A table header's brackets close on its own line.
    let t = out.trim_start();
    if t.starts_with('[') && !t.contains('=') {
        return (out, 0, multi);
    }
    (out, depth, multi)
}

/// `[a.b]` / `[[a.b]]` → `["a", "b"]`; quoted segments unquoted.
fn table_header(line: &str) -> Option<Vec<String>> {
    let t = line.trim();
    let inner = t
        .strip_prefix("[[")
        .and_then(|r| r.strip_suffix("]]"))
        .or_else(|| t.strip_prefix('[').and_then(|r| r.strip_suffix(']')))?;
    parse_key(inner)
}

/// `key = value` at the first `=` outside quotes.
fn split_key_value(t: &str) -> Option<(&str, &str)> {
    let mut quote: Option<char> = None;
    for (i, c) in t.char_indices() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            (None, '=') => return Some((t[..i].trim(), t[i + 1..].trim())),
            _ => {}
        }
    }
    None
}

/// A dotted key, each segment bare (`a-b_c`) or quoted.
fn parse_key(key: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut rest = key.trim();
    while !rest.is_empty() {
        let (seg, after) = if let Some(r) = rest.strip_prefix('"') {
            let end = r.find('"')?;
            (r[..end].to_string(), &r[end + 1..])
        } else if let Some(r) = rest.strip_prefix('\'') {
            let end = r.find('\'')?;
            (r[..end].to_string(), &r[end + 1..])
        } else {
            let end = rest.find('.').unwrap_or(rest.len());
            let seg = rest[..end].trim();
            if seg.is_empty()
                || !seg
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                return None;
            }
            (seg.to_string(), &rest[end..])
        };
        out.push(seg);
        let after = after.trim_start();
        rest = match after.strip_prefix('.') {
            Some(r) => r.trim_start(),
            None if after.is_empty() => "",
            None => return None,
        };
    }
    (!out.is_empty()).then_some(out)
}

/// A string (basic or literal), an array (its strings), or anything else
/// TOML allows as a value.
fn parse_value(v: &str) -> Option<TomlValue> {
    let v = v.trim();
    if v.is_empty() {
        return None;
    }
    if let Some(s) = parse_string(v) {
        return Some(TomlValue::Str(s));
    }
    if v.starts_with("\"\"\"") || v.starts_with("'''") {
        return Some(TomlValue::Other);
    }
    if v.starts_with('[') {
        if !v.ends_with(']') {
            return None;
        }
        let inner = &v[1..v.len() - 1];
        let mut out = Vec::new();
        for item in split_top(inner) {
            let item = item.trim();
            if item.is_empty() {
                continue;
            }
            if let Some(s) = parse_string(item) {
                out.push(s);
            }
        }
        return Some(TomlValue::Array(out));
    }
    if let Some(inner) = v.strip_prefix('{') {
        let inner = inner.strip_suffix('}')?;
        let mut out = Vec::new();
        for item in split_top(inner) {
            let item = item.trim();
            if item.is_empty() {
                continue;
            }
            let (k, val) = split_key_value(item)?;
            out.push((parse_key(k)?.join("."), parse_value(val)?));
        }
        return Some(TomlValue::Table(out));
    }
    match v {
        "true" => return Some(TomlValue::Bool(true)),
        "false" => return Some(TomlValue::Bool(false)),
        _ => {}
    }
    let bare = v
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+' | '_' | ':' | ' '));
    bare.then_some(TomlValue::Other)
}

/// A whole basic (`"…"`) or literal (`'…'`) string.
fn parse_string(v: &str) -> Option<String> {
    if v.starts_with("\"\"\"") || v.starts_with("'''") {
        return None;
    }
    if let Some(r) = v.strip_prefix('\'') {
        let inner = r.strip_suffix('\'')?;
        return (!inner.contains('\'')).then(|| inner.to_string());
    }
    let r = v.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = r.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push(chars.next()?),
            '"' => return chars.as_str().trim().is_empty().then_some(out),
            c => out.push(c),
        }
    }
    None
}

/// `inner` split at commas outside strings, brackets and braces.
fn split_top(inner: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut start = 0;
    let mut prev = '\0';
    for (i, c) in inner.char_indices() {
        match quote {
            Some(q) if c == q && prev != '\\' => quote = None,
            Some(_) => {}
            None => match c {
                '"' | '\'' => quote = Some(c),
                '[' | '{' => depth += 1,
                ']' | '}' => depth -= 1,
                ',' if depth == 0 => {
                    out.push(&inner[start..i]);
                    start = i + 1;
                }
                _ => {}
            },
        }
        prev = c;
    }
    out.push(&inner[start..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cargo_manifest_names_its_crate_and_its_dependencies() {
        let text = r#"
[package]
name = "my-crate" # the published name
version = "0.1.0"

[lib]
name = "my_lib"

[dependencies]
serde = { version = "1", features = ["derive"] }
serde-json = "1"
tokio.workspace = true
renamed = { package = "real-name", version = "2" }

[dependencies.log]
version = "0.4"

[dev-dependencies]
tempfile = "3"

[target.'cfg(unix)'.dependencies]
libc = "0.2"

[features]
default = ["a", "b"]
"#;
        let m = CargoManifest::parse(text, "crates/x/Cargo.toml").unwrap();
        assert_eq!(m.dir, "crates/x");
        assert_eq!(m.krate.as_deref(), Some("my_lib"));
        assert_eq!(
            m.deps,
            [
                "libc",
                "log",
                "renamed",
                "serde",
                "serde_json",
                "tempfile",
                "tokio"
            ]
        );
        let ws = CargoManifest::parse(
            "[workspace]\nmembers = [\n  \"crates/*\",\n]\n\n[workspace.dependencies]\nanyhow = \"1\"\n",
            "Cargo.toml",
        )
        .unwrap();
        assert_eq!((ws.krate, ws.deps), (None, vec!["anyhow".to_string()]));
        let paths = CargoManifest::parse(
            "[package]\nname = \"app\"\n[dependencies]\nfoo = { package = \"bar\", path = \"../bar\" }\nbaz.workspace = true\nqux = { workspace = true }\n[dependencies.near]\npath = \"near\"\n",
            "crates/app/Cargo.toml",
        )
        .unwrap();
        assert_eq!(
            paths.local,
            [
                ("foo".to_string(), "crates/bar".to_string()),
                ("near".to_string(), "crates/app/near".to_string())
            ]
        );
        assert_eq!(paths.inherited, ["baz", "qux"]);
        let named = CargoManifest::parse("[package]\nname = \"a-b\"\n", "a/Cargo.toml").unwrap();
        assert_eq!(named.krate.as_deref(), Some("a_b"));
    }

    #[test]
    fn a_broken_cargo_manifest_is_not_read() {
        assert!(CargoManifest::parse("[package\nname = 1", "Cargo.toml").is_none());
        assert!(CargoManifest::parse(
            "[package]\n<<<<<<< HEAD\nname = \"a\"\n=======\nname = \"b\"\n>>>>>>> x\n",
            "Cargo.toml"
        )
        .is_none());
        assert!(CargoManifest::parse("[dependencies]\nserde = [\n", "Cargo.toml").is_none());
    }

    #[test]
    fn a_go_module_names_its_prefix_requires_and_local_replaces() {
        let text = "module github.com/acme/demo // the module\n\ngo 1.22\n\nrequire (\n\tgithub.com/pkg/errors v0.9.1\n\tgolang.org/x/sync v0.7.0 // indirect\n)\n\nrequire example.com/one v1.0.0\n\nreplace example.com/lib => ../lib\nreplace (\n\texample.com/far v1 => example.com/fork v2\n)\n";
        let m = GoModule::parse(text, "svc/go.mod").unwrap();
        assert_eq!(m.module.as_deref(), Some("github.com/acme/demo"));
        assert_eq!(
            m.requires,
            [
                "example.com/one",
                "github.com/pkg/errors",
                "golang.org/x/sync"
            ]
        );
        assert_eq!(
            m.local,
            [("example.com/lib".to_string(), "lib".to_string())]
        );
        assert!(GoModule::parse("module a\nrequire (\n", "go.mod").is_none());
        assert!(GoModule::parse("<<<<<<< HEAD\nmodule a\n", "go.mod").is_none());
        assert!(GoModule::parse("modle a\n", "go.mod").is_none());
    }

    #[test]
    fn python_manifests_declare_import_names() {
        let req = PyManifest::parse(
            "# pinned\nrequests[socks]>=2.31\nhttpx>=0.27\nPyYAML>=6.0 ; python_version>'3'\n-r base.txt\n-e .\ngit+https://x/y.git\npython-dateutil==2.9  # dates\n",
            "requirements.txt",
        )
        .unwrap();
        assert!(req.deps.contains(&"requests".to_string()));
        assert!(req.deps.contains(&"httpx".to_string()));
        assert!(req.deps.contains(&"yaml".to_string()), "{:?}", req.deps);
        assert!(req.deps.contains(&"dateutil".to_string()));
        assert!(!req.deps.iter().any(|d| d.contains("base") || d == "git"));
        let pp = PyManifest::parse(
            "[project]\nname = \"my-tool\"\ndependencies = [\n  \"requests>=2\",\n  \"beautifulsoup4\",\n]\n\n[project.optional-dependencies]\ntest = [\"pytest\"]\n\n[tool.poetry.dependencies]\npython = \"^3.11\"\nhttpx = \"*\"\n",
            "pyproject.toml",
        )
        .unwrap();
        assert_eq!(pp.name.as_deref(), Some("my_tool"));
        for d in ["requests", "bs4", "pytest", "httpx"] {
            assert!(pp.deps.contains(&d.to_string()), "{d}: {:?}", pp.deps);
        }
        assert!(!pp.deps.contains(&"python".to_string()));
        let cfg = PyManifest::parse(
            "[metadata]\nname = pkg\n\n[options]\ninstall_requires =\n    click>=8\n    rich\npackages = find:\n",
            "setup.cfg",
        )
        .unwrap();
        assert_eq!(cfg.deps, ["click", "rich"]);
        assert!(PyManifest::parse("[project\n", "pyproject.toml").is_none());
        assert!(PyManifest::parse("<<<<<<< HEAD\nx\n", "requirements.txt").is_none());
    }

    #[test]
    fn an_environment_stored_before_task_007_reads_back() {
        let old = r#"{"tsconfig":null,"manifests":[{"dir":"","name":null,"deps":["rxjs"]}]}"#;
        let env = LinkEnv::from_json(old).unwrap();
        assert_eq!(env.script.manifests[0].deps, ["rxjs"]);
        assert!(env.cargo.is_empty());
        // Only TypeScript parts: the same JSON, so the same fingerprint, as
        // the script environment alone.
        assert_eq!(env.to_json(), env.script.to_json());
        assert_eq!(env.fingerprint(), env.script.fingerprint());
    }

    #[test]
    fn manifest_kinds_by_file_name() {
        assert_eq!(ManifestKind::of("a/Cargo.toml"), Some(ManifestKind::Cargo));
        assert_eq!(ManifestKind::of("go.mod"), Some(ManifestKind::GoMod));
        assert_eq!(
            ManifestKind::of("requirements-dev.txt"),
            Some(ManifestKind::Requirements)
        );
        assert_eq!(ManifestKind::of("x/package.json"), Some(ManifestKind::Npm));
        assert_eq!(ManifestKind::of("README.md"), None);
    }
}
