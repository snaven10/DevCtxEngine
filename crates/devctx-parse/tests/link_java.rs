//! The Java resolver (PLAN-009 TASK-005, DD-7): a multi-file fixture with
//! packages, parsed and linked as the indexer does, without a store.
//! Cases (a)-(i) of the task, plus the exclusion of `contains` (m-b).

use std::collections::HashMap;
use std::path::Path;

use devctx_parse::resolve::link::{link_rows, LinkEdge, Outcome, RepoIndex, Resolved};
use devctx_parse::{parse, Lang};

const ROOT: &str = "tests/fixtures/java/link";

struct Linked {
    index: RepoIndex,
    edges: Vec<LinkEdge>,
    /// id → qualified name.
    names: HashMap<u64, String>,
}

fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().is_some_and(|x| x == "java") {
            out.push(p);
        }
    }
}

fn linked() -> Linked {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join(ROOT);
    let mut files = Vec::new();
    walk(&root, &mut files);
    files.sort();
    let (mut symbols, mut edges) = (Vec::new(), Vec::new());
    for f in files {
        let rel = f
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let mut pf = parse(Lang::java(), &std::fs::read_to_string(&f).unwrap()).unwrap();
        pf.assign_ids("repo", &rel);
        let (s, e) = link_rows(&rel, &pf);
        symbols.extend(s);
        edges.extend(e);
    }
    let names = symbols
        .iter()
        .map(|s| (s.id, s.qualified.clone()))
        .collect();
    Linked {
        index: RepoIndex::new(symbols, &edges),
        edges,
        names,
    }
}

impl Linked {
    /// The outcomes of the edges of `kind` from `src` (qualified) to
    /// `dst_name`, in line order.
    fn all(&self, kind: &str, src: &str, dst: &str) -> Vec<Outcome> {
        let found: Vec<Outcome> = self
            .edges
            .iter()
            .filter(|e| e.kind == kind && e.dst_name == dst)
            .filter(|e| self.names.get(&e.src_id).map(String::as_str) == Some(src))
            .map(|e| self.index.resolve(e).expect("resolved"))
            .collect();
        assert!(
            !found.is_empty(),
            "no {kind} {src} -> {dst} in {:#?}",
            self.edges
                .iter()
                .filter(|e| self.names.get(&e.src_id).map(String::as_str) == Some(src))
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

    fn one(&self, kind: &str, src: &str, dst: &str) -> Outcome {
        self.all(kind, src, dst).remove(0)
    }

    fn call(&self, src: &str, dst: &str) -> Outcome {
        self.one("calls", src, dst)
    }

    /// `(destination qualified, confidence, resolution, external)`.
    fn show(&self, o: &Outcome) -> (Option<String>, &'static str, &'static str, bool) {
        match o {
            Outcome::Resolved(Resolved {
                dst_id,
                confidence,
                resolution,
                external,
            }) => (
                dst_id.map(|d| self.names[&d].clone()),
                confidence,
                resolution,
                *external,
            ),
            Outcome::Discard => (None, "-", "discard", false),
        }
    }
}

fn q(s: &str) -> Option<String> {
    Some(s.to_string())
}

/// (a) Three inner classes, each with a `service` field of another type:
/// each call reaches its own service, by the field, `high`.
#[test]
fn a_each_inner_class_types_its_own_field() {
    let l = linked();
    for s in ["Alpha", "Beta", "Gamma"] {
        let o = l.call(
            &format!("DraftResource.{s}ServiceAdapter.page"),
            &format!("{s}Service.findPaginated"),
        );
        assert_eq!(
            l.show(&o),
            (
                q(&format!("{s}Service.findPaginated")),
                "high",
                "field",
                false
            )
        );
    }
}

/// (b) `var q = em.createQuery(…)`: `q` is no type (nothing `var.`), its
/// call to a name the repository lacks is dropped; `var x = new T()` is `T`.
#[test]
fn b_var_is_never_a_type() {
    let l = linked();
    assert!(l.edges.iter().all(|e| !e.dst_name.starts_with("var.")));
    assert_eq!(
        l.call("DraftResource.query", "setParameter"),
        Outcome::Discard
    );
    assert_eq!(
        l.show(&l.call("DraftResource.query", "EntityManager.createQuery")),
        (None, "high", "external_known", true)
    );
    assert_eq!(
        l.show(&l.call("DraftResource.query", "Office.rename")),
        (q("Office.rename"), "high", "local", false)
    );
}

/// (c) `LOG` is a constant field, typed `Logger` by its declaration: never a
/// type of its own; `Logger` comes from a package the repository lacks.
#[test]
fn c_a_constant_is_a_field_with_its_type() {
    let l = linked();
    assert!(l.edges.iter().all(|e| !e.dst_name.starts_with("LOG.")));
    assert_eq!(
        l.show(&l.call("DraftResource.log", "Logger.info")),
        (None, "high", "external_known", true)
    );
}

/// (d) `this.helper = helper` in the constructor: constructor injection.
#[test]
fn d_constructor_injection() {
    let l = linked();
    assert_eq!(
        l.show(&l.call("DraftResource.audit", "AuditHelper.record")),
        (q("AuditHelper.record"), "high", "ctor_inject", false)
    );
}

/// (e) A type of the same package and an imported one.
#[test]
fn e_same_package_and_import() {
    let l = linked();
    assert_eq!(
        l.show(&l.call("DraftResource.imported", "AuditUtil.toJson")),
        (q("AuditUtil.toJson"), "high", "same_package", false)
    );
    assert_eq!(
        l.show(&l.call("DraftResource.imported", "Office.normalize")),
        (q("Office.normalize"), "high", "import", false)
    );
}

/// (f) `Office.findByCode(c).flatMap(o -> o.persist())`: `findByCode`
/// resolved; `flatMap` is a method of its declared return type `Uni`
/// (external); `Office.listAll().map(…)` follows an external call: nothing
/// types it, and `map` is no name of the repository, so it is a weak
/// external (`chain_external`, `medium`). `o.persist()` (an untyped lambda parameter) and
/// `q.setParameter` (an untyped `var`) call names the repository lacks:
/// dropped and counted.
#[test]
fn f_fluent_chains() {
    let l = linked();
    assert_eq!(
        l.show(&l.call("DraftResource.find", "Office.findByCode")),
        (q("Office.findByCode"), "high", "import", false)
    );
    assert_eq!(
        l.show(&l.call("DraftResource.find", "flatMap")),
        (None, "high", "external_known", true)
    );
    assert_eq!(l.call("DraftResource.find", "persist"), Outcome::Discard);
    assert_eq!(
        l.show(&l.call("DraftResource.listing", "map")),
        (None, "medium", "chain_external", true)
    );
    let dropped = l
        .edges
        .iter()
        .filter(|e| l.index.resolve(e) == Some(Outcome::Discard))
        .count();
    assert_eq!(dropped, 2, "persist and setParameter");
}

/// (g) `new AlphaServiceAdapter()` instantiates the inner class.
#[test]
fn g_new_is_instantiates() {
    let l = linked();
    assert_eq!(
        l.show(&l.one(
            "instantiates",
            "DraftResource.adapter",
            "AlphaServiceAdapter"
        )),
        (
            q("DraftResource.AlphaServiceAdapter"),
            "high",
            "same_file",
            false
        )
    );
}

/// (h) A static Panache method the entity does not define: inherited from an
/// external supertype.
#[test]
fn h_an_undefined_panache_static_is_inherited_and_external() {
    let l = linked();
    assert_eq!(
        l.show(&l.call("DraftResource.byId", "Office.findById")),
        (None, "high", "inherited", true)
    );
    assert_eq!(
        l.show(&l.call("DraftResource.listing", "Office.listAll")),
        (None, "high", "inherited", true)
    );
}

/// (i) For-each variables: declared (`Office o`) and `var o` over a
/// `List<Office>`.
#[test]
fn i_for_each_variables() {
    let l = linked();
    let both = l.all("calls", "DraftResource.loop", "Office.rename");
    assert_eq!(both.len(), 2);
    for o in &both {
        assert_eq!(l.show(o), (q("Office.rename"), "high", "local", false));
    }
}

/// m-b: `contains` is never resolved by the link pass (DD-6): it comes
/// resolved from the parse, and `resolve` declines it. (That the pass never
/// loads nor rewrites those rows is `devctx-index`'s tests'.)
#[test]
fn the_link_pass_declines_contains() {
    let l = linked();
    let any = l.edges[0].clone();
    let contains = LinkEdge {
        kind: "contains".into(),
        ..any
    };
    assert_eq!(l.index.resolve(&contains), None);
}

/// Conventions with no definition behind them: a call made available by a
/// static wildcard import of an external class (`assertEquals`) is
/// external; `Object`'s `toString` on an untyped lambda parameter is external
/// but only `medium` (a repository type may override it); a literal is of
/// its type; a Lombok `@Data` getter reaches its field, `medium`.
#[test]
fn static_imports_object_methods_literals_and_lombok() {
    let l = linked();
    assert_eq!(
        l.show(&l.call("Reports.check", "assertEquals")),
        (None, "high", "external_known", true)
    );
    assert_eq!(
        l.show(&l.call("Reports.check", "toString")),
        (None, "medium", "inherited", true)
    );
    assert_eq!(
        l.show(&l.call("Reports.check", "String.equals")),
        (None, "high", "external_known", true)
    );
    for o in l.all("calls", "Reports.check", "Row.getLabel") {
        assert_eq!(
            l.show(&o),
            (q("Reports.Row.label"), "medium", "param", false)
        );
    }
}

/// MAJOR 1 (review): a dotted receiver whose first segment is a binding is
/// typed through its fields (`target.parent.rename()`, `this.cfg.server
/// .port()`), never taken for a package; a real package path stays external.
#[test]
fn a_dotted_receiver_is_typed_through_its_fields() {
    let l = linked();
    assert_eq!(
        l.show(&l.call("Cases.members", "rename")),
        (q("Office.rename"), "high", "field", false)
    );
    assert_eq!(
        l.show(&l.call("Cases.members", "port")),
        (q("Cases.Server.port"), "high", "field", false)
    );
    assert_eq!(
        l.show(&l.call("Cases.members", "of")),
        (None, "high", "external_known", true)
    );
}

/// MAJOR 2: after an external call nothing types the receiver: a name the
/// repository defines, or one shaped like an accessor, stays undecided and
/// not external; any other is external, but never `high`.
#[test]
fn a_call_after_an_external_one_is_not_external_by_default() {
    let l = linked();
    assert_eq!(
        l.show(&l.call("Cases.afterExternal", "getOffice")),
        (None, "low", "name_only", false)
    );
    assert_eq!(
        l.show(&l.call("Cases.afterExternal", "rename")),
        (None, "low", "name_only", false)
    );
    assert_eq!(
        l.show(&l.call("Cases.afterExternal", "orElseThrow")),
        (None, "medium", "chain_external", true)
    );
}

/// MAJOR 3: a lambda parameter shadows the field of its name, untyped; an
/// `instanceof` pattern variable is typed by its pattern.
#[test]
fn lambda_parameters_and_patterns_bind_their_names() {
    let l = linked();
    let outs = l.all("calls", "Cases.shadowed", "rename");
    assert_eq!(outs.len(), 1, "the pattern's call is typed: Office.rename");
    assert_eq!(l.show(&outs[0]), (None, "low", "name_only", false));
    assert_eq!(
        l.show(&l.call("Cases.shadowed", "Office.rename")),
        (q("Office.rename"), "high", "local", false)
    );
}

/// MINOR: a chain is no surer than its previous call (a Lombok accessor is
/// `medium`), and an overload no arity fits is not `high`.
#[test]
fn confidence_never_escalates() {
    let l = linked();
    assert_eq!(
        l.show(&l.call("Cases.escalation", "rename")),
        (q("Office.rename"), "medium", "return_type", false)
    );
    assert_eq!(
        l.show(&l.call("Cases.escalation", "Office.rename")),
        (q("Office.rename"), "medium", "param", false)
    );
}

/// Link a handful of sources given inline (path, language, text).
fn link_sources(files: &[(&str, &str)]) -> Linked {
    let (mut symbols, mut edges) = (Vec::new(), Vec::new());
    for (path, src) in files {
        let lang = devctx_parse::detect_lang(Path::new(path)).unwrap();
        let mut pf = parse(lang, src).unwrap();
        pf.assign_ids("repo", path);
        let (s, e) = link_rows(path, &pf);
        symbols.extend(s);
        edges.extend(e);
    }
    let names = symbols
        .iter()
        .map(|s| (s.id, s.qualified.clone()))
        .collect();
    Linked {
        index: RepoIndex::new(symbols, &edges),
        edges,
        names,
    }
}

/// MINOR: an anonymous class is a scope: a bare call in it is its
/// supertype's first. Its supertype is external here: a name the enclosing
/// class also defines (`cancel`) is undecided, not the enclosing method in
/// `high`; one it does not (`purge`) is the supertype's.
#[test]
fn an_anonymous_class_scopes_its_bare_calls() {
    let l = link_sources(&[(
        "p/Clock.java",
        "package p;\nimport java.util.TimerTask;\nclass Clock {\n    void cancel() {}\n    \
         void start() {\n        new TimerTask() { public void run() { cancel(); purge(); } };\n    }\n}\n",
    )]);
    assert_eq!(
        l.show(&l.call("Clock.start.run", "cancel")),
        (None, "low", "name_only", false)
    );
    assert_eq!(
        l.show(&l.call("Clock.start.run", "purge")),
        (None, "high", "inherited", true)
    );
}

/// Deviation 4: constructor injection labels the edge, the field's declared
/// type types it — `Service.m`, not the implementation the constructor takes.
#[test]
fn constructor_injection_keeps_the_declared_type() {
    let l = link_sources(&[
        (
            "p/Service.java",
            "package p;\npublic class Service { public void m() {} }\n",
        ),
        (
            "p/ServiceImpl.java",
            "package p;\npublic class ServiceImpl extends Service { public void m() {} }\n",
        ),
        (
            "p/Foo.java",
            "package p;\nclass Foo {\n    private final Service s;\n    \
             Foo(ServiceImpl s) { this.s = s; }\n    void go() { s.m(); }\n}\n",
        ),
    ]);
    assert_eq!(
        l.show(&l.call("Foo.go", "Service.m")),
        (q("Service.m"), "high", "ctor_inject", false)
    );
}

/// MINOR: two types with one fully qualified name (two modules) are no
/// `high` answer.
#[test]
fn a_duplicated_fully_qualified_type_is_not_high() {
    let l = link_sources(&[
        (
            "a/src/p/Dup.java",
            "package p;\npublic class Dup { public void m() {} }\n",
        ),
        (
            "b/src/p/Dup.java",
            "package p;\npublic class Dup { public void m() {} }\n",
        ),
        (
            "c/src/q/User.java",
            "package q;\nimport p.Dup;\nclass User { void go(Dup d) { d.m(); } }\n",
        ),
    ]);
    let (dst, conf, _, _) = l.show(&l.call("User.go", "Dup.m"));
    assert_eq!((dst.as_deref(), conf), (Some("Dup.m"), "medium"));
}

/// MINOR: outside Java a type found only by being the one of its name is
/// `medium`, and so is what is typed by it.
#[test]
fn a_unique_name_type_does_not_make_a_typed_call_high() {
    let l = link_sources(&[
        ("a.py", "def f(repo: Repo):\n    repo.load()\n"),
        ("b.py", "class Repo:\n    def load(self):\n        pass\n"),
    ]);
    let (dst, conf, _, _) = l.show(&l.call("f", "Repo.load"));
    assert_eq!((dst.as_deref(), conf), (Some("Repo.load"), "medium"));
}

/// Arity is a Java rule: a Rust method's `&self` is no argument, so
/// `self.m()` is not an overload "no arity fits".
#[test]
fn arity_is_read_from_java_signatures_only() {
    let l = link_sources(&[(
        "src/a.rs",
        "struct S;\nimpl S {\n    fn m(&self) {}\n    fn go(&self) { self.m(); }\n}\n",
    )]);
    let (dst, conf, res, _) = l.show(&l.call("S.go", "S.m"));
    assert_eq!((dst.as_deref(), conf, res), (Some("S.m"), "high", "self"));
}
