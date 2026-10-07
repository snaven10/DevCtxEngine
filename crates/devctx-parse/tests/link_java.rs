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

/// (f) `Office.findByCodigo(c).flatMap(o -> o.persist())`: `findByCodigo`
/// resolved; `flatMap` is a method of its declared return type `Uni`
/// (external); `Office.listAll().map(…)` follows an external call, so it is
/// external too. `o.persist()` (an untyped lambda parameter) and
/// `q.setParameter` (an untyped `var`) call names the repository lacks:
/// dropped and counted.
#[test]
fn f_fluent_chains() {
    let l = linked();
    assert_eq!(
        l.show(&l.call("DraftResource.find", "Office.findByCodigo")),
        (q("Office.findByCodigo"), "high", "import", false)
    );
    assert_eq!(
        l.show(&l.call("DraftResource.find", "flatMap")),
        (None, "high", "return_type", true)
    );
    assert_eq!(l.call("DraftResource.find", "persist"), Outcome::Discard);
    assert_eq!(
        l.show(&l.call("DraftResource.listing", "map")),
        (None, "high", "return_type", true)
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
/// resolved from the parse, and `resolve` declines it.
#[test]
fn the_link_pass_declines_contains() {
    let l = linked();
    let any = l.edges[0].clone();
    let contains = LinkEdge {
        kind: "contains".into(),
        ..any
    };
    assert_eq!(l.index.resolve(&contains), None);
    assert!(l.edges.iter().all(|e| e.kind != "contains"));
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
