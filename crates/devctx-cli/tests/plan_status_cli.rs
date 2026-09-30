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

/// Table output for this repository's own `plans/` (5 real plans) fits in 600 bytes.
#[test]
fn table_output_on_the_real_repo_plans_fits_the_hook_budget() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let out = devctx(&repo_root, &["plan-status"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stdout.len() <= 600,
        "table output was {} bytes: {}",
        out.stdout.len(),
        String::from_utf8_lossy(&out.stdout)
    );
}
