//! The compaction from the command line (PLAN-009 TASK-018, tests 25, 27,
//! 28 and 36): the automatic one after the `devctx index --full` that
//! migrates a pre-graph index, its refusals, and `devctx compact`.
//!
//! Indexing needs embeddings without a model: the project uses the `custom`
//! provider, served by a local endpoint that answers fixed vectors (the same
//! device as `serve_lifecycle.rs`).
#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

mod common;

const DIM: usize = 384;

struct Tmp(PathBuf);

impl Tmp {
    fn new(tag: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("devctx_compact_it_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn home(&self) -> PathBuf {
        self.0.join("central")
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        common::reap_servers_under(&self.0);
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `POST /embed` → fixed unit-ish vectors of width [`DIM`].
fn fake_embedder() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let _ = embed_conn(stream);
            });
        }
    });
    format!("http://127.0.0.1:{port}")
}

fn embed_conn(stream: TcpStream) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut out = stream;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let mut len = 0usize;
        loop {
            line.clear();
            reader.read_line(&mut line)?;
            let l = line.trim_end();
            if l.is_empty() {
                break;
            }
            if let Some((k, v)) = l.split_once(':') {
                if k.eq_ignore_ascii_case("content-length") {
                    len = v.trim().parse().unwrap_or(0);
                }
            }
        }
        let mut body = vec![0u8; len];
        reader.read_exact(&mut body)?;
        let texts = serde_json::from_slice::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v["texts"].as_array().map(|a| a.len()))
            .unwrap_or(0);
        let vectors: Vec<Vec<f32>> = (0..texts)
            .map(|i| {
                (0..DIM)
                    .map(|j| (((i + j) % 17) as f32 + 1.0) / 17.0)
                    .collect()
            })
            .collect();
        let resp = serde_json::json!({ "vectors": vectors, "dimension": DIM }).to_string();
        write!(
            out,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{resp}",
            resp.len()
        )?;
        out.flush()?;
    }
}

fn git(root: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?}");
}

/// A project of a few Rust files, indexed with the `custom` provider.
fn project(tmp: &Tmp) -> PathBuf {
    let root = tmp.0.join("alpha");
    std::fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    for i in 0..12 {
        std::fs::write(
            root.join(format!("m{i:02}.rs")),
            format!("pub fn f{i}() -> u32 {{\n    helper_{i}()\n}}\n"),
        )
        .unwrap();
    }
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "files"]);
    let out = Command::new(env!("CARGO_BIN_EXE_devctx"))
        .env("DEVCTX_HOME", tmp.home())
        .env("DEVCTX_NO_AUTOSERVE", "1")
        .args(["projects", "add", root.to_str().unwrap(), "--init"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let cfg = root.join(".devctx/config.yaml");
    let text = std::fs::read_to_string(&cfg).unwrap();
    let switched = text.replacen(
        "provider: local\n  model: minilm-l6",
        "provider: custom\n  model: fake-embed",
        1,
    );
    assert_ne!(
        text, switched,
        "the generated config changed shape:\n{text}"
    );
    std::fs::write(&cfg, switched).unwrap();
    root
}

fn db(root: &Path) -> PathBuf {
    root.join(".devctx/state/index.duckdb")
}

fn devctx(tmp: &Tmp, root: &Path, endpoint: &str, env: &[(&str, &str)], args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_devctx"));
    cmd.env_remove("DEVCTX_NO_AUTO_COMPACT")
        .env_remove("DEVCTX_TEST_ASSUME_TTY");
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.env("DEVCTX_HOME", tmp.home())
        .env("DEVCTX_MODEL_CACHE", tmp.0.join("models"))
        .env("DEVCTX_NO_AUTOSERVE", "1")
        .env("DEVCTX_EMBED_ENDPOINT", endpoint)
        .env("DEVCTX_EMBED_DIMENSION", DIM.to_string())
        .current_dir(root)
        .args(args)
        .output()
        .unwrap()
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn note(root: &Path) -> Option<String> {
    let s = devctx_store::Store::open(&db(root), DIM).unwrap();
    s.get_index_meta("", "", devctx_store::compact::COMPACTION_META_KEY)
        .unwrap()
}

/// Make the index look as 0.9.0 left it: an extractor stamp of before the
/// graph on its branch.
fn make_pre_graph(root: &Path) {
    let s = devctx_store::Store::open(&db(root), DIM).unwrap();
    let repo_path = std::fs::canonicalize(root)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert!(
        s.get_index_meta(&repo_path, "main", devctx_store::EXTRACTOR_META_KEY)
            .unwrap()
            .is_some(),
        "the index is not filed under {repo_path}"
    );
    s.set_index_meta(
        &repo_path,
        "main",
        devctx_store::EXTRACTOR_META_KEY,
        "v1-0123456789abcdef",
    )
    .unwrap();
}

/// 25, 27 and 36: after migrating a pre-graph index, `devctx index --full`
/// without a terminal (an agent) or with the switch leaves the compaction
/// owed and ends with the exact line `devctx compact`; with a terminal it
/// compacts, once — a second full run does not.
#[test]
fn the_upgrade_compacts_once_and_only_with_a_terminal() {
    let tmp = Tmp::new("auto");
    let root = project(&tmp);
    let ep = fake_embedder();
    let first = devctx(&tmp, &root, &ep, &[], &["index", "--full"]);
    assert!(first.status.success(), "{}", text(&first));
    assert_eq!(note(&root), None, "a fresh index owes nothing");

    // No terminal: owed, and the line to run.
    make_pre_graph(&root);
    let agent = devctx(&tmp, &root, &ep, &[], &["index", "--full"]);
    let said = text(&agent);
    assert!(agent.status.success(), "{:?} {said}", agent.status);
    assert!(said.lines().any(|l| l == "devctx compact"), "{said}");
    assert!(said.contains("no terminal"), "{said}");
    assert_eq!(note(&root).as_deref(), Some("owed:upgrade"));

    // The switch: owed.
    make_pre_graph(&root);
    let off = devctx(
        &tmp,
        &root,
        &ep,
        &[
            ("DEVCTX_NO_AUTO_COMPACT", "1"),
            ("DEVCTX_TEST_ASSUME_TTY", "1"),
        ],
        &["index", "--full"],
    );
    let said = text(&off);
    assert!(said.contains("DEVCTX_NO_AUTO_COMPACT"), "{said}");
    assert!(said.lines().any(|l| l == "devctx compact"), "{said}");
    assert_eq!(note(&root).as_deref(), Some("owed:upgrade"));

    // A terminal: compacted, once.
    make_pre_graph(&root);
    let tty = devctx(
        &tmp,
        &root,
        &ep,
        &[("DEVCTX_TEST_ASSUME_TTY", "1")],
        &["index", "--full"],
    );
    let said = text(&tty);
    assert!(tty.status.success(), "{said}");
    assert!(said.contains("compacted"), "{said}");
    let n = note(&root).unwrap();
    assert!(n.starts_with("done:") && n.ends_with(":upgrade"), "{n}");
    assert!(devctx_store::CompactPaths::for_db(&db(&root)).side.exists());

    let again = devctx(
        &tmp,
        &root,
        &ep,
        &[("DEVCTX_TEST_ASSUME_TTY", "1")],
        &["index", "--full"],
    );
    let said = text(&again);
    assert!(again.status.success(), "{said}");
    assert!(
        !said.contains("compacting"),
        "a second full run compacted: {said}"
    );
    assert_eq!(note(&root).as_deref(), Some(n.as_str()));
}

/// 26 (through the server): `devctx index --full` run against a server it
/// started stops that server with SIGTERM — it leaves cleanly — and
/// compacts; `status` through the next server reports the compaction done.
#[test]
fn through_the_server_the_upgrade_stops_it_cleanly_and_compacts() {
    let tmp = Tmp::new("served");
    let root = project(&tmp);
    let ep = fake_embedder();
    let first = devctx(&tmp, &root, &ep, &[], &["index", "--full"]);
    assert!(first.status.success(), "{}", text(&first));
    make_pre_graph(&root);
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_devctx"));
    let served = cmd
        .env_remove("DEVCTX_NO_AUTOSERVE")
        .env_remove("DEVCTX_NO_AUTO_COMPACT")
        .env("DEVCTX_TEST_ASSUME_TTY", "1")
        .env("DEVCTX_HOME", tmp.home())
        .env("DEVCTX_MODEL_CACHE", tmp.0.join("models"))
        .env("DEVCTX_EMBED_ENDPOINT", &ep)
        .env("DEVCTX_EMBED_DIMENSION", DIM.to_string())
        .current_dir(&root)
        .args(["index", "--full"])
        .output()
        .unwrap();
    let said = text(&served);
    assert!(served.status.success(), "{said}");
    assert!(said.contains("stopping the server"), "{said}");
    // Compacted at all means the server let the file go: with it open, the
    // compaction lock cannot be taken.
    assert!(said.contains("· compacted"), "{said}");
    let n = note(&root).unwrap();
    assert!(n.starts_with("done:"), "{n}");

    let status = Command::new(env!("CARGO_BIN_EXE_devctx"))
        .env_remove("DEVCTX_NO_AUTOSERVE")
        .env("DEVCTX_HOME", tmp.home())
        .env("DEVCTX_MODEL_CACHE", tmp.0.join("models"))
        .env("DEVCTX_EMBED_ENDPOINT", &ep)
        .env("DEVCTX_EMBED_DIMENSION", DIM.to_string())
        .current_dir(&root)
        .args(["status"])
        .output()
        .unwrap();
    let said = text(&status);
    assert!(said.contains(r#""compaction":{"#), "{said}");
    assert!(said.contains(r#""state":"done""#), "{said}");
}

fn sha(p: &Path) -> String {
    let out = Command::new("sha256sum").arg(p).output().unwrap();
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string()
}

/// Every file under `dir`, with its size and sha256.
fn listing(dir: &Path) -> Vec<(String, u64, String)> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_file() {
            out.push((
                e.file_name().to_string_lossy().into_owned(),
                std::fs::metadata(&p).unwrap().len(),
                sha(&p),
            ));
        }
    }
    out.sort();
    out
}

/// 28 and the Paso 4 command: `--dry-run` creates and changes nothing;
/// `devctx compact` compacts with or without a note, `--keep-original` keeps
/// a hard link of the original, and with nothing to give back it says so
/// unless `--force`.
#[test]
fn devctx_compact_dry_run_keep_original_and_force() {
    let tmp = Tmp::new("command");
    let root = project(&tmp);
    let ep = fake_embedder();
    let first = devctx(&tmp, &root, &ep, &[], &["index", "--full"]);
    assert!(first.status.success(), "{}", text(&first));
    // Free space to give back: a full run that rewrites everything.
    for i in 0..12 {
        std::fs::write(
            root.join(format!("m{i:02}.rs")),
            format!("pub fn g{i}() -> u32 {{\n    other_{i}()\n}}\n"),
        )
        .unwrap();
    }
    git(&root, &["commit", "-q", "-am", "rewrite"]);
    let second = devctx(&tmp, &root, &ep, &[], &["index", "--full"]);
    assert!(second.status.success(), "{}", text(&second));
    assert_eq!(note(&root), None);

    let state = root.join(".devctx/state");
    let before = listing(&state);
    let dry = devctx(&tmp, &root, &ep, &[], &["compact", "--dry-run"]);
    let said = text(&dry);
    assert!(dry.status.success(), "{said}");
    assert!(said.contains("blocks"), "{said}");
    assert!(said.contains("nothing was changed"), "{said}");
    assert_eq!(before, listing(&state), "--dry-run touched the state");

    let run = devctx(&tmp, &root, &ep, &[], &["compact", "--keep-original"]);
    let said = text(&run);
    assert!(run.status.success(), "{said}");
    assert!(said.contains("compacted"), "{said}");
    let original = state.join("index.duckdb.pre-compact");
    assert!(original.exists(), "{said}");
    let n = note(&root).unwrap();
    assert!(n.starts_with("done:") && n.ends_with(":manual"), "{n}");

    let nothing = devctx(&tmp, &root, &ep, &[], &["compact"]);
    let said = text(&nothing);
    assert!(nothing.status.success(), "{said}");
    assert!(said.contains("Nothing to give back"), "{said}");
    std::fs::remove_file(&original).unwrap();
    let forced = devctx(&tmp, &root, &ep, &[], &["compact", "--force"]);
    let said = text(&forced);
    assert!(forced.status.success(), "{said}");
    assert!(said.contains("compacted"), "{said}");
}

/// M-D restoration: a memory lost after a compaction comes back from the
/// sidecar with `devctx memories import`, embedded again.
#[test]
fn a_memory_is_restored_from_the_sidecar() {
    let tmp = Tmp::new("restore");
    let root = project(&tmp);
    let ep = fake_embedder();
    let first = devctx(&tmp, &root, &ep, &[], &["index", "--full"]);
    assert!(first.status.success(), "{}", text(&first));
    let rem = devctx(
        &tmp,
        &root,
        &ep,
        &[],
        &[
            "remember",
            "--title",
            "Interceptor retries",
            "--type",
            "decision",
            "--scope",
            "local",
            "the token interceptor retries once on 401",
        ],
    );
    assert!(rem.status.success(), "{}", text(&rem));
    let id = {
        let s = devctx_store::Store::open(&db(&root), DIM).unwrap();
        let all = s.all_memories("alpha").unwrap();
        assert_eq!(all.len(), 1, "{all:?}");
        all[0].id.clone()
    };
    let run = devctx(&tmp, &root, &ep, &[], &["compact", "--force"]);
    assert!(run.status.success(), "{}", text(&run));
    let side = devctx_store::CompactPaths::for_db(&db(&root)).side;
    assert!(side.exists());

    // Lost (here: forgotten for good) after the compaction.
    {
        let s = devctx_store::Store::open(&db(&root), DIM).unwrap();
        assert!(s.forget_memory(&id).unwrap());
        assert!(s.get_memory(&id).unwrap().is_none());
    }
    let imp = devctx(
        &tmp,
        &root,
        &ep,
        &[],
        &["memories", "import", side.to_str().unwrap()],
    );
    let said = text(&imp);
    assert!(imp.status.success(), "{said}");
    assert!(said.contains("restored 1"), "{said}");
    let s = devctx_store::Store::open(&db(&root), DIM).unwrap();
    let m = s.get_memory(&id).unwrap().expect("restored");
    assert_eq!(m.content, "the token interceptor retries once on 401");
    assert!(s.vector_by_id(&id).unwrap().is_some(), "embedded again");
}
