//! End-to-end tests for `devctx plan-status` (PLAN-005 TASK-004).
//!
//! No daemon, no store: the subcommand reads `plans/` straight off disk, so these tests run
//! against a plain temp directory — no `devctx init`, no central home required.

use std::path::PathBuf;
use std::process::{Command, Output};

struct Tmp(PathBuf);

impl Tmp {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "devctx_plan_status_cli_it_{tag}_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn devctx(cwd: &PathBuf, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_devctx"))
        .current_dir(cwd)
        .env("DEVCTX_HOME", cwd.join("unused-central-home"))
        .args(args)
        .output()
        .expect("running devctx")
}

/// Like [`devctx`], with `HOME` pinned: the plans-root walk from a group member upwards stops
/// at `$HOME` (PLAN-007 DD-2), so a test about it must say where that is.
fn devctx_with_home(cwd: &PathBuf, home: &std::path::Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_devctx"))
        .current_dir(cwd)
        .env("DEVCTX_HOME", cwd.join("unused-central-home"))
        .env("HOME", home)
        .args(args)
        .output()
        .expect("running devctx")
}

fn write_fixture(root: &std::path::Path) {
    let dir = root.join("plans/PLAN-002-con-pendientes/tasks");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        root.join("plans/PLAN-002-con-pendientes/PLAN-002-con-pendientes.md"),
        "# PLAN-002 — con pendientes\n\n| Task | Estado |\n|---|---|\n| TASK-001 | `pending` |\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("TASK-001-a.md"),
        "# TASK-001 — a\n\n- **Depende de:** —\n- **Estado:** `pending`\n\n## Objetivo\n",
    )
    .unwrap();
}

/// `--format json` on a temp repo with `plans/` parses and reports the active plan — no
/// `.devctx/` project required.
#[test]
fn json_format_parses_and_reports_active_without_a_devctx_project() {
    let tmp = Tmp::new("json");
    write_fixture(&tmp.0);

    let out = devctx(&tmp.0, &["plan-status", "--format", "json"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON");
    assert_eq!(value["active"].as_str(), Some("PLAN-002"));

    // No project bound, no store touched: no daemon should have been started.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("started background server"),
        "plan-status must not start the daemon: {stderr}"
    );
}

/// A single plan requested by short id (`PLAN-002` normalized from `2`) returns the same shape.
#[test]
fn detail_by_short_plan_id() {
    let tmp = Tmp::new("detail");
    write_fixture(&tmp.0);

    let out = devctx(&tmp.0, &["plan-status", "2", "--format", "json"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).unwrap();
    assert_eq!(value["plan"].as_str(), Some("PLAN-002"));
    assert_eq!(value["ready"].as_array().unwrap().len(), 1);
}

/// An unknown plan id exits non-zero and names the plans that do exist.
#[test]
fn unknown_plan_id_exits_nonzero_with_available_ids() {
    let tmp = Tmp::new("unknown");
    write_fixture(&tmp.0);

    let out = devctx(&tmp.0, &["plan-status", "PLAN-999"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("PLAN-002"), "{stderr}");
}

/// No `plans/` directory at all: exits 0 and says so, rather than erroring.
#[test]
fn no_plans_directory_exits_zero() {
    let tmp = Tmp::new("noplans");
    let out = devctx(&tmp.0, &["plan-status"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("sin planes"), "{stdout}");
}

// ---------------------------------------------------------------------------
// PLAN-007 TASK-006 — `plan-status` from inside a member of a workspace.
//
// Layout, with `HOME` pointing at the temp root so the walk upwards is bounded by it:
//   <tmp>/ws/plans/PLAN-002-con-pendientes/…     plans kept at the workspace root
//   <tmp>/ws/api/.devctx/config.yaml             a member; no `plans/` of its own
// No `devctx init`, no registry, no daemon: the config is a hand-written minimum.
// ---------------------------------------------------------------------------

/// Registers `ws/api` as a project (config only) and returns `(ws, api)`.
fn write_member(base: &std::path::Path, group: Option<&str>) -> (PathBuf, PathBuf) {
    let ws = base.join("ws");
    let api = ws.join("api");
    std::fs::create_dir_all(api.join(".devctx")).unwrap();
    let group_line = group.map(|g| format!("  group: {g}\n")).unwrap_or_default();
    std::fs::write(
        api.join(".devctx/config.yaml"),
        format!(
            "project:\n  name: api\n  path: {}\n{group_line}",
            api.display()
        ),
    )
    .unwrap();
    (ws, api)
}

fn json_of(out: &Output) -> serde_json::Value {
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).expect("valid JSON")
}

fn plan_ids(value: &serde_json::Value) -> Vec<String> {
    value["plans"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap().to_string())
        .collect()
}

/// A member that declares `group:` and has no `plans/` reads the workspace's, and says so.
#[test]
fn from_inside_a_group_member_the_workspace_plans_are_listed_as_ancestor() {
    let tmp = Tmp::new("member_group");
    let (ws, api) = write_member(&tmp.0, Some("acme"));
    write_fixture(&ws);

    let out = devctx_with_home(&api, &tmp.0, &["plan-status", "--format", "json"]);
    let value = json_of(&out);
    assert_eq!(plan_ids(&value), ["PLAN-002"]);
    assert_eq!(value["active"].as_str(), Some("PLAN-002"));
    assert_eq!(value["plans_root"]["source"].as_str(), Some("ancestor"));
    assert_eq!(
        value["plans_root"]["path"].as_str(),
        Some(ws.to_string_lossy().as_ref())
    );

    // A single plan from the member resolves against the same root.
    let out = devctx_with_home(&api, &tmp.0, &["plan-status", "2", "--format", "json"]);
    let detail = json_of(&out);
    assert_eq!(detail["plan"].as_str(), Some("PLAN-002"));
    assert_eq!(detail["plans_root"]["source"].as_str(), Some("ancestor"));
}

/// Without `group:` a member stands alone: it must not adopt somebody else's `plans/`.
#[test]
fn a_member_without_a_group_does_not_adopt_the_workspace_plans() {
    let tmp = Tmp::new("member_nogroup");
    let (ws, api) = write_member(&tmp.0, None);
    write_fixture(&ws);

    let out = devctx_with_home(&api, &tmp.0, &["plan-status", "--format", "json"]);
    let value = json_of(&out);
    assert!(plan_ids(&value).is_empty(), "{value}");
    assert_eq!(value["plans_root"]["source"].as_str(), Some("none"));
}

/// A member with its own `plans/` keeps them, even when the workspace above has plans too.
#[test]
fn a_member_with_its_own_plans_wins_over_the_workspace() {
    let tmp = Tmp::new("member_own");
    let (ws, api) = write_member(&tmp.0, Some("acme"));
    write_fixture(&ws);
    let own = api.join("plans/PLAN-003-propio");
    std::fs::create_dir_all(&own).unwrap();
    std::fs::write(own.join("PLAN-003-propio.md"), "# PLAN-003 — propio\n").unwrap();

    let out = devctx_with_home(&api, &tmp.0, &["plan-status", "--format", "json"]);
    let value = json_of(&out);
    assert_eq!(plan_ids(&value), ["PLAN-003"]);
    assert_eq!(value["plans_root"]["source"].as_str(), Some("project"));
}

/// The walk upwards stops before `$HOME`: with `HOME` set to the workspace itself, nothing above
/// the member is eligible and the answer is the empty list, not the workspace's plans.
#[test]
fn the_ancestor_walk_stops_before_home() {
    let tmp = Tmp::new("member_home");
    let (ws, api) = write_member(&tmp.0, Some("acme"));
    write_fixture(&ws);

    let out = devctx_with_home(&api, &ws, &["plan-status", "--format", "json"]);
    let value = json_of(&out);
    assert!(plan_ids(&value).is_empty(), "{value}");
    assert_eq!(value["plans_root"]["source"].as_str(), Some("none"));
}

// ---------------------------------------------------------------------------
// The SessionStart hook injects this output on every turn, so it has a size budget.
// ---------------------------------------------------------------------------

/// Writes one plan whose tasks are `(id, title, status, depends_on)`.
fn write_budget_plan(
    root: &std::path::Path,
    n: u32,
    title: &str,
    tasks: &[(u32, &str, &str, &str)],
) {
    let dir = root.join(format!("plans/PLAN-{n:03}-fixture"));
    std::fs::create_dir_all(dir.join("tasks")).unwrap();
    std::fs::write(
        dir.join(format!("PLAN-{n:03}-fixture.md")),
        format!("# PLAN-{n:03} — {title}\n"),
    )
    .unwrap();
    for (id, task_title, status, deps) in tasks {
        std::fs::write(
            dir.join(format!("tasks/TASK-{id:03}-x.md")),
            format!(
                "# TASK-{id:03} — {task_title}\n\n- **Depende de:** {deps}\n- **Estado:** `{status}`\n"
            ),
        )
        .unwrap();
    }
}

/// The plan-status table of a *fixed* fixture shaped like this repository's own `plans/` — four
/// finished plans and one active plan with a ready task, one in progress and two blocked — fits
/// the hook's 600-byte budget.
///
/// This test used to run against the repository's live `plans/`. That tied a size budget to data
/// that grows with every plan written (PLAN-007 alone took the real output from 485 to 723 bytes
/// the day it entered the repo), so it failed for reasons unrelated to the formatter. The budget
/// is about the formatter's compactness (titles truncated, one line per plan, blocked tasks listed
/// by id), and a fixture pins exactly that. Plans are never trimmed to satisfy it. The fixture
/// output is ~330 bytes, so the margin catches a formatter that starts to bloat, not a plan that
/// grows.
#[test]
fn table_output_on_a_fixed_five_plan_fixture_fits_the_hook_budget() {
    let tmp = Tmp::new("budget");
    let root = &tmp.0;
    write_budget_plan(
        root,
        1,
        "Base del indexador",
        &[(1, "a", "done", "—"), (2, "b", "done", "TASK-001")],
    );
    write_budget_plan(
        root,
        2,
        "Memoria por grupos",
        &[
            (1, "a", "done", "—"),
            (2, "b", "done", "—"),
            (3, "c", "done", "TASK-002"),
        ],
    );
    write_budget_plan(
        root,
        3,
        "Servidor MCP",
        &[
            (1, "a", "done", "—"),
            (2, "b", "done", "—"),
            (3, "c", "done", "—"),
            (4, "d", "done", "TASK-003"),
        ],
    );
    write_budget_plan(
        root,
        4,
        "Dashboard web",
        &[
            (1, "a", "done", "—"),
            (2, "b", "done", "—"),
            (3, "c", "done", "—"),
        ],
    );
    write_budget_plan(
        root,
        5,
        "Plan activo con pendientes",
        &[
            (1, "Base", "done", "—"),
            (2, "Trabajo en curso", "in_progress", "—"),
            (3, "Lista para empezar", "pending", "TASK-001"),
            (4, "Espera a la 002", "pending", "TASK-002"),
            (5, "Espera a 003 y 004", "pending", "TASK-003, TASK-004"),
        ],
    );

    let out = devctx(root, &["plan-status"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Guard the fixture itself: it must exercise the whole block, or the budget proves nothing.
    assert!(stdout.contains("PLAN-005  1/5"), "{stdout}");
    assert!(stdout.contains("listas:"), "{stdout}");
    assert!(stdout.contains("en curso:"), "{stdout}");
    assert!(stdout.contains("bloqueadas: 2 ("), "{stdout}");
    assert!(
        out.stdout.len() <= 600,
        "table output was {} bytes: {stdout}",
        out.stdout.len()
    );
}
