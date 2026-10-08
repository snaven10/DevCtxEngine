//! Lexical scopes and the type of what a call's receiver names (PLAN-009
//! TASK-005, DD-7 rules 2-4).
//!
//! The `types` query of a language captures declarations — a field, a
//! parameter, a local, a for-each variable, a `this.x = x` in a constructor
//! — each with the name it binds and, when written, its type. Each binding
//! belongs to the innermost node of the language's `scopes` around its name:
//! a class body (so every inner class has its own fields), a method (its
//! parameters), a block, a lambda, a `for`. A receiver is looked up from where
//! it is used outwards, the first scope that binds the name wins, and a local
//! binds only after its declaration.
//!
//! A language without `scopes` has one scope, the file, where the first
//! declaration of a name wins and position does not matter: the flat
//! `type_map` it had before (TASK-006/007 give TypeScript, Python, Rust and Go
//! their scopes).

use std::collections::HashMap;

use tree_sitter::{Node, Query, QueryCursor, StreamingIterator};

use crate::lang::Lang;
use crate::resolve::LangResolver;

/// How a binding got its type: what `resolution` an edge typed by it gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    /// A field of the enclosing class (rule 3).
    Field,
    /// A parameter of the enclosing callable (rule 2).
    Param,
    /// A local, a for-each variable, a resource, a catch parameter (rule 2);
    /// also `var x = new T()` and `var x = (T) e`.
    Local,
    /// A field assigned from a constructor parameter, `this.x = x` (rule 4).
    CtorInject,
    /// `var x = T.factory(…)`: `T` only if the link pass finds the method
    /// called on `x` in `T` (DD-7: a static of a repository type).
    Static,
}

impl Via {
    /// The token the edge's hint carries (and the `resolution` it becomes).
    pub fn as_str(self) -> &'static str {
        match self {
            Via::Field => "field",
            Via::Param => "param",
            Via::Local => "local",
            Via::CtorInject => "ctor_inject",
            Via::Static => "static",
        }
    }
}

/// A type as written, reduced to what a lookup needs: the name without
/// generic arguments or whitespace (`List`, `a.b.Foo`, `Foo[]`) and, for a
/// container type, the element a for-each over it binds (`List<Foo>` and
/// `Foo[]` → `Foo`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeText {
    /// Without generic arguments.
    pub base: String,
    /// The first generic argument's base, or an array's element.
    pub elem: Option<String>,
}

impl TypeText {
    /// Parse a type as written (`java.util.List<? extends Foo>`, `Foo[]`).
    pub fn parse(text: &str) -> Option<Self> {
        let t: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        if t.is_empty() {
            return None;
        }
        // A TypeScript union with `null`/`undefined` is the other member
        // (`Foo | null` → `Foo`); any other union stays as written (no name).
        let members = top_level_split(&t, '|');
        if members.len() > 1 {
            let kept: Vec<&str> = members
                .into_iter()
                .filter(|m| !matches!(*m, "null" | "undefined" | "void" | ""))
                .collect();
            if let [one] = kept.as_slice() {
                return TypeText::parse(one);
            }
        }
        if let Some(open) = t.find('<') {
            let base = t[..open].to_string();
            let inner = &t[open + 1..t.rfind('>').unwrap_or(t.len())];
            let first = top_level_first(inner);
            let first = first
                .strip_prefix("?extends")
                .or_else(|| first.strip_prefix("?super"))
                .unwrap_or(first);
            let elem = TypeText::parse(first).map(|e| e.base).filter(|e| e != "?");
            return Some(Self { base, elem });
        }
        if let Some(element) = t.strip_suffix("[]") {
            return Some(Self {
                elem: Some(element.to_string()),
                base: t.clone(),
            });
        }
        Some(Self {
            base: t,
            elem: None,
        })
    }
}

/// `text` split at `sep` outside `<…>`, `(…)`, `[…]` and `{…}`.
fn top_level_split(text: &str, sep: char) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    for (i, c) in text.char_indices() {
        match c {
            '<' | '(' | '[' | '{' => depth += 1,
            '>' | ')' | ']' | '}' => depth = depth.saturating_sub(1),
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

/// The first comma-separated argument at depth 0 of `inner`.
fn top_level_first(inner: &str) -> &str {
    let mut depth = 0usize;
    for (i, c) in inner.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => return &inner[..i],
            _ => {}
        }
    }
    inner
}

/// One name a declaration binds.
#[derive(Debug, Clone)]
pub struct Binding {
    /// The bound name.
    pub name: String,
    /// Its type, when written or inferred; `None` for a declaration whose
    /// type nothing local says (`var q = em.createQuery(…)`).
    pub ty: Option<TypeText>,
    /// How it got its type.
    pub via: Via,
    /// Where the declaration starts: a local binds after it.
    pub at: usize,
    /// A member of the enclosing class (a field, a constructor parameter
    /// property, a field assigned from a constructor parameter): what `this.x`
    /// reads, and — in a language with an implicit `this` (Java) — a bare `x`.
    pub member: bool,
    /// A function of the file bound to the name (TypeScript/JavaScript: a
    /// `function f` — hoisted, so in scope from the start of its scope — or
    /// a `const f = () => …`): calling the name calls that symbol.
    pub func: bool,
    /// A Go method's receiver: the instance, as `self` is (TASK-007).
    pub this: bool,
    /// An untyped local whose value is a method call (`let x = a.f()`): the
    /// call's byte range, so a call on `x` is a chain after it (TASK-007
    /// review). Rust, Go and Python only (`fields_by_link`).
    pub init_call: Option<(usize, usize)>,
}

/// The bindings of a file, by scope.
pub struct Scopes {
    /// Scope node id → its bindings, in source order.
    by_scope: HashMap<usize, Vec<Binding>>,
    /// The language has `scopes` (else: one flat file scope, first wins).
    scoped: bool,
    /// A bare name can be a member of the enclosing class (Java); in
    /// TypeScript/JavaScript a member is only `this.x`.
    implicit_this: bool,
    /// The language's resolver (literal types, fields typed by the link).
    resolver: &'static dyn LangResolver,
    lang: Lang,
    root: usize,
}

/// A declaration whose type depends on another binding: resolved once the
/// explicit ones are known.
struct Pending<'t> {
    name: String,
    name_node: Node<'t>,
    scope: usize,
    via: Via,
    at: usize,
    init: Option<Node<'t>>,
    iter: Option<Node<'t>>,
    assign: bool,
    /// Python `self.x = v`: an attribute of the instance, assigned in a
    /// method (TASK-007).
    attr: bool,
    member: bool,
}

/// One capture of the `types` query.
struct Capture<'t> {
    pattern: usize,
    role: Option<&'t str>,
    name: Node<'t>,
    ty: Option<Node<'t>>,
    init: Option<Node<'t>>,
    iter: Option<Node<'t>>,
}

impl Scopes {
    /// Collect the bindings `query` (the language's `types`) captures.
    pub fn build(
        root: Node<'_>,
        bytes: &[u8],
        lang: Lang,
        query: Option<&Query>,
        resolver: &'static dyn LangResolver,
        injects: bool,
    ) -> Self {
        let mut out = Self {
            by_scope: HashMap::new(),
            scoped: !lang.scopes().is_empty(),
            implicit_this: resolver.implicit_this(),
            resolver,
            lang,
            root: root.id(),
        };
        let Some(query) = query else {
            return out;
        };
        let names = query.capture_names();
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(query, root, bytes);
        let mut captures: Vec<Capture<'_>> = Vec::new();
        while let Some(m) = matches.next() {
            let mut c = Capture {
                pattern: m.pattern_index,
                role: None,
                name: root,
                ty: None,
                init: None,
                iter: None,
            };
            let mut named = false;
            for cap in m.captures {
                match names[cap.index as usize] {
                    "name" => {
                        c.name = cap.node;
                        named = true;
                    }
                    "type" => c.ty = Some(cap.node),
                    "init" => c.init = Some(cap.node),
                    "iter" => c.iter = Some(cap.node),
                    r => c.role = r.strip_prefix("bind.").or(c.role),
                }
            }
            if named {
                captures.push(c);
            }
        }
        // One name node binds once per scope: of several patterns that
        // capture it (Python `x: Foo = make()` is typed and initialised; a
        // class attribute is also an assignment), the first in the query
        // wins.
        let scope_for = |c: &Capture<'_>, out: &Scopes| -> usize {
            match c.role {
                Some("func") => match c.name.parent() {
                    Some(def) => out.scope_of(def, lang),
                    None => out.scope_of(c.name, lang),
                },
                // A parameter property belongs to the class around its
                // constructor, not to the constructor; a Python attribute to
                // the class around its method.
                Some("inject") | Some("attr") => {
                    match enclosing_kind(c.name, lang.function_kinds()) {
                        Some(func) => out.scope_of(func, lang),
                        None => out.scope_of(c.name, lang),
                    }
                }
                _ => out.scope_of(c.name, lang),
            }
        };
        let mut first: HashMap<(usize, usize), usize> = HashMap::new();
        for c in &captures {
            let key = (c.name.id(), scope_for(c, &out));
            first
                .entry(key)
                .and_modify(|p| *p = (*p).min(c.pattern))
                .or_insert(c.pattern);
        }
        let mut pending: Vec<Pending<'_>> = Vec::new();
        for c in &captures {
            let scope = scope_for(c, &out);
            if first.get(&(c.name.id(), scope)) != Some(&c.pattern) {
                continue;
            }
            let (role, name_node, ty, init, iter) = (c.role, c.name, c.ty, c.init, c.iter);
            let Ok(text) = name_node.utf8_text(bytes) else {
                continue;
            };
            let via = match role {
                Some("field") | Some("attr") => Via::Field,
                Some("param") => Via::Param,
                // A constructor parameter property (`constructor(private x:
                // T)`): a field of the class, injected (rule 4).
                Some("inject") => Via::CtorInject,
                _ => Via::Local,
            };
            let member = matches!(role, Some("field") | Some("inject") | Some("attr"));
            // A function bound to the name: a `function f` (hoisted: in scope
            // from the start of the scope around its declaration) or a
            // `const f = () => …` (from its declaration; anywhere in the
            // module's own scope, which closures read later). Its type is
            // no type: the name is a symbol of its own.
            let func_value = init.is_some_and(|i| {
                matches!(
                    i.kind(),
                    "arrow_function" | "function_expression" | "generator_function" | "class"
                )
            });
            if role == Some("func") || func_value {
                let at = if role == Some("func") || scope == out.root {
                    0
                } else {
                    name_node
                        .parent()
                        .map_or(name_node.start_byte(), |p| p.start_byte())
                };
                // A function of a class body is its method (Python): never
                // a bare name of the methods.
                let in_class = out
                    .scope_node(name_node.parent().unwrap_or(name_node), lang)
                    .is_some_and(|n| lang.container_kinds().iter().any(|k| k == n.kind()));
                out.push(
                    scope,
                    Binding {
                        name: text.to_string(),
                        ty: None,
                        via,
                        at,
                        member: member || (role == Some("func") && in_class),
                        func: true,
                        this: false,
                        init_call: None,
                    },
                );
                continue;
            }
            let at = name_node
                .parent()
                .map_or(name_node.start_byte(), |p| p.start_byte());
            let written = ty
                .filter(|t| t.utf8_text(bytes).is_ok_and(|t| t.trim() != "var"))
                .and_then(|t| resolver.type_text(t, bytes));
            let assign = role == Some("assign");
            let attr = role == Some("attr");
            if written.is_some() && !assign {
                out.push(
                    scope,
                    Binding {
                        name: text.to_string(),
                        ty: written,
                        via,
                        at,
                        member,
                        func: false,
                        this: role == Some("receiver"),
                        init_call: None,
                    },
                );
                continue;
            }
            pending.push(Pending {
                name: text.to_string(),
                name_node,
                scope,
                via,
                at,
                init,
                iter,
                assign,
                attr,
                member,
            });
        }
        for p in pending {
            if p.assign {
                out.inject(&p, bytes, lang);
                continue;
            }
            // `self.x = x` with `x` a typed parameter of `__init__`: injected,
            // as Java's `this.x = x` (rule 4).
            if p.attr && out.inject(&p, bytes, lang) {
                continue;
            }
            let inferred = p
                .init
                .and_then(|i| match out.param_type(i, bytes) {
                    Some(t) if p.attr => Some((t, Via::Field)),
                    _ => resolver.init_type(i, bytes),
                })
                .or_else(|| {
                    let iterable = p.iter?;
                    let key = iterable.utf8_text(bytes).ok()?;
                    let elem = out.lookup(iterable, key)?.ty.as_ref()?.elem.clone()?;
                    Some((TypeText::parse(&elem)?, Via::Local))
                });
            // `x = f()` with `f` a local value (a closure, a parameter): what
            // it returns is no function of the file's (review).
            let inferred = inferred.filter(|(t, _)| {
                let Some(callee) = t
                    .base
                    .strip_suffix('?')
                    .unwrap_or(&t.base)
                    .strip_suffix("()")
                else {
                    return true;
                };
                let first = callee.split('.').next().unwrap_or(callee);
                let at = p.init.unwrap_or(p.name_node);
                !out.lookup(at, first).is_some_and(|b| !b.func)
            });
            let (ty, via) = match inferred {
                Some((t, Via::Static)) => (Some(t), Via::Static),
                // `inject(T)`: injected, a field or a local — only when the
                // file imports Angular's `inject` (a local helper of that
                // name injects nothing).
                Some((t, Via::CtorInject)) if injects => (Some(t), Via::CtorInject),
                Some((_, Via::CtorInject)) => (None, p.via),
                Some((t, _)) => (Some(t), p.via),
                None => (None, p.via),
            };
            // `let x = a.f()`: a call on `x` is a chain after `a.f()`.
            let init_call = match (&ty, p.init) {
                (None, Some(i)) if resolver.fields_by_link() && !p.member => {
                    let call = if i.kind() == "try_expression" || i.kind() == "await_expression" {
                        i.named_child(0).unwrap_or(i)
                    } else {
                        i
                    };
                    matches!(call.kind(), "call_expression" | "call")
                        .then(|| (i.start_byte(), i.end_byte()))
                }
                _ => None,
            };
            out.push(
                p.scope,
                Binding {
                    name: p.name,
                    ty,
                    via,
                    at: p.at,
                    member: p.member,
                    func: false,
                    this: false,
                    init_call,
                },
            );
        }
        out.settle_members();
        out
    }

    /// Several typed assignments of one attribute that disagree (Python
    /// `self.x = A()` here, `self.x = B()` there): none types it.
    fn settle_members(&mut self) {
        for bs in self.by_scope.values_mut() {
            let mut types: HashMap<String, Option<TypeText>> = HashMap::new();
            let mut clash: Vec<String> = Vec::new();
            for b in bs.iter().filter(|b| b.member && b.via == Via::Field) {
                let Some(t) = &b.ty else { continue };
                match types.get(&b.name) {
                    Some(Some(prev)) if prev != t => clash.push(b.name.clone()),
                    Some(_) => {}
                    None => {
                        types.insert(b.name.clone(), Some(t.clone()));
                    }
                }
            }
            for b in bs
                .iter_mut()
                .filter(|b| b.member && b.via == Via::Field && clash.contains(&b.name))
            {
                b.ty = None;
            }
        }
    }

    /// The type of a typed parameter an initializer names (`self.repo =
    /// repo` with `repo: Repo`).
    fn param_type(&self, init: Node<'_>, bytes: &[u8]) -> Option<TypeText> {
        if init.kind() != "identifier" {
            return None;
        }
        let b = self.lookup(init, init.utf8_text(bytes).ok()?)?;
        (b.via == Via::Param && !b.member)
            .then(|| b.ty.clone())
            .flatten()
    }

    /// `this.x = x` inside a constructor, where `x` is a typed parameter:
    /// the field `x` of the class is typed by it (rule 4).
    fn inject(&mut self, p: &Pending<'_>, bytes: &[u8], lang: Lang) -> bool {
        let Some(func) = enclosing_kind(p.name_node, lang.function_kinds()) else {
            return false;
        };
        let is_ctor = func.kind().contains("constructor")
            || func
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(bytes).ok())
                .is_some_and(|n| n == "constructor" || n == "__init__");
        if !is_ctor {
            return false;
        }
        let Some(init) = p.init else { return false };
        let Ok(key) = init.utf8_text(bytes) else {
            return false;
        };
        let Some(param) = self.lookup(init, key) else {
            return false;
        };
        if param.via != Via::Param || param.ty.is_none() || param.member {
            return false;
        }
        let class_scope = self.scope_of(func, lang);
        // The field's declared type types it; the constructor only labels
        // the edge (`Foo(ServiceImpl s) { this.s = s; }` with `Service s`
        // stays `Service`, so its callers stay `Service.m`'s).
        let declared = self.by_scope.get(&class_scope).and_then(|bs| {
            bs.iter()
                .find(|b| b.name == p.name && b.member && b.via == Via::Field && b.ty.is_some())
                .and_then(|b| b.ty.clone())
        });
        let ty = declared.or_else(|| param.ty.clone());
        self.push(
            class_scope,
            Binding {
                name: p.name.clone(),
                ty,
                via: Via::CtorInject,
                at: p.at,
                member: true,
                func: false,
                this: false,
                init_call: None,
            },
        );
        true
    }

    fn push(&mut self, scope: usize, b: Binding) {
        self.by_scope.entry(scope).or_default().push(b);
    }

    /// The innermost scope around `node` (not `node` itself).
    fn scope_of(&self, node: Node<'_>, lang: Lang) -> usize {
        if !self.scoped {
            return self.root;
        }
        enclosing_kind(node, lang.scopes()).map_or(self.root, |n| n.id())
    }

    /// The innermost scope node around `node` (not `node` itself); `None`
    /// at the file's own scope.
    fn scope_node<'t>(&self, node: Node<'t>, lang: Lang) -> Option<Node<'t>> {
        if !self.scoped {
            return None;
        }
        enclosing_kind(node, lang.scopes())
    }

    /// What `name` is bound to where `at` uses it: the innermost scope around
    /// `at` that binds it; in a scope, a field assigned from a constructor
    /// parameter over its declaration, and the last local declared before
    /// `at`.
    pub fn lookup(&self, at: Node<'_>, name: &str) -> Option<&Binding> {
        let visible = |b: &&Binding| self.implicit_this || !b.member;
        if !self.scoped {
            return self
                .by_scope
                .get(&self.root)?
                .iter()
                .filter(visible)
                .find(|b| b.name == name);
        }
        let use_at = at.start_byte();
        let mut cur = at.parent();
        while let Some(n) = cur {
            if let Some(found) = self
                .by_scope
                .get(&n.id())
                .and_then(|bs| pick(bs, name, use_at, self.implicit_this))
            {
                return Some(found);
            }
            cur = n.parent();
        }
        // A module binding seen from inside a function (Python): the body
        // runs after the module's assignments, whatever their position.
        let use_at = if self.resolver.late_module_bindings()
            && enclosing_kind(at, self.lang.scopes()).is_some()
        {
            usize::MAX
        } else {
            use_at
        };
        self.by_scope
            .get(&self.root)
            .and_then(|bs| pick(bs, name, use_at, self.implicit_this))
    }

    /// Whether `b` is bound in the file's own scope (the module).
    pub fn at_top_level(&self, b: &Binding) -> bool {
        self.by_scope
            .get(&self.root)
            .is_some_and(|v| v.iter().any(|x| std::ptr::eq(x, b)))
    }

    /// Whether a bare name can be a member of the enclosing class (Java).
    pub fn implicit_this(&self) -> bool {
        self.implicit_this
    }

    /// The language's resolver.
    pub fn resolver(&self) -> &'static dyn LangResolver {
        self.resolver
    }

    /// A field of the class around `at` (`this.x`): the innermost class-level
    /// binding, never a local or a parameter.
    pub fn field(&self, at: Node<'_>, name: &str) -> Option<&Binding> {
        if !self.scoped {
            // One flat scope: a `this.x` is whatever `x` is bound to.
            return self.lookup(at, name);
        }
        let mut cur = at.parent();
        while let Some(n) = cur {
            if let Some(bs) = self.by_scope.get(&n.id()) {
                let mut found: Option<&Binding> = None;
                for b in bs.iter().filter(|b| b.name == name && b.member) {
                    match b.via {
                        Via::CtorInject => return Some(b),
                        // A typed assignment over an untyped one (Python
                        // `self.x = None` then `self.x = Foo()`).
                        Via::Field
                            if found.is_none()
                                || (found.is_some_and(|f| f.ty.is_none()) && b.ty.is_some()) =>
                        {
                            found = Some(b)
                        }
                        _ => {}
                    }
                }
                if found.is_some() {
                    return found;
                }
            }
            cur = n.parent();
        }
        None
    }
}

/// The binding of `name` in one scope's list, as seen from `use_at`; a
/// member only where a bare name can be one (`implicit_this`).
fn pick<'b>(
    bs: &'b [Binding],
    name: &str,
    use_at: usize,
    implicit_this: bool,
) -> Option<&'b Binding> {
    let mut best: Option<&Binding> = None;
    for b in bs
        .iter()
        .filter(|b| b.name == name && (implicit_this || !b.member))
    {
        match b.via {
            Via::CtorInject if b.member => return Some(b),
            Via::Local | Via::Static | Via::CtorInject if b.at > use_at => {}
            _ => best = Some(b),
        }
    }
    best
}

/// The nearest ancestor of `node` (not itself) whose kind is in `kinds`.
fn enclosing_kind<'t>(node: Node<'t>, kinds: &[String]) -> Option<Node<'t>> {
    let mut cur = node.parent();
    while let Some(n) = cur {
        if kinds.iter().any(|k| k == n.kind()) {
            return Some(n);
        }
        cur = n.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_type_keeps_its_element() {
        let t = TypeText::parse("java.util.List<Foo>").unwrap();
        assert_eq!(
            (t.base.as_str(), t.elem.as_deref()),
            ("java.util.List", Some("Foo"))
        );
        let t = TypeText::parse("Map<String, List<Long>>").unwrap();
        assert_eq!(
            (t.base.as_str(), t.elem.as_deref()),
            ("Map", Some("String"))
        );
        let t = TypeText::parse("Foo[]").unwrap();
        assert_eq!((t.base.as_str(), t.elem.as_deref()), ("Foo[]", Some("Foo")));
        let t = TypeText::parse("List<? extends Bar>").unwrap();
        assert_eq!(t.elem.as_deref(), Some("Bar"));
        assert_eq!(TypeText::parse("Foo").unwrap().elem, None);
        assert_eq!(TypeText::parse("Foo | null").unwrap().base, "Foo");
        assert_eq!(TypeText::parse("A | B").unwrap().base, "A|B");
    }
}
