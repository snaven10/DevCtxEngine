//! The plan parser against a frozen corpus (PLAN-007 TASK-006).
//!
//! `fixtures/plans/corpus/plans/` holds two plans written the way real, hand-maintained plans
//! are: `**Status:**` in bold, `Estado: x` without it, YAML front matter, a `## Estado` section,
//! several fields on one line separated by ` · `, `✅ **\`done\`** — ejecutada…`, `Depende de`
//! with prose around the ids, `TASK-001b`, `TASK-R01`, a `TASK-001-DESIGN.md` companion next to the
//! real `TASK-001-*.md`, a rollback table next to the status table. They are trimmed copies of
//! those shapes, committed here on purpose: this test must never read a checkout of another
//! repository, and a tolerance regression has to show up in `cargo test`, not on the dashboard.
//!
//! The assertions are exact. The warnings the corpus produces are the *legitimate* ones — a
//! status with no keyword, a `Depende de` that is only prose, a companion doc sharing an id, a
//! file in `tasks/` that is not a task, a table that disagrees with the task file — so a change
//! that adds or drops one is a change in behaviour, and this is where it gets noticed.

use std::collections::BTreeMap;
use std::path::PathBuf;

use devctx_core::plans::{analyze, load_plans, status_label, Plan};

fn corpus_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plans/corpus")
}

fn plan<'a>(plans: &'a [Plan], id: &str) -> &'a Plan {
    plans
        .iter()
        .find(|p| p.id == id)
        .unwrap_or_else(|| panic!("{id} missing from the corpus"))
}

/// `task id -> status label` of one plan.
fn statuses(p: &Plan) -> BTreeMap<String, String> {
    p.tasks
        .iter()
        .map(|t| (t.id.clone(), status_label(&t.status)))
        .collect()
}

fn expected(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

#[test]
fn the_corpus_loads_both_plans_with_their_titles() {
    let plans = load_plans(&corpus_root());
    let ids: Vec<&str> = plans.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(ids, ["PLAN-101", "PLAN-102"]);

    assert_eq!(
        plan(&plans, "PLAN-101").title,
        "Alfa: migración de catálogos"
    );
    // `# PLAN-1: Beta — reportes` inside `PLAN-102-beta/`: the prefix is stripped whatever its number.
    assert_eq!(plan(&plans, "PLAN-102").title, "Beta — reportes");

    // 7 tasks and 6 tasks: `TASK-001-DESIGN.md` is a companion, not an eighth.
    assert_eq!(plan(&plans, "PLAN-101").tasks.len(), 7);
    assert_eq!(plan(&plans, "PLAN-102").tasks.len(), 6);
}

#[test]
fn every_status_notation_in_the_corpus_is_read() {
    let plans = load_plans(&corpus_root());

    assert_eq!(
        statuses(plan(&plans, "PLAN-101")),
        expected(&[
            ("TASK-001", "done"),        // **Status:** done
            ("TASK-001b", "done"),       // front matter `status: completed`
            ("TASK-002", "in_progress"), // `## Estado` section: 🔄 **`in_progress`** — …
            ("TASK-003", "pending"),     // one line: **Plan:** … · **Estado:** PENDIENTE · …
            ("TASK-004", "skipped"),     // ⏭ descartada — reemplazada por TASK-003
            ("TASK-005", "unknown(revisar con QA)"),
            ("TASK-R01", "done"), // ✅ **`done`** — ejecutada el 2026-05-02
        ])
    );

    assert_eq!(
        statuses(plan(&plans, "PLAN-102")),
        expected(&[
            ("TASK-001", "done"),        // `Estado: Completed`, no bold
            ("TASK-002", "in_progress"), // **Estado**: sigue `in_progress`.
            ("TASK-003", "pending"),     // 🟢 `pending`
            ("TASK-004", "pending"),     // Pendiente
            ("TASK-005", "done"),        // ✅ implemented (PR #12)
            ("TASK-006", "pending"),     // todo
        ])
    );
}

#[test]
fn the_corpus_produces_exactly_the_legitimate_warnings() {
    let root = corpus_root();
    let plans = load_plans(&root);
    let alfa_tasks = root.join("plans/PLAN-101-alfa/tasks");
    let beta_tasks = root.join("plans/PLAN-102-beta/tasks");

    assert_eq!(
        sorted(plan(&plans, "PLAN-101").warnings.clone()),
        sorted(vec![
            // The companion shares the id of `TASK-001-ALCANCE.md`, which sorts first and is the task.
            format!(
                "id duplicado: {}",
                alfa_tasks.join("TASK-001-DESIGN.md").display()
            ),
            // A status with no recognizable keyword stays Unknown and says so.
            "TASK-005: Estado no reconocido 'revisar con QA'".to_string(),
            // The table says pending, the task file says in_progress: the file wins, the table warns.
            "TASK-002: tabla dice pending, archivo dice in_progress".to_string(),
        ]),
        "PLAN-101 warnings"
    );

    assert_eq!(
        sorted(plan(&plans, "PLAN-102").warnings.clone()),
        sorted(vec![
            format!(
                "nombre de archivo de task no reconocido: {}",
                beta_tasks.join("README.md").display()
            ),
            "TASK-006: Depende de sin TASK reconocible 'pendiente de decisión del cliente'"
                .to_string(),
        ]),
        "PLAN-102 warnings"
    );

    // What the old parser warned about and the tolerant one must not: no `Fase 0`, no invented ids,
    // no `TASK-001b` read as a self-cycle, no `BRIEF-…` picked as the plan document.
    for p in &plans {
        for w in &p.warnings {
            assert!(!w.contains("token no reconocido"), "{}: {w}", p.id);
            assert!(!w.contains("múltiples documentos"), "{}: {w}", p.id);
            assert!(!w.contains("ciclo"), "{}: {w}", p.id);
        }
    }
    assert_eq!(
        plan(&plans, "PLAN-102")
            .doc_path
            .as_deref()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str()),
        Some("PLAN-102-beta.md")
    );
}

#[test]
fn dependencies_are_extracted_from_prose_and_cross_plan_refs_are_only_reported() {
    let plans = load_plans(&corpus_root());
    let alfa = plan(&plans, "PLAN-101");
    let beta = plan(&plans, "PLAN-102");
    let task = |p: &Plan, id: &str| p.tasks.iter().find(|t| t.id == id).unwrap().clone();

    // Front-matter list.
    assert_eq!(task(alfa, "TASK-001b").depends_on, ["TASK-001"]);
    // Prose around the ids; `TASK-001b` stays distinct from `TASK-001`.
    assert_eq!(task(alfa, "TASK-002").depends_on, ["TASK-001", "TASK-001b"]);
    // Cross-plan reference: reported, not a dependency.
    let r01 = task(alfa, "TASK-R01");
    assert_eq!(r01.depends_on, ["TASK-001"]);
    assert_eq!(r01.external_deps, ["PLAN-102/TASK-002"]);

    // `Fase 0` invents nothing; the cross-plan reference is still external.
    let t3 = task(beta, "TASK-003");
    assert_eq!(t3.depends_on, ["TASK-001"]);
    assert_eq!(t3.external_deps, ["PLAN-101/TASK-002"]);
    // `TASK-001/002/003` expands to three.
    assert_eq!(
        task(beta, "TASK-004").depends_on,
        ["TASK-001", "TASK-002", "TASK-003"]
    );
    // Prose only: no dependency invented.
    assert!(task(beta, "TASK-006").depends_on.is_empty());
}

#[test]
fn the_status_table_ignores_the_rollback_table_and_strikes_become_skipped() {
    let plans = load_plans(&corpus_root());
    let alfa = plan(&plans, "PLAN-101");

    let table: BTreeMap<String, String> = alfa
        .table_status
        .iter()
        .map(|(k, v)| (k.clone(), status_label(v)))
        .collect();
    // TASK-002 reads `pending` from the Estado column. The rollback table below it lists
    // `TASK-002 | hecho el respaldo previo…`: read as a status table it would turn into `done`.
    assert_eq!(
        table,
        expected(&[
            ("TASK-001", "done"),
            ("TASK-001b", "done"),
            ("TASK-002", "pending"),
            ("TASK-003", "pending"),
            ("TASK-004", "skipped"), // ~~TASK-004~~ with an empty status cell
        ])
    );
}

#[test]
fn the_dependency_analysis_counts_skipped_as_resolved() {
    let plans = load_plans(&corpus_root());

    let a = analyze(plan(&plans, "PLAN-101"));
    // An Unknown status falls in with the pending ones: nothing blocks it, so it is ready.
    assert_eq!(a.ready, ["TASK-005"]);
    assert_eq!(a.in_progress, ["TASK-002"]);
    assert_eq!(
        a.blocked,
        [("TASK-003".to_string(), vec!["TASK-002".to_string()])]
    );
    assert!(a.missing.is_empty(), "{:?}", a.missing);
    assert!(a.cycles.is_empty(), "{:?}", a.cycles);

    let b = analyze(plan(&plans, "PLAN-102"));
    assert_eq!(b.ready, ["TASK-003", "TASK-006"]);
    assert_eq!(b.in_progress, ["TASK-002"]);
    assert_eq!(
        b.blocked,
        [(
            "TASK-004".to_string(),
            vec!["TASK-002".to_string(), "TASK-003".to_string()]
        )]
    );
    assert!(b.missing.is_empty(), "{:?}", b.missing);
    assert!(b.cycles.is_empty(), "{:?}", b.cycles);
}
