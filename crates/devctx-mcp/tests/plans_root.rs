//! `do_plan_status` on a real `AppState` (PLAN-007 TASK-006).
//!
//! The resolver has unit tests and the binary has end-to-end ones; what neither reaches is the
//! seam between them: `AppState::build` has to feed the resolver the project's root, whether it
//! declares `group:`, and `$HOME`, and `do_plan_status` has to answer from the result. These build
//! the state the way the daemon does, from a `ProjectConfig`, and read the JSON it would serve.
//!
//! `AppState::build` reads `$HOME` from the environment, which is process-wide, so every test here
//! takes the same lock and restores it. This file is its own test binary: no other test shares the
//! process.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use devctx_core::config::{Project, ProjectConfig};
use devctx_mcp::state::{do_plan_status, AppState, PlanListOpts};

static ENV: Mutex<()> = Mutex::new(());

struct Tmp(PathBuf);

impl Tmp {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "devctx_mcp_plans_root_{tag}_{}",
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

/// `<root>/plans/PLAN-<n>-x/` with a doc and one pending task.
fn write_plan(root: &Path, n: u32) {
    let dir = root.join(format!("plans/PLAN-{n:03}-x"));
    std::fs::create_dir_all(dir.join("tasks")).unwrap();
    std::fs::write(
        dir.join(format!("PLAN-{n:03}-x.md")),
        format!("# PLAN-{n:03} — x\n"),
    )
    .unwrap();
    std::fs::write(
        dir.join("tasks/TASK-001-a.md"),
        "# TASK-001 — a\n\n- **Depende de:** —\n- **Estado:** `pending`\n",
    )
    .unwrap();
}

/// A config for a project at `member`, with its store kept outside the tree so the member
/// directory stays exactly what a real checkout looks like.
fn config(member: &Path, store: &Path, group: &str) -> ProjectConfig {
    std::fs::create_dir_all(store).unwrap();
    ProjectConfig {
        state_dir: store.to_string_lossy().into_owned(),
        project: Project {
            name: "api".into(),
            path: member.to_string_lossy().into_owned(),
            group: group.into(),
        },
        ..Default::default()
    }
}

/// Builds the state with `HOME` set to `home`, restores it, and returns what `do_plan_status`
/// answers (parsed).
fn status_with_home(cfg: ProjectConfig, home: &Path, plan: Option<&str>) -> serde_json::Value {
    let _guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
    // Opening the store must not try to reach a central daemon.
    std::env::set_var("DEVCTX_NO_AUTOSERVE", "1");
    let previous = std::env::var_os("HOME");
    std::env::set_var("HOME", home);
    let state = AppState::build(cfg);
    match previous {
        Some(h) => std::env::set_var("HOME", h),
        None => std::env::remove_var("HOME"),
    }
    let state = state.expect("building AppState");
    let raw = do_plan_status(&state, plan, PlanListOpts::default()).expect("do_plan_status");
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("not JSON ({e}): {raw}"))
}

fn plan_ids(value: &serde_json::Value) -> Vec<String> {
    value["plans"]
        .as_array()
        .unwrap_or_else(|| panic!("no `plans` array in: {value}"))
        .iter()
        .map(|p| p["id"].as_str().unwrap().to_string())
        .collect()
}

/// A member that declares `group:` and has no `plans/` answers with the workspace's, as
/// `ancestor`, for the list and for a single plan.
#[test]
fn a_group_member_answers_with_the_workspace_plans() {
    let tmp = Tmp::new("group");
    let ws = tmp.0.join("ws");
    let member = ws.join("api");
    std::fs::create_dir_all(&member).unwrap();
    write_plan(&ws, 1);
    write_plan(&ws, 2);

    let cfg = config(&member, &tmp.0.join("store"), "acme");
    let list = status_with_home(cfg.clone(), &tmp.0, None);
    assert_eq!(plan_ids(&list), ["PLAN-001", "PLAN-002"], "{list}");
    assert_eq!(
        list["plans_root"]["source"].as_str(),
        Some("ancestor"),
        "{list}"
    );
    assert_eq!(
        list["plans_root"]["path"].as_str(),
        Some(ws.to_string_lossy().as_ref()),
        "{list}"
    );

    let detail = status_with_home(cfg, &tmp.0, Some("PLAN-002"));
    assert_eq!(detail["plan"].as_str(), Some("PLAN-002"), "{detail}");
    assert_eq!(
        detail["plans_root"]["source"].as_str(),
        Some("ancestor"),
        "{detail}"
    );
}

/// The same member without `group:` stands alone: no adoption, an empty list that says why.
#[test]
fn a_member_without_a_group_reads_nothing_above_it() {
    let tmp = Tmp::new("nogroup");
    let ws = tmp.0.join("ws");
    let member = ws.join("api");
    std::fs::create_dir_all(&member).unwrap();
    write_plan(&ws, 1);

    let list = status_with_home(config(&member, &tmp.0.join("store"), ""), &tmp.0, None);
    assert!(plan_ids(&list).is_empty(), "{list}");
    assert_eq!(
        list["plans_root"]["source"].as_str(),
        Some("none"),
        "{list}"
    );
}

/// A member's own `plans/` wins over the workspace's.
#[test]
fn a_member_with_its_own_plans_keeps_them() {
    let tmp = Tmp::new("own");
    let ws = tmp.0.join("ws");
    let member = ws.join("api");
    std::fs::create_dir_all(&member).unwrap();
    write_plan(&ws, 9);
    write_plan(&member, 1);

    let list = status_with_home(config(&member, &tmp.0.join("store"), "acme"), &tmp.0, None);
    assert_eq!(plan_ids(&list), ["PLAN-001"], "{list}");
    assert_eq!(
        list["plans_root"]["source"].as_str(),
        Some("project"),
        "{list}"
    );
}

/// The walk upwards never reaches `$HOME` or beyond: with `HOME` at the workspace, the plans in it
/// are not adopted.
#[test]
fn the_walk_stops_before_home() {
    let tmp = Tmp::new("home");
    let ws = tmp.0.join("ws");
    let member = ws.join("api");
    std::fs::create_dir_all(&member).unwrap();
    write_plan(&ws, 1);

    let list = status_with_home(config(&member, &tmp.0.join("store"), "acme"), &ws, None);
    assert!(plan_ids(&list).is_empty(), "{list}");
    assert_eq!(
        list["plans_root"]["source"].as_str(),
        Some("none"),
        "{list}"
    );
}
