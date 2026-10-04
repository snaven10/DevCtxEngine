//! How `devctx serve` ends: on SIGTERM, on its idle timer, and while a blocking
//! task is stuck (PLAN-008 B10, B11).
//!
//! The stuck task is a model download. `HF_ENDPOINT` points the loader at a
//! listener that accepts connections and never answers, which is exactly the
//! frozen socket seen in the field: ESTABLISHED, no bytes, no timeout. The
//! model cache is a private empty directory so the loader really has to fetch.

use std::io::{Read, Write};
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
}

impl Drop for Serve {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start_serve(tmp: &Tmp, root: &Path, idle: Option<u64>, hole: Option<u16>) -> Serve {
    let port = free_port();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_devctx"));
    cmd.env("DEVCTX_HOME", tmp.home())
        .env("DEVCTX_MODEL_CACHE", tmp.cache())
        .env("DEVCTX_NO_AUTOSERVE", "1")
        .current_dir(root)
        .args(["serve", "--addr", &format!("127.0.0.1:{port}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
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
#[test]
fn an_idle_server_exits_even_with_an_index_stuck() {
    let tmp = Tmp::new("idlestuck");
    let root = project(&tmp);
    let (hole, accepted) = black_hole();
    let mut serve = start_serve(&tmp, &root, Some(3), Some(hole));
    stick_an_index(&serve, &accepted);
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

/// B9: a server whose project was deleted from under it leaves on its own, even
/// with a long idle window (the check rides the same watchdog, every
/// `idle / 4`).
#[test]
fn a_server_whose_project_vanished_exits_by_itself() {
    let tmp = Tmp::new("vanish");
    let root = project(&tmp);
    let mut serve = start_serve(&tmp, &root, Some(40), None);
    std::fs::remove_dir_all(&root).unwrap();
    let took = exits_within(&mut serve.child, Duration::from_secs(30));
    eprintln!("B9 project deleted, exit: {took:?}");
    let took = took.expect("the server outlived its project");
    assert!(took <= Duration::from_secs(20), "took {took:?}");
}
