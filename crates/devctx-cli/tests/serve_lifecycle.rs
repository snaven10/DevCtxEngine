//! How `devctx serve` ends: on SIGTERM, on its idle timer, and while a blocking
//! task is stuck (PLAN-008 B10, B11).
//!
//! The stuck task is a model download. `HF_ENDPOINT` points the loader at a
//! listener that accepts connections and never answers, which is exactly the
//! frozen socket seen in the field: ESTABLISHED, no bytes, no timeout. The
//! model cache is a private empty directory so the loader really has to fetch.
//!
//! The indexing tests (PLAN-008 TASK-009 fixup D1) need an index that really
//! advances, slowly and deterministically, without a model: the project is
//! switched to the `custom` embedding provider, served by [`FakeEmbedder`] — a
//! local HTTP endpoint that answers each batch after a fixed delay, or stops
//! answering after a given number of batches.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

mod common;

struct Tmp(PathBuf);

impl Tmp {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("devctx_life_it_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn home(&self) -> PathBuf {
        self.0.join("central")
    }
    fn cache(&self) -> PathBuf {
        self.0.join("empty-model-cache")
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        common::reap_servers_under(&self.0);
        let _ = Command::new(env!("CARGO_BIN_EXE_devctx"))
            .env("DEVCTX_HOME", self.home())
            .args(["serve", "--central", "--stop"])
            .output();
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A listener that accepts and never says a word. Counts what it accepted.
fn black_hole() -> (u16, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let accepted = Arc::new(AtomicUsize::new(0));
    let seen = accepted.clone();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming().flatten() {
            seen.fetch_add(1, Ordering::SeqCst);
            held.push(stream);
        }
    });
    (port, accepted)
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn project(tmp: &Tmp) -> PathBuf {
    let root = tmp.0.join("alpha");
    std::fs::create_dir_all(&root).unwrap();
    let git = Command::new("git")
        .args(["init", "-q"])
        .current_dir(&root)
        .status()
        .unwrap();
    assert!(git.success());
    std::fs::write(root.join("lib.rs"), "pub fn alpha() {}\n").unwrap();
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
    root
}

/// A `devctx serve` started by hand, killed on drop if the test failed first.
struct Serve {
    child: Child,
    port: u16,
    root: PathBuf,
    /// The server's stderr, kept so a failure can say what the server saw.
    log: PathBuf,
}

impl Serve {
    /// What the server wrote to stderr so far.
    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

impl Drop for Serve {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start_serve(tmp: &Tmp, root: &Path, idle: Option<u64>, hole: Option<u16>) -> Serve {
    start_serve_env(tmp, root, idle, hole, &[])
}

fn start_serve_env(
    tmp: &Tmp,
    root: &Path,
    idle: Option<u64>,
    hole: Option<u16>,
    envs: &[(&str, &str)],
) -> Serve {
    let port = free_port();
    let log = tmp.0.join(format!("serve-{port}.log"));
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_devctx"));
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.env("DEVCTX_HOME", tmp.home())
        .env("DEVCTX_MODEL_CACHE", tmp.cache())
        .env("DEVCTX_NO_AUTOSERVE", "1")
        .current_dir(root)
        .args(["serve", "--addr", &format!("127.0.0.1:{port}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(&log).expect("creating the serve log"));
    if let Some(secs) = idle {
        cmd.args(["--idle", &secs.to_string()]);
    }
    if let Some(h) = hole {
        cmd.env("HF_ENDPOINT", format!("http://127.0.0.1:{h}"));
    }
    let child = cmd.spawn().expect("spawning serve");
    let serve = Serve {
        child,
        port,
        root: root.to_path_buf(),
        log,
    };
    let t0 = Instant::now();
    while http(port, "GET", "/health", "", Duration::from_secs(2)).is_none() {
        assert!(
            t0.elapsed() < Duration::from_secs(60),
            "the server never answered /health"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    serve
}

/// One plain HTTP/1.0-style exchange; `None` if nothing answered in time.
fn http(port: u16, method: &str, path: &str, body: &str, read: Duration) -> Option<String> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).ok()?;
    s.set_read_timeout(Some(read)).ok()?;
    write!(
        s,
        "{method} {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .ok()?;
    let mut out = String::new();
    s.read_to_string(&mut out).ok()?;
    Some(out)
}

fn serve_json(root: &Path) -> PathBuf {
    root.join(".devctx/state/serve.json")
}

fn sigterm(child: &Child) {
    let ok = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(ok.success());
}

/// Wait for `child` to exit; its wall time, or `None` past `limit`.
fn exits_within(child: &mut Child, limit: Duration) -> Option<Duration> {
    let t0 = Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            return Some(t0.elapsed());
        }
        if t0.elapsed() >= limit {
            return None;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Start a stuck `/index`: the request is in flight and the loader is blocked
/// on the black hole. Returns once the loader has really connected to it.
fn stick_an_index(serve: &Serve, accepted: &AtomicUsize) {
    let port = serve.port;
    std::thread::spawn(move || {
        // Never answered; ends when the server goes away.
        let _ = http(port, "POST", "/index", "{}", Duration::from_secs(120));
    });
    let t0 = Instant::now();
    while accepted.load(Ordering::SeqCst) == 0 {
        assert!(
            t0.elapsed() < Duration::from_secs(30),
            "the index never reached the model download"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A plain stop is quick, checkpoints and withdraws its advertisement.
#[test]
fn sigterm_stops_an_idle_server_within_two_seconds() {
    let tmp = Tmp::new("term");
    let root = project(&tmp);
    let mut serve = start_serve(&tmp, &root, None, None);
    assert!(serve_json(&serve.root).exists(), "serve.json is advertised");

    sigterm(&serve.child);
    let took = exits_within(&mut serve.child, Duration::from_secs(10));
    eprintln!("B10 plain SIGTERM exit: {took:?}");
    let took = took.expect("serve ignored SIGTERM");
    assert!(took <= Duration::from_secs(2), "took {took:?}");
    assert!(!serve_json(&serve.root).exists(), "serve.json left behind");
}

/// D1b (TASK-017 item 9): `serve --stop` against a server whose final
/// checkpoint takes longer than the stop's fixed SIGTERM wait. The test seam
/// `DEVCTX_TEST_SLOW_CHECKPOINT_MS` stretches the checkpoint to 9 s; the server
/// holds its checkpoint marker meanwhile, so `--stop` keeps waiting instead of
/// sending SIGKILL, and the server leaves cleanly (exit 0, WAL folded).
#[test]
fn serve_stop_waits_for_a_final_checkpoint_longer_than_its_fixed_wait() {
    let tmp = Tmp::new("slowckpt");
    let root = project(&tmp);
    let mut serve = start_serve_env(
        &tmp,
        &root,
        None,
        None,
        &[("DEVCTX_TEST_SLOW_CHECKPOINT_MS", "9000")],
    );
    let t0 = Instant::now();
    let stop = Command::new(env!("CARGO_BIN_EXE_devctx"))
        .env("DEVCTX_HOME", tmp.home())
        .env("DEVCTX_NO_AUTOSERVE", "1")
        .current_dir(&root)
        .args(["serve", "--stop"])
        .output()
        .unwrap();
    let took = t0.elapsed();
    let said = String::from_utf8_lossy(&stop.stderr);
    assert!(stop.status.success(), "{said}");
    assert!(
        took > Duration::from_secs(8),
        "the stop returned before the checkpoint could have finished: {took:?}\n{said}"
    );
    let code = exits_within(&mut serve.child, Duration::from_secs(5))
        .and_then(|_| serve.child.try_wait().unwrap())
        .expect("serve --stop returned with the server alive");
    assert_eq!(
        code.code(),
        Some(0),
        "the server must finish its checkpoint and exit 0, not die by SIGKILL: {code:?}"
    );
    assert!(!serve_json(&serve.root).exists(), "serve.json left behind");
    assert!(wal_folded(&root), "the WAL outlived the server");
}

/// B10: SIGTERM while a blocking task is stuck in a download that never
/// answers. The orderly path cannot finish (the connection stays open, the
/// blocking pool never drains); the watchdog must.
#[test]
fn sigterm_stops_a_server_stuck_in_a_model_download() {
    let tmp = Tmp::new("stuck");
    let root = project(&tmp);
    let (hole, accepted) = black_hole();
    let mut serve = start_serve(&tmp, &root, None, Some(hole));
    stick_an_index(&serve, &accepted);

    sigterm(&serve.child);
    let took = exits_within(&mut serve.child, Duration::from_secs(20));
    eprintln!("B10 stuck-download SIGTERM exit: {took:?}");
    let took = took.expect("serve survived SIGTERM with a stuck download");
    assert!(took <= Duration::from_secs(10), "took {took:?}");
    assert!(!serve_json(&serve.root).exists(), "serve.json left behind");
}

/// B11: with nothing asking for anything, `--idle` ends the process.
#[test]
fn an_idle_server_exits_after_its_window() {
    let tmp = Tmp::new("idle");
    let root = project(&tmp);
    let mut serve = start_serve(&tmp, &root, Some(2), None);
    let took = exits_within(&mut serve.child, Duration::from_secs(15));
    eprintln!("B11 --idle 2 exit: {took:?}");
    let took = took.expect("--idle 2 was not honoured");
    assert!(took <= Duration::from_secs(8), "took {took:?}");
    assert!(!serve_json(&serve.root).exists(), "serve.json left behind");
}

/// B11: an `/index` stuck on a dead download must not make the server immortal.
///
/// Fixup D1 (item 7): the run is now visible from before its model loads, and
/// only a download that *receives data* counts as progress — so this really
/// exercises the stall branch of the exemption: `running` with an `advanced`
/// older than `DEVCTX_INDEX_STALL_SECS`.
#[test]
fn an_idle_server_exits_even_with_an_index_stuck() {
    let tmp = Tmp::new("idlestuck");
    let root = project(&tmp);
    let (hole, accepted) = black_hole();
    let mut serve = start_serve_env(
        &tmp,
        &root,
        Some(3),
        Some(hole),
        &[("DEVCTX_INDEX_STALL_SECS", "2")],
    );
    stick_an_index(&serve, &accepted);
    let progress = http(
        serve.port,
        "GET",
        "/index/progress",
        "",
        Duration::from_secs(2),
    )
    .expect("progress answers while the index is stuck");
    assert!(
        progress.contains("\"running\":true"),
        "the stuck run must be visible (and so subject to the stall rule): {progress}"
    );
    let took = exits_within(&mut serve.child, Duration::from_secs(20));
    eprintln!("B11 --idle 3 with a stuck index, exit: {took:?}");
    let took = took.expect("--idle 3 was not honoured with a stuck /index");
    assert!(took <= Duration::from_secs(12), "took {took:?}");
    assert!(!serve_json(&serve.root).exists(), "serve.json left behind");
}

/// B10c: a download that never gets a byte fails with a reason, instead of
/// hanging the request (and the embedder lock behind it) forever.
#[test]
fn a_stalled_model_download_fails_with_an_explicit_error() {
    let tmp = Tmp::new("stall");
    let root = project(&tmp);
    let (hole, _accepted) = black_hole();
    let port = free_port();
    let mut child = Command::new(env!("CARGO_BIN_EXE_devctx"))
        .env("DEVCTX_HOME", tmp.home())
        .env("DEVCTX_MODEL_CACHE", tmp.cache())
        .env("DEVCTX_MODEL_STALL_SECS", "2")
        .env("HF_ENDPOINT", format!("http://127.0.0.1:{hole}"))
        .current_dir(&root)
        .args(["serve", "--addr", &format!("127.0.0.1:{port}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let t0 = Instant::now();
    while http(port, "GET", "/health", "", Duration::from_secs(2)).is_none() {
        assert!(t0.elapsed() < Duration::from_secs(60), "no /health");
        std::thread::sleep(Duration::from_millis(100));
    }
    let started = Instant::now();
    let answer = http(port, "POST", "/index", "{}", Duration::from_secs(30))
        .expect("the stalled download never produced an answer");
    let took = started.elapsed();
    eprintln!("B10c stalled download answered in {took:?}");
    let _ = child.kill();
    let _ = child.wait();
    assert!(
        answer.contains("made no progress"),
        "expected the stall error, got: {answer}"
    );
    assert!(took <= Duration::from_secs(15), "took {took:?}");
}

/// B9: a server whose project was deleted from under it leaves on its own —
/// with no `--idle` at all (fixup D1: the check has its own watchdog), after
/// the required run of consecutive "not found" polls.
#[test]
fn a_server_whose_project_vanished_exits_by_itself() {
    let tmp = Tmp::new("vanish");
    let root = project(&tmp);
    let mut serve = start_serve_env(&tmp, &root, None, None, &[("DEVCTX_VANISH_POLL_MS", "500")]);
    std::fs::remove_dir_all(&root).unwrap();
    let took = exits_within(&mut serve.child, Duration::from_secs(30));
    eprintln!("B9 project deleted, exit: {took:?}");
    let took = took.expect("the server outlived its project");
    assert!(took <= Duration::from_secs(20), "took {took:?}");
}

// --- PLAN-008 TASK-009 fixup D1: indexing vs. stop and idle ---

const DIM: usize = 384;

/// A `custom`-provider embedding endpoint: answers `POST /embed` after
/// `delay`, and stops answering for good once `answer_first` batches (if set)
/// have been served.
struct FakeEmbedder {
    port: u16,
    served: Arc<AtomicUsize>,
}

impl FakeEmbedder {
    fn start(delay: Duration, answer_first: Option<usize>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let served = Arc::new(AtomicUsize::new(0));
        let count = served.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let count = count.clone();
                std::thread::spawn(move || {
                    let _ = embed_conn(stream, delay, answer_first, &count);
                });
            }
        });
        Self { port, served }
    }

    fn endpoint(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

/// Serve requests on one (keep-alive) connection.
fn embed_conn(
    stream: TcpStream,
    delay: Duration,
    answer_first: Option<usize>,
    served: &AtomicUsize,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut out = stream;
    loop {
        let mut len = 0usize;
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(());
        }
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
        let n = served.fetch_add(1, Ordering::SeqCst);
        if answer_first.is_some_and(|limit| n >= limit) {
            // Stop answering: hold the connection open, say nothing.
            loop {
                std::thread::sleep(Duration::from_secs(3600));
            }
        }
        std::thread::sleep(delay);
        let vectors: Vec<Vec<f32>> = (0..texts)
            .map(|i| {
                (0..DIM)
                    .map(|j| (((i + j + n) % 17) as f32 + 1.0) / 17.0)
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

/// A project of `files` source files indexed with the `custom` provider.
fn custom_project(tmp: &Tmp, files: usize) -> PathBuf {
    let root = project(tmp);
    for i in 0..files {
        std::fs::write(
            root.join(format!("m{i:03}.rs")),
            format!("pub fn f{i}() -> u32 {{\n    {i}\n}}\n"),
        )
        .unwrap();
    }
    let git = |args: &[&str]| {
        let ok = Command::new("git")
            .args(args)
            .current_dir(&root)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    };
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "files"]);
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

fn embed_env(fake: &FakeEmbedder) -> Vec<(&'static str, String)> {
    vec![
        ("DEVCTX_EMBED_ENDPOINT", fake.endpoint()),
        ("DEVCTX_EMBED_DIMENSION", DIM.to_string()),
    ]
}

fn start_indexing_serve(
    tmp: &Tmp,
    root: &Path,
    fake: &FakeEmbedder,
    idle: Option<u64>,
    extra: &[(&str, &str)],
) -> Serve {
    let env = embed_env(fake);
    let mut all: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    all.extend_from_slice(extra);
    start_serve_env(tmp, root, idle, None, &all)
}

/// Start `POST /index` on its own thread; its raw answer arrives on the
/// returned channel (or nothing, if the server dies without answering).
fn start_index(port: u16, body: &'static str) -> std::sync::mpsc::Receiver<Option<String>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(http(port, "POST", "/index", body, Duration::from_secs(300)));
    });
    rx
}

/// Wait until the server's index progress reports at least `done` files.
fn wait_for_files(port: u16, done: usize, limit: Duration) {
    let t0 = Instant::now();
    loop {
        if let Some(r) = http(port, "GET", "/index/progress", "", Duration::from_secs(2)) {
            let body = r.split("\r\n\r\n").nth(1).unwrap_or_default();
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
                if v["done"].as_u64().unwrap_or(0) as usize >= done {
                    return;
                }
            }
        }
        assert!(t0.elapsed() < limit, "the index never reached {done} files");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn db_path(root: &Path) -> PathBuf {
    root.join(".devctx/state/index.duckdb")
}

/// The WAL is folded: absent, or empty.
fn wal_folded(root: &Path) -> bool {
    std::fs::metadata(root.join(".devctx/state/index.duckdb.wal"))
        .map(|m| m.len() == 0)
        .unwrap_or(true)
}

/// Run `devctx <args>` in the project, directly against the store (no server).
fn devctx_direct(
    tmp: &Tmp,
    root: &Path,
    fake: &FakeEmbedder,
    args: &[&str],
) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_devctx"));
    for (k, v) in embed_env(fake) {
        cmd.env(k, v);
    }
    cmd.env("DEVCTX_HOME", tmp.home())
        .env("DEVCTX_MODEL_CACHE", tmp.cache())
        .env("DEVCTX_NO_AUTOSERVE", "1")
        .current_dir(root)
        .args(args)
        .output()
        .unwrap()
}

/// After the server is gone the database must open cleanly, and survive the
/// operation the broken-ART failure mode breaks: a full reindex, which begins
/// by deleting every row.
fn assert_database_sound(tmp: &Tmp, root: &Path, fake: &FakeEmbedder, files: usize) {
    assert!(db_path(root).exists(), "no database was written");
    let status = devctx_direct(tmp, root, fake, &["status"]);
    assert!(
        status.status.success(),
        "the database does not open cleanly: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    let full = devctx_direct(tmp, root, fake, &["index", "--full"]);
    let out = format!(
        "{}{}",
        String::from_utf8_lossy(&full.stdout),
        String::from_utf8_lossy(&full.stderr)
    );
    assert!(full.status.success(), "a full reindex failed: {out}");
    assert!(
        !out.contains("Failed to delete all rows"),
        "the ART indexes are broken: {out}"
    );
    // Every source file plus the project's own `lib.rs`.
    assert!(
        out.contains(&format!("full reindex ({} files", files + 1)),
        "the full reindex did not cover every file: {out}"
    );
}

/// Item 7: an index that keeps advancing holds the idle timer off for as long
/// as it runs — far past the idle window — and the server leaves once it ends.
#[test]
fn an_idle_server_waits_for_an_index_that_is_advancing() {
    let tmp = Tmp::new("idleadv");
    let root = custom_project(&tmp, 24);
    let fake = FakeEmbedder::start(Duration::from_millis(300), None);
    let mut serve = start_indexing_serve(
        &tmp,
        &root,
        &fake,
        Some(2),
        &[("DEVCTX_INDEX_STALL_SECS", "3")],
    );
    let answer = start_index(serve.port, "{}");
    // Well past the idle window, with the run still going.
    std::thread::sleep(Duration::from_secs(5));
    assert!(
        serve.child.try_wait().unwrap().is_none(),
        "the server quit under an index that was advancing"
    );
    let reply = answer
        .recv_timeout(Duration::from_secs(60))
        .expect("the index never answered")
        .expect("the server died before answering");
    assert!(
        reply.contains("\"files_indexed\""),
        "reply: {reply:?}\nserver log:\n{}",
        serve.log()
    );
    assert!(!reply.contains("\"cancelled\""), "{reply}");
    let took = exits_within(&mut serve.child, Duration::from_secs(15));
    eprintln!("D1 idle exit after an advancing index: {took:?}");
    took.expect("the server stayed after its index finished");
    assert!(wal_folded(&root), "the WAL outlived the server");
    assert_database_sound(&tmp, &root, &fake, 24);
}

/// Item 1: SIGTERM during a healthy index cancels it at the next file, which
/// commits and checkpoints; the server then exits promptly and the database is
/// sound.
#[test]
fn sigterm_during_an_index_cancels_it_and_leaves_a_sound_database() {
    let tmp = Tmp::new("termidx");
    let root = custom_project(&tmp, 40);
    let fake = FakeEmbedder::start(Duration::from_millis(300), None);
    let mut serve = start_indexing_serve(&tmp, &root, &fake, None, &[]);
    let answer = start_index(serve.port, "{}");
    wait_for_files(serve.port, 3, Duration::from_secs(60));

    sigterm(&serve.child);
    let took = exits_within(&mut serve.child, Duration::from_secs(30));
    eprintln!("D1 SIGTERM mid-index exit: {took:?}");
    let took = took.expect("serve survived SIGTERM during an index");
    assert!(took <= Duration::from_secs(5), "took {took:?}");
    let reply = answer.recv_timeout(Duration::from_secs(5)).ok().flatten();
    if let Some(r) = &reply {
        assert!(r.contains("\"cancelled\":true"), "{r}");
    }
    assert!(!serve_json(&serve.root).exists(), "serve.json left behind");
    assert!(wal_folded(&root), "the cancelled index left its WAL behind");
    assert!(
        fake.served.load(Ordering::SeqCst) < 40,
        "the run should have stopped early, not finished"
    );
    assert_database_sound(&tmp, &root, &fake, 40);
}

/// D1b item 3: `devctx index` routed to a server that cancels the run (it is
/// stopping) must not exit 0 as if the index were complete: it says the run
/// was cancelled and fails, so `devctx index && …` does not carry on.
#[test]
fn devctx_index_fails_when_the_server_cancels_its_run() {
    let tmp = Tmp::new("clicancel");
    let root = custom_project(&tmp, 40);
    let fake = FakeEmbedder::start(Duration::from_millis(300), None);
    let mut serve = start_indexing_serve(&tmp, &root, &fake, None, &[]);
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_devctx"));
    for (k, v) in embed_env(&fake) {
        cmd.env(k, v);
    }
    let client = cmd
        .env("DEVCTX_HOME", tmp.home())
        .env("DEVCTX_MODEL_CACHE", tmp.cache())
        .env("DEVCTX_NO_AUTOSERVE", "1")
        .current_dir(&root)
        .arg("index")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_for_files(serve.port, 3, Duration::from_secs(60));
    sigterm(&serve.child);
    // A CLI that hangs fails this test rather than the whole suite.
    let out = common::wait_with_timeout(client, Duration::from_secs(60))
        .unwrap_or_else(|e| panic!("`devctx index` never returned: {e}"));
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    exits_within(&mut serve.child, Duration::from_secs(30)).expect("serve survived SIGTERM");
    assert!(!out.status.success(), "a cancelled run exited 0:\n{said}");
    assert!(said.contains("cancelled"), "{said}");
}

/// Item 3: `devctx serve --stop` sees the index and waits for the cancellation
/// instead of escalating to SIGKILL in the middle of it.
#[test]
fn serve_stop_during_an_index_waits_for_the_cancellation() {
    let tmp = Tmp::new("stopidx");
    let root = custom_project(&tmp, 40);
    let fake = FakeEmbedder::start(Duration::from_millis(300), None);
    let mut serve = start_indexing_serve(&tmp, &root, &fake, None, &[]);
    let _answer = start_index(serve.port, "{}");
    wait_for_files(serve.port, 3, Duration::from_secs(60));

    let stop = Command::new(env!("CARGO_BIN_EXE_devctx"))
        .env("DEVCTX_HOME", tmp.home())
        .env("DEVCTX_NO_AUTOSERVE", "1")
        .current_dir(&root)
        .args(["serve", "--stop"])
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&stop.stderr);
    assert!(stop.status.success(), "{said}");
    assert!(said.contains("the server is indexing"), "{said}");
    let code = exits_within(&mut serve.child, Duration::from_secs(5))
        .and_then(|_| serve.child.try_wait().unwrap());
    let code = code.expect("serve --stop returned with the server alive");
    assert_eq!(
        code.code(),
        Some(0),
        "the server must exit on its own (0), not by SIGKILL: {code:?}"
    );
    assert!(wal_folded(&root), "the WAL outlived the server");
    assert_database_sound(&tmp, &root, &fake, 40);
}

/// Items 1–2, D1b items 2 and 4: an index stuck *inside a file's writes*
/// (the test seam `DEVCTX_TEST_STALL_IN_WRITE` holds it there, after the
/// file's old vectors were deleted) cannot reach a file boundary. The watchdog
/// waits out the quiet window and forces the exit — and the database must be
/// sound *without* a `--full` to paper over it: the WAL folded, and no file
/// whose recorded hash says "indexed, unchanged" while its vectors are gone.
///
/// The run is a `--full` over an existing index, the one case where such a
/// hole is permanent: the content did not change, so no incremental run would
/// ever look at the file again.
#[test]
fn a_forced_exit_mid_index_leaves_a_sound_database() {
    let tmp = Tmp::new("forceidx");
    let files = 30;
    let root = custom_project(&tmp, files);
    let fake = FakeEmbedder::start(Duration::from_millis(50), None);
    let first = devctx_direct(&tmp, &root, &fake, &["index"]);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let mut serve = start_indexing_serve(
        &tmp,
        &root,
        &fake,
        None,
        &[("DEVCTX_TEST_STALL_IN_WRITE", "5")],
    );
    let _answer = start_index(serve.port, r#"{"full":true}"#);
    let t0 = Instant::now();
    while !serve.log().contains("test seam: stalled inside the writes") {
        assert!(
            t0.elapsed() < Duration::from_secs(60),
            "never reached the stall:\n{}",
            serve.log()
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    sigterm(&serve.child);
    let took = exits_within(&mut serve.child, Duration::from_secs(40));
    eprintln!("D1 forced exit with an index stuck mid-write: {took:?}");
    let took = took.expect("serve survived SIGTERM with an index stuck mid-write");
    assert!(took <= Duration::from_secs(25), "took {took:?}");
    assert!(!serve_json(&serve.root).exists(), "serve.json left behind");
    assert!(wal_folded(&root), "the forced exit left the WAL behind");

    {
        let store = devctx_store::Store::open(&db_path(&root), DIM)
            .expect("the database does not open after the forced exit");
        let holes = store.files_missing_vectors().unwrap();
        assert!(
            holes.is_empty(),
            "files recorded as indexed with no vectors (a write cut half-way): {holes:?}\n{}",
            serve.log()
        );
        assert!(
            store.count(&Default::default()).unwrap() > 0,
            "the earlier index is still there"
        );
    }
    // The ART indexes are intact: an incremental run (which begins by deleting
    // the changed file's rows) works without a `--full`.
    std::fs::write(
        root.join("m000.rs"),
        "pub fn changed() -> u32 {\n    7\n}\n",
    )
    .unwrap();
    let inc = devctx_direct(&tmp, &root, &fake, &["index"]);
    let out = format!(
        "{}{}",
        String::from_utf8_lossy(&inc.stdout),
        String::from_utf8_lossy(&inc.stderr)
    );
    assert!(inc.status.success(), "an incremental run failed: {out}");
    assert!(!out.contains("Failed to delete all rows"), "{out}");
}
