//! M1-1: `hello()` over the base's socket. A stand-in core listens on a Unix socket in a temporary
//! folder, speaks one JSON-RPC message per line, and counts the requests it receives.

use makiba::base::{BaseError, connect};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_millis(500);

/// What the stand-in core does with a request line.
#[derive(Clone, Copy)]
enum Mode {
    /// Answers `hello` with this version.
    Version(&'static str),
    /// Reads the request and says nothing.
    Silent,
}

struct Core {
    _dir: tempfile::TempDir,
    socket: PathBuf,
    requests: Arc<AtomicUsize>,
}

fn core(mode: Mode) -> Core {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("test.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let requests = Arc::new(AtomicUsize::new(0));
    let seen = requests.clone();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            let seen = seen.clone();
            thread::spawn(move || {
                let mut writer = stream.try_clone().unwrap();
                for line in BufReader::new(stream).lines() {
                    let Ok(line) = line else { return };
                    seen.fetch_add(1, Ordering::SeqCst);
                    let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                    assert_eq!(request["jsonrpc"], "2.0");
                    assert_eq!(request["method"], "hello");
                    if let Mode::Version(v) = mode {
                        let reply = serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": request["id"],
                            "result": {
                                "generation": 1,
                                "core": { "name": "herdr-core", "version": v },
                                "methods": [],
                                "features": [],
                            },
                        });
                        writeln!(writer, "{reply}").unwrap();
                    }
                }
            });
        }
    });
    Core {
        _dir: dir,
        socket,
        requests,
    }
}

#[test]
fn hello_returns_the_base_name_and_version() {
    let c = core(Mode::Version("0.1.21"));
    let info = connect(&c.socket, TIMEOUT).hello().expect("hello");
    assert_eq!(info.name, "herdr-core");
    assert_eq!(info.version, "0.1.21");
    assert_eq!(c.requests.load(Ordering::SeqCst), 1);
}

#[test]
fn a_version_outside_the_range_is_refused_and_nothing_else_is_sent() {
    let c = core(Mode::Version("0.2.0"));
    let base = connect(&c.socket, TIMEOUT);
    match base.hello() {
        Err(BaseError::UnsupportedVersion { found, .. }) => assert_eq!(found, "0.2.0"),
        other => panic!("expected UnsupportedVersion, got {other:?}"),
    }
    // Give a stray second request time to arrive, then count: only the hello.
    thread::sleep(Duration::from_millis(100));
    assert_eq!(c.requests.load(Ordering::SeqCst), 1);
}

#[test]
fn a_base_that_is_not_running_is_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    // No socket file at all.
    let missing = dir.path().join("none.sock");
    assert!(matches!(
        connect(&missing, TIMEOUT).hello(),
        Err(BaseError::Unavailable(_))
    ));
    // A stale socket file: bound once, nobody listening now.
    let stale = dir.path().join("stale.sock");
    drop(UnixListener::bind(&stale).unwrap());
    assert!(matches!(
        connect(&stale, TIMEOUT).hello(),
        Err(BaseError::Unavailable(_))
    ));
}

#[test]
fn a_base_that_does_not_answer_times_out_as_unavailable() {
    let c = core(Mode::Silent);
    let started = std::time::Instant::now();
    let result = connect(&c.socket, Duration::from_millis(200)).hello();
    assert!(
        matches!(result, Err(BaseError::Unavailable(_))),
        "{result:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(5));
    // No retry: the one request is all the core saw.
    assert_eq!(c.requests.load(Ordering::SeqCst), 1);
}
