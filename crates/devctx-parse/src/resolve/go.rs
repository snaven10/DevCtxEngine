//! Go (PLAN-009 TASK-007, DD-7, basic per Q-6): what a file declares about
//! itself, the type a binding takes, and the link pass's rules — packages by
//! directory, imports under the `module` of `go.mod` (or a local `replace`)
//! as the repository's, the rest from outside, methods by their receiver.

use tree_sitter::Node;

use super::link::{
    cap, external, hi, is_callable, is_type, undecided, Call, LinkEdge, Outcome, RepoIndex,
    Resolved, TypeRef, ValueType,
};
use super::scope::{TypeText, Via};
use super::typescript::{dir_of, join};
use super::{compact, LangResolver};
use crate::facts::ImportFact;

/// Go.
pub struct Go;

impl LangResolver for Go {
    fn package_from_path(&self, _path: &str) -> Option<String> {
        None // `package x` in the source
    }

    fn exported(&self, _def: Node<'_>, name: &str, _bytes: &[u8]) -> Option<bool> {
        Some(name.chars().next().is_some_and(char::is_uppercase))
    }

    fn import_target(&self, imp: &ImportFact) -> String {
        imp.path.clone()
    }

    /// A bare name is never a method: the receiver reaches one.
    fn implicit_this(&self) -> bool {
        false
    }

    /// Fields are declared on the type, apart from the methods.
    fn fields_by_link(&self) -> bool {
        true
    }

    fn literal_type(&self, kind: &str) -> Option<&'static str> {
        Some(match kind {
            "interpreted_string_literal" | "raw_string_literal" => "string",
            "int_literal" => "int",
            "float_literal" => "float64",
            "rune_literal" => "rune",
            _ => return None,
        })
    }

    fn type_text(&self, node: Node<'_>, bytes: &[u8]) -> Option<TypeText> {
        type_text(&compact(node, bytes))
    }

    /// `x := New(…)` / `pkg.New(…)` is what the call returns (`New()`), a
    /// conversion `T(x)` its type; `&T{}` and `T{}` are `T`.
    fn init_type(&self, init: Node<'_>, bytes: &[u8]) -> Option<(TypeText, Via)> {
        let typed = |base: String| Some((TypeText { base, elem: None }, Via::Local));
        match init.kind() {
            "call_expression" => {
                let f = init.child_by_field_name("function")?;
                if !matches!(f.kind(), "identifier" | "selector_expression") {
                    return None;
                }
                let text = compact(f, bytes);
                dotted(&text).then(|| format!("{text}()")).and_then(typed)
            }
            "unary_expression" | "parenthesized_expression" => {
                let inner = init
                    .child_by_field_name("operand")
                    .or_else(|| init.named_child(0))?;
                self.init_type(inner, bytes)
            }
            "composite_literal" => {
                let t = init.child_by_field_name("type")?;
                type_text(&compact(t, bytes)).map(|t| (t, Via::Local))
            }
            k => self.literal_type(k).and_then(|t| typed(t.into())),
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

/// A Go type as written: no pointer; generic arguments out (`Svc[T]` →
/// `Svc`); `pkg.T` as is. A slice, a map, a function or an inline struct
/// names no type of the repository.
pub fn type_text(text: &str) -> Option<TypeText> {
    let t = text.trim().trim_start_matches('*');
    let t = t.split('[').next().unwrap_or(t);
    dotted(t).then(|| TypeText {
        base: t.to_string(),
        elem: None,
    })
}

/// The return type a Go signature declares (`func NewServer() *Server` →
/// `Server`, `func Load() (*Config, error)` → `Config`); `None` when it
/// declares none.
pub fn return_type(signature: &str) -> Option<String> {
    let rest = signature.trim().strip_prefix("func")?.trim_start();
    // The receiver of a method.
    let rest = if rest.starts_with('(') {
        &rest[group_end(rest)?..]
    } else {
        rest
    };
    let params = rest.find('(')?;
    let after = rest[params..].trim_start();
    let result = after[group_end(after)?..].trim();
    let result = result.trim_end_matches('{').trim();
    if result.is_empty() {
        return None;
    }
    let first = if let Some(inner) = result.strip_prefix('(') {
        let inner = inner.strip_suffix(')').unwrap_or(inner);
        let first = inner.split(',').next()?.trim();
        // A named result (`(cfg *Config, err error)`): its type.
        first.rsplit(' ').next().unwrap_or(first)
    } else {
        result
    };
    type_text(first).map(|t| t.base)
}

/// The byte after the balanced group that opens `text`.
fn group_end(text: &str) -> Option<usize> {
    let mut depth = 0usize;
    for (i, c) in text.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// A field's declared type (`store *store.Store` → `store.Store`).
pub fn field_type(signature: &str) -> Option<String> {
    let mut parts = signature.split_whitespace();
    parts.next()?;
    type_text(parts.next()?).map(|t| t.base)
}

/// The name an import path binds without an alias: its last segment, a
/// major-version suffix left out (`…/v2`, `gopkg.in/yaml.v3`).
pub fn import_name(path: &str) -> &str {
    let mut segs: Vec<&str> = path.split('/').collect();
    let is_major = |s: &str| {
        s.strip_prefix('v')
            .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
    };
    if segs.len() > 1 && segs.last().is_some_and(|s| is_major(s)) {
        segs.pop();
    }
    let last = segs.last().copied().unwrap_or(path);
    match last.rsplit_once(".v") {
        Some((name, v)) if !v.is_empty() && v.chars().all(|c| c.is_ascii_digit()) => name,
        _ => last,
    }
}

// ------------------------------------------------------------- the link pass

/// Where an import path leads.
#[derive(Debug, Clone, PartialEq, Eq)]
enum GoPkg {
    /// A directory of the repository.
    Repo(String),
    /// The standard library or another module.
    External,
    /// No `go.mod` says.
    Unknown,
}

impl RepoIndex {
    /// Give every method defined beside its receiver type's file (another
    /// file of the package) that type as owner.
    pub(super) fn go_build(&mut self) {
        let methods: Vec<usize> = (0..self.syms.len())
            .filter(|&i| self.syms[i].kind == "method")
            .filter(|&i| self.lang_key(&self.syms[i].file) == Some("go"))
            .filter(|&i| {
                !self.syms[i]
                    .parent_id
                    .and_then(|p| self.by_id.get(&p))
                    .is_some_and(|&p| is_type(&self.syms[p].kind))
            })
            .collect();
        for m in methods {
            let Some((ty, _)) = self.syms[m].qualified.rsplit_once('.') else {
                continue;
            };
            let file = self.syms[m].file.clone();
            if let Some(t) = self.go_package_type(&file, ty) {
                self.owners.insert(m, t);
                let id = self.syms[t].id;
                self.add_child(id, m);
            }
        }
    }

    /// The top-level symbols named `name` of the package of `file` (its
    /// directory, its `package` clause).
    fn go_package_items(&self, file: &str, name: &str) -> Vec<usize> {
        let dir = dir_of(file);
        let pkg = self.files.get(file).and_then(|f| f.package.clone());
        self.go_dir_items(dir, pkg.as_deref(), name)
    }

    /// The top-level symbols named `name` of the Go files of `dir` (of
    /// package `pkg`, when given; else not an external test package).
    fn go_dir_items(&self, dir: &str, pkg: Option<&str>, name: &str) -> Vec<usize> {
        self.by_qualified
            .get(name)
            .into_iter()
            .flatten()
            .copied()
            .filter(|&i| {
                let f = &self.syms[i].file;
                dir_of(f) == dir
                    && self.lang_key(f) == Some("go")
                    && self.files.get(f).is_some_and(|info| {
                        let p = info.package.as_deref();
                        match pkg {
                            Some(want) => p == Some(want),
                            None => !p.is_some_and(|p| p.ends_with("_test")),
                        }
                    })
                    && self.syms[i].parent_id == self.files.get(f).and_then(|x| x.id)
            })
            .collect()
    }

    fn go_package_type(&self, file: &str, name: &str) -> Option<usize> {
        self.go_package_items(file, name)
            .into_iter()
            .find(|&i| is_type(&self.syms[i].kind))
    }

    /// Where an import path leads (DD-9): under a `go.mod`'s `module`, or a
    /// local `replace`, the repository's; a path whose first segment has no
    /// dot is the standard library; any other path is another module once a
    /// `go.mod` exists (Go builds nothing from outside its module without
    /// one), and undecided without.
    fn go_package(&self, path: &str) -> GoPkg {
        for m in &self.go {
            if let Some(module) = &m.module {
                if path == module {
                    return GoPkg::Repo(m.dir.clone());
                }
                if let Some(rest) = path.strip_prefix(&format!("{module}/")) {
                    return GoPkg::Repo(join(&m.dir, rest));
                }
            }
            for (from, to) in &m.local {
                if path == from {
                    return GoPkg::Repo(to.clone());
                }
                if let Some(rest) = path.strip_prefix(&format!("{from}/")) {
                    return GoPkg::Repo(join(to, rest));
                }
            }
        }
        let first = path.split('/').next().unwrap_or(path);
        if !first.contains('.') {
            return GoPkg::External;
        }
        if self.go.is_empty() {
            GoPkg::Unknown
        } else {
            GoPkg::External
        }
    }

    /// The package the local name `local` of `file` imports.
    fn go_import_of(&self, file: &str, local: &str) -> Option<GoPkg> {
        let info = self.files.get(file)?;
        let found = info
            .go
            .iter()
            .find(|(path, alias)| alias.as_deref().unwrap_or_else(|| import_name(path)) == local)?;
        Some(self.go_package(&found.0))
    }

    /// A Go type as written in `file` (`Server`, `store.Store`, `error`).
    pub(super) fn go_type(&self, name: &str, file: &str, _ctx: Option<usize>) -> TypeRef {
        if let Some((pkg, ty)) = name.split_once('.') {
            return match self.go_import_of(file, pkg) {
                Some(GoPkg::Repo(dir)) => match self
                    .go_dir_items(&dir, None, ty)
                    .into_iter()
                    .find(|&i| is_type(&self.syms[i].kind))
                {
                    Some(t) => TypeRef::Repo(t, "import"),
                    None => TypeRef::Unknown,
                },
                Some(GoPkg::External) => TypeRef::External,
                _ => TypeRef::Unknown,
            };
        }
        if let Some(t) = self.go_package_type(file, name) {
            let how = if self.syms[t].file == file {
                "same_file"
            } else {
                "same_package"
            };
            return TypeRef::Repo(t, how);
        }
        if self.platform(file, name) {
            return TypeRef::External;
        }
        TypeRef::Unknown
    }

    /// `f()` in Go: a function (or a conversion to a type) of the package,
    /// a builtin; nothing else is visible.
    pub(super) fn go_free(&self, c: &Call<'_>) -> Outcome {
        let items: Vec<usize> = self
            .go_package_items(c.file, c.callee)
            .into_iter()
            .filter(|&i| is_callable(&self.syms[i].kind) || is_type(&self.syms[i].kind))
            .collect();
        if let Some(&i) = items.first() {
            let how = if self.syms[i].file == c.file {
                "same_file"
            } else {
                "same_package"
            };
            let out = hi(i, self, how);
            return if items.len() > 1 {
                cap(out, "medium")
            } else {
                out
            };
        }
        if self.platform(c.file, c.callee) {
            return external("external_known");
        }
        undecided()
    }

    /// `p.f()` with `p` bound by no scope: an imported package's function,
    /// a type of the package (a method expression), something from outside.
    pub(super) fn go_name(&self, c: &Call<'_>, recv: &str) -> Outcome {
        if let Some(pkg) = self.go_import_of(c.file, recv) {
            return match pkg {
                GoPkg::Repo(dir) => match self
                    .go_dir_items(&dir, None, c.callee)
                    .into_iter()
                    .find(|&i| is_callable(&self.syms[i].kind) || is_type(&self.syms[i].kind))
                {
                    Some(i) => hi(i, self, "import"),
                    None => undecided(),
                },
                GoPkg::External => external("external_known"),
                GoPkg::Unknown => undecided(),
            };
        }
        if let Some(t) = self.go_package_type(c.file, recv) {
            return self.in_type(c, t, "same_package", "high");
        }
        self.untyped(c)
    }

    /// What calling `path` returns (`NewServer`, `store.New`): a function's
    /// declared result, a conversion's type; a call into another module, a
    /// value nothing types.
    pub(super) fn go_value(&self, path: &str, file: &str) -> ValueType {
        let found = match path.split_once('.') {
            Some((pkg, name)) => match self.go_import_of(file, pkg) {
                Some(GoPkg::Repo(dir)) => self.go_dir_items(&dir, None, name).into_iter().next(),
                Some(GoPkg::External) => return ValueType::FromExternalCall,
                _ => return ValueType::Unknown,
            },
            None => self.go_package_items(file, path).into_iter().next(),
        };
        let Some(i) = found else {
            return ValueType::Unknown;
        };
        if is_type(&self.syms[i].kind) {
            return ValueType::Repo(i, "local", "high");
        }
        let fs = &self.syms[i];
        let Some(ret) = fs.signature.as_deref().and_then(return_type) else {
            return ValueType::Unknown;
        };
        match self.go_type(&ret, &fs.file, Some(i)) {
            TypeRef::Repo(t, how) => ValueType::Repo(t, "return_type", self.type_conf(t, how)),
            TypeRef::External => ValueType::External("high"),
            TypeRef::Unknown => ValueType::Unknown,
        }
    }

    /// An `imports` row of a Go file: a package of the repository, or from
    /// outside.
    pub(super) fn go_import(&self, e: &LinkEdge) -> Outcome {
        match self.go_package(&e.dst_name) {
            GoPkg::Repo(_) => Outcome::Resolved(Resolved {
                dst_id: None,
                confidence: "high",
                resolution: "import",
                external: false,
            }),
            GoPkg::External => external("external_known"),
            GoPkg::Unknown => undecided(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_and_import_names() {
        assert_eq!(
            return_type("func NewServer() *Server").as_deref(),
            Some("Server")
        );
        assert_eq!(
            return_type("func (s *Store) Get(key string) string").as_deref(),
            Some("string")
        );
        assert_eq!(
            return_type("func Load(p string) (cfg *Config, err error)").as_deref(),
            Some("Config")
        );
        assert_eq!(
            return_type("func Open() (*store.Store, error)").as_deref(),
            Some("store.Store")
        );
        assert_eq!(return_type("func (s *Server) helper()"), None);
        assert_eq!(
            field_type("store *store.Store").as_deref(),
            Some("store.Store")
        );
        assert_eq!(import_name("github.com/a/b/v2"), "b");
        assert_eq!(import_name("gopkg.in/yaml.v3"), "yaml");
        assert_eq!(import_name("net/http"), "http");
    }
}
