//! Compacting a project's database from the CLI (PLAN-009 TASK-018): once,
//! automatically, after the `devctx index --full` that migrates an index made
//! before the symbol graph; and on request, `devctx compact`.
//!
//! The compaction itself is `devctx_store::compact`. What lives here is the
//! part only a client can do: decide, announce, stop this project's server
//! (and refuse to go on unless it left cleanly), and report. A server never
//! compacts the file it serves.

use std::path::Path;

use devctx_core::config::ProjectConfig;
use devctx_store::compact::{
    self, CompactError, CompactOptions, CompactReport, COMPACTION_META_KEY,
    COMPACTION_OWED_FAILED_VERIFY, COMPACTION_OWED_UPGRADE,
};

use crate::remote::{self, Reclaim, Remote};

/// Set to anything to keep `devctx index --full` from compacting by itself.
pub const NO_AUTO_COMPACT_ENV: &str = "DEVCTX_NO_AUTO_COMPACT";

/// For the integration tests only: act as if a terminal were attached.
const ASSUME_TTY_ENV: &str = "DEVCTX_TEST_ASSUME_TTY";

/// What to do after a full run, from the note it left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AutoDecision {
    /// Nothing is owed.
    NotOwed,
    /// Owed, but not now; the reason, for the person reading.
    Skip(String),
    /// Owed, and this session may run it.
    Run,
}

/// What the session that ran the index says about itself.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Session {
    /// `--full` was asked for: a run the pipeline promoted to full (a
    /// rebased history, another model, a new branch) does not count (T1).
    pub requested_full: bool,
    /// `DEVCTX_NO_AUTO_COMPACT` is set.
    pub disabled: bool,
    /// A terminal is attached.
    pub tty: bool,
    /// This is Windows, where the replacement is not verified yet (T5).
    pub windows: bool,
}

impl Session {
    fn here(requested_full: bool) -> Self {
        Self {
            requested_full,
            disabled: std::env::var_os(NO_AUTO_COMPACT_ENV).is_some(),
            tty: is_tty(),
            windows: cfg!(windows),
        }
    }
}

/// The automatic compaction runs only for an upgrade's note, after a
/// `--full` that was asked for, never when it is switched off, never on
/// Windows until the cross-check, and only with a terminal to announce it on
/// — an agent or a script gets the line to run instead (D3).
pub(crate) fn auto_decision(note: Option<&str>, s: Session) -> AutoDecision {
    match note {
        Some(COMPACTION_OWED_UPGRADE) => {}
        Some(COMPACTION_OWED_FAILED_VERIFY) => {
            return AutoDecision::Skip(
                "the last compaction's copy failed verification; only `devctx compact` retries it"
                    .into(),
            )
        }
        _ => return AutoDecision::NotOwed,
    }
    if !s.requested_full {
        return AutoDecision::Skip(
            "only a `devctx index --full` that was asked for compacts by itself".into(),
        );
    }
    if s.disabled {
        return AutoDecision::Skip(format!("{NO_AUTO_COMPACT_ENV} is set"));
    }
    if s.windows {
        return AutoDecision::Skip(
            "the automatic compaction is not verified on Windows yet; stop the server and run \
             it yourself"
                .into(),
        );
    }
    if !s.tty {
        return AutoDecision::Skip(
            "no terminal to announce it on (run from an agent or a script)".into(),
        );
    }
    AutoDecision::Run
}

/// Whether the file may be replaced after stopping the server: only if it
/// left on SIGTERM (it folded its WAL) and no WAL is left (D3).
pub(crate) fn after_stop(reclaim: &Reclaim, wal_bytes: u64) -> Result<(), String> {
    match reclaim {
        Reclaim::NoServer | Reclaim::Stopped if wal_bytes == 0 => Ok(()),
        Reclaim::NoServer | Reclaim::Stopped => Err(format!(
            "the index has {wal_bytes} bytes of writes not folded into it"
        )),
        Reclaim::Killed => {
            Err("the server had to be killed (SIGKILL) and may have left writes unfolded".into())
        }
        Reclaim::NotStopped(why) => Err(format!("the server could not be stopped: {why}")),
    }
}

fn is_tty() -> bool {
    use std::io::IsTerminal;
    // The test override exists only in debug builds (T3).
    (cfg!(debug_assertions) && std::env::var_os(ASSUME_TTY_ENV).is_some())
        || (std::io::stdin().is_terminal() && std::io::stdout().is_terminal())
}

fn wal_bytes(db: &Path) -> u64 {
    std::fs::metadata(devctx_store::CompactPaths::for_db(db).wal)
        .map(|m| m.len())
        .unwrap_or(0)
}

fn mib(b: u64) -> u64 {
    b.div_ceil(1 << 20)
}

/// What a person waits: measured at ~55 MiB/s of file on a loaded machine
/// (5.4 s for 296 MiB), rounded up.
fn seconds_for(bytes: u64) -> u64 {
    (mib(bytes) / 50).max(1) + 1
}

/// The owed compaction, explained, with the exact command on a line of its
/// own.
fn print_owed(why: &str) {
    eprintln!(
        "· this index was upgraded from a version without the symbol graph, and the upgrade left \
         its file at about twice what it holds. It was not compacted now: {why}. To give the \
         space back (it stops this project's server for a few seconds), run:"
    );
    eprintln!("devctx compact");
}

/// The HNSW index's size and presence, as the server reports them.
pub(crate) fn hnsw_from_status(raw: &str) -> (Option<u64>, bool) {
    let v: serde_json::Value = serde_json::from_str(raw).unwrap_or_default();
    let h = &v["memory"]["hnsw"];
    (
        h["estimated_bytes"].as_u64(),
        h["present"].as_bool().unwrap_or(false),
    )
}

/// After a full run that did not get cancelled: compact if the run left the
/// upgrade's note and this session may. Never fails the run that called it.
pub fn after_full_index(
    cfg: &ProjectConfig,
    requested_full: bool,
    note: Option<&str>,
    server: Option<&Remote>,
    hnsw: (Option<u64>, bool),
) {
    let decision = auto_decision(note, Session::here(requested_full));
    match decision {
        AutoDecision::NotOwed => {}
        AutoDecision::Skip(why) => print_owed(&why),
        AutoDecision::Run => {
            if let Err(why) = auto_compact(cfg, server, hnsw) {
                print_owed(&why);
            }
        }
    }
}

fn auto_compact(
    cfg: &ProjectConfig,
    server: Option<&Remote>,
    (hnsw_bytes, hnsw_present): (Option<u64>, bool),
) -> Result<(), String> {
    let db = cfg.db_path();
    let pre = compact::preflight(&db, hnsw_bytes, &compact::SystemProbe::default())
        .map_err(|e| e.to_string())?;
    compact::check_extensions(hnsw_present, false).map_err(|e| e.to_string())?;
    if let Some(r) = server {
        if r.index_progress().is_ok_and(|p| p.running) {
            return Err("another indexing run is in flight".into());
        }
    }
    eprintln!(
        "· compacting the index once after the upgrade ({} MiB, about {} s): stopping the \
         server of {} meanwhile; anything that asks for it waits",
        mib(pre.db_bytes),
        seconds_for(pre.db_bytes),
        cfg.project.name
    );
    let stopped = remote::reclaim_db_report(cfg);
    after_stop(&stopped, wal_bytes(&db))?;
    let mut opts = CompactOptions::new("upgrade");
    opts.hnsw_bytes = hnsw_bytes;
    run(cfg, &db, opts).map(|_| ())
}

/// Run the compaction with Ctrl-C caught, print the outcome, and leave the
/// note a failure calls for.
pub fn run(
    cfg: &ProjectConfig,
    db: &Path,
    mut opts: CompactOptions<'_>,
) -> Result<CompactReport, String> {
    let interrupts = devctx_core::interrupt::Interrupts::catch();
    opts.cancel = Some(interrupts.flag());
    let out = compact::compact(db, &opts);
    drop(interrupts);
    match out {
        Ok(r) => {
            print_report(&r);
            Ok(r)
        }
        Err(e) => {
            record_failure(cfg, db, &e);
            Err(e.to_string())
        }
    }
}

fn record_failure(cfg: &ProjectConfig, db: &Path, e: &CompactError) {
    let Some(note) = e.note() else {
        return;
    };
    let dim = devctx_embed::dimension_for(&cfg.embeddings.provider, &cfg.embeddings.model);
    let written = devctx_store::Store::open(db, dim)
        .map_err(|e| e.to_string())
        .and_then(|s| {
            s.set_index_meta("", "", COMPACTION_META_KEY, note)
                .map_err(|e| e.to_string())
        });
    if let Err(e) = written {
        // The run left a marker next to the index; the next open writes the
        // note from it (T7).
        eprintln!(
            "· could not record `{note}` in the index now ({e}); the next open of the index \
             records it"
        );
    }
}

fn print_report(r: &CompactReport) {
    let secs: f64 = r.phases.iter().map(|(_, s)| s).sum();
    eprintln!(
        "· compacted {}: {} → {} MiB on disk, {} → {} blocks in use, in {secs:.1} s",
        r.db.display(),
        mib(r.bytes_before),
        mib(r.bytes_after),
        r.before.used,
        r.after.used
    );
    eprintln!(
        "  {}",
        r.phases
            .iter()
            .map(|(name, s)| format!("{name} {s:.1} s"))
            .collect::<Vec<_>>()
            .join(" · ")
    );
    if !r.null_filled.is_empty() {
        eprintln!(
            "  columns this version added, left empty: {}",
            r.null_filled.join(", ")
        );
    }
    if !r.ignored.is_empty() {
        eprintln!("  empty leftovers not copied: {}", r.ignored.join(", "));
    }
    eprintln!(
        "  memories saved to {} (kept 30 days; `devctx memories import` restores from it)",
        r.sidecar.display()
    );
    if let Some(k) = &r.kept_sidecar {
        eprintln!(
            "  an older sidecar held memories the index no longer has; kept as {}",
            k.display()
        );
    }
    if let Some(o) = &r.original {
        eprintln!("  the original is kept at {}", o.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// TASK-018 tests 27 (decision part): no terminal, or the switch, leave
    /// the compaction owed; a failed verification is never retried by itself.
    #[test]
    fn the_automatic_compaction_needs_the_note_a_terminal_and_no_switch() {
        let ok = Session {
            requested_full: true,
            disabled: false,
            tty: true,
            windows: false,
        };
        assert_eq!(auto_decision(None, ok), AutoDecision::NotOwed);
        assert_eq!(
            auto_decision(Some("done:0.10.0:1:upgrade"), ok),
            AutoDecision::NotOwed
        );
        assert_eq!(
            auto_decision(Some(COMPACTION_OWED_UPGRADE), ok),
            AutoDecision::Run
        );
        for (s, needle) in [
            (
                Session {
                    disabled: true,
                    ..ok
                },
                NO_AUTO_COMPACT_ENV,
            ),
            (Session { tty: false, ..ok }, "no terminal"),
            // T1: a run the pipeline promoted to full.
            (
                Session {
                    requested_full: false,
                    ..ok
                },
                "--full",
            ),
            // T5.
            (
                Session {
                    windows: true,
                    ..ok
                },
                "Windows",
            ),
        ] {
            match auto_decision(Some(COMPACTION_OWED_UPGRADE), s) {
                AutoDecision::Skip(why) => assert!(why.contains(needle), "{why}"),
                other => panic!("{other:?}"),
            }
        }
        assert!(matches!(
            auto_decision(Some(COMPACTION_OWED_FAILED_VERIFY), ok),
            AutoDecision::Skip(_)
        ));
    }

    /// TASK-018 test 26 (decision part): a server that had to be killed, or a
    /// WAL left behind, keeps the file from being replaced.
    #[test]
    fn only_a_clean_stop_lets_the_file_be_replaced() {
        assert!(after_stop(&Reclaim::Stopped, 0).is_ok());
        assert!(after_stop(&Reclaim::NoServer, 0).is_ok());
        assert!(after_stop(&Reclaim::Killed, 0)
            .unwrap_err()
            .contains("SIGKILL"));
        assert!(after_stop(&Reclaim::Stopped, 170)
            .unwrap_err()
            .contains("not folded"));
        assert!(after_stop(&Reclaim::NotStopped("no".into()), 0).is_err());
    }

    #[test]
    fn the_hnsw_size_is_read_off_the_status_block() {
        let raw = r#"{"memory":{"hnsw":{"present":true,"estimated_bytes":1234}}}"#;
        assert_eq!(hnsw_from_status(raw), (Some(1234), true));
        assert_eq!(hnsw_from_status("not json"), (None, false));
    }
}
