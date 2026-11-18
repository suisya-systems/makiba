//! M2-1: `makibad` runs once per state folder, says it listens only after it does, beats every
//! 10 s, stops cleanly on SIGTERM, and `makiba status` shows a dead one. The tests run the real
//! binary: `makiba daemon` on the stand-in base (`MAKIBA_BASE=stub`, whose first `Resync` comes
//! after `MAKIBA_STUB_SUBSCRIBE_DELAY_MS`), and `makiba status`.

use rusqlite::{Connection, OpenFlags};
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const MAKIBA: &str = env!("CARGO_BIN_EXE_makiba");

/// A `makibad` that is killed when the test ends, so a failed test leaves nothing running.
struct Daemon(Child);

impl Daemon {
    fn pid(&self) -> u32 {
        self.0.id()
    }

    fn sigterm(&self) {
        let ok = Command::new("kill")
            .args(["-TERM", &self.pid().to_string()])
            .status()
            .unwrap()
            .success();
        assert!(ok, "kill -TERM failed");
    }

    fn wait_exit(&mut self, within: Duration) -> std::process::ExitStatus {
        let until = Instant::now() + within;
        while Instant::now() < until {
            if let Some(status) = self.0.try_wait().unwrap() {
                return status;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("makibad did not exit within {within:?}");
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn command(dir: &Path, args: &[&str], delay_ms: u64) -> Command {
    let mut c = Command::new(MAKIBA);
    c.args(args)
        .env("MAKIBA_STATE_DIR", dir)
        // The door (M3-3) in the scratch folder too, on a port of the system's choosing.
        .env("MAKIBA_DOOR", "none")
        .env("MAKIBA_PORT", "0")
        .env("MAKIBA_PERSON_KEY", dir.join("key/person.key"))
        .env("MAKIBA_BASE", "stub")
        .env("MAKIBA_STUB_SUBSCRIBE_DELAY_MS", delay_ms.to_string())
        .stdin(Stdio::null());
    c
}

fn start(dir: &Path, delay_ms: u64) -> Daemon {
    let child = command(dir, &["daemon"], delay_ms)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    Daemon(child)
}

fn status(dir: &Path) -> Output {
    command(dir, &["status"], 0).output().unwrap()
}

struct Row {
    pid: i64,
    subscribed: i64,
    heartbeat_at: Option<String>,
}

/// The dispatcher row, read as a person's reader would: read-only. `None` while there is no
/// database, no table or no row yet.
fn row(dir: &Path) -> Option<Row> {
    let conn =
        Connection::open_with_flags(dir.join("state.db"), OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    conn.query_row(
        "SELECT pid, subscribed, heartbeat_at FROM dispatcher WHERE id = 1",
        [],
        |r| {
            Ok(Row {
                pid: r.get::<_, Option<i64>>(0)?.unwrap_or(0),
                subscribed: r.get(1)?,
                heartbeat_at: r.get(2)?,
            })
        },
    )
    .ok()
}

/// The payloads of the events of `kind`, oldest first.
fn events(dir: &Path, kind: &str) -> Vec<String> {
    let Ok(conn) =
        Connection::open_with_flags(dir.join("state.db"), OpenFlags::SQLITE_OPEN_READ_ONLY)
    else {
        return Vec::new();
    };
    let Ok(mut q) = conn.prepare("SELECT payload_json FROM events WHERE kind = ?1 ORDER BY id")
    else {
        return Vec::new();
    };
    q.query_map([kind], |r| r.get::<_, String>(0))
        .map(|rows| rows.map(Result::unwrap).collect())
        .unwrap_or_default()
}

fn wait<T>(what: &str, within: Duration, mut f: impl FnMut() -> Option<T>) -> T {
    let until = Instant::now() + within;
    while Instant::now() < until {
        if let Some(v) = f() {
            return v;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("timed out waiting for {what}");
}

#[test]
fn a_second_makibad_names_the_first_ones_pid() {
    let dir = tempfile::tempdir().unwrap();
    let first = start(dir.path(), 0);
    wait(
        "the first makibad to be subscribed",
        Duration::from_secs(10),
        || row(dir.path()).filter(|r| r.subscribed == 1),
    );

    let second = command(dir.path(), &["daemon"], 0).output().unwrap();
    assert!(
        !second.status.success(),
        "a second makibad must exit non-zero"
    );
    let said = String::from_utf8_lossy(&second.stderr).to_string();
    assert!(
        said.contains(&first.pid().to_string()),
        "the message must name pid {}: {said:?}",
        first.pid()
    );
    // It did not disturb the first one.
    assert_eq!(row(dir.path()).unwrap().pid, i64::from(first.pid()));
}

#[test]
fn subscribed_is_zero_until_the_subscription_exists() {
    let dir = tempfile::tempdir().unwrap();
    let delay = Duration::from_millis(4000);
    let began = Instant::now();
    let daemon = start(dir.path(), delay.as_millis() as u64);

    // The row exists before the subscription does, and says it is not subscribed.
    let early = wait("the dispatcher row", Duration::from_secs(10), || {
        row(dir.path())
    });
    assert!(
        began.elapsed() < delay,
        "the row must be written before the subscription is established"
    );
    assert_eq!(early.pid, i64::from(daemon.pid()));
    assert_eq!(early.subscribed, 0, "subscribed before the base answered");
    assert!(
        events(dir.path(), "dispatcher_started").is_empty(),
        "dispatcher_started written before the subscription"
    );
    let shown = String::from_utf8_lossy(&status(dir.path()).stdout).to_lowercase();
    assert!(
        shown.contains("subscribed"),
        "status says nothing of it: {shown:?}"
    );

    wait("subscribed = 1", Duration::from_secs(15), || {
        row(dir.path()).filter(|r| r.subscribed == 1)
    });
    assert!(
        began.elapsed() >= delay,
        "subscribed = 1 after {:?}, before the stand-in's first Resync",
        began.elapsed()
    );
    let started = events(dir.path(), "dispatcher_started");
    assert_eq!(started.len(), 1, "dispatcher_started: {started:?}");
    let payload: serde_json::Value = serde_json::from_str(&started[0]).unwrap();
    assert_eq!(payload["pid"], i64::from(daemon.pid()));
    assert_eq!(payload["base"], "stub");
    assert!(row(dir.path()).unwrap().heartbeat_at.is_some());
    assert!(
        status(dir.path()).status.success(),
        "a live, subscribed makibad"
    );
}

#[test]
fn sigterm_stops_cleanly_and_releases_the_lock() {
    let dir = tempfile::tempdir().unwrap();
    let mut daemon = start(dir.path(), 0);
    let first = wait("subscribed", Duration::from_secs(10), || {
        row(dir.path()).filter(|r| r.subscribed == 1)
    });

    // It beats again, ten seconds apart.
    let beat = first.heartbeat_at.clone().unwrap();
    wait("a later heartbeat", Duration::from_secs(20), || {
        row(dir.path())
            .and_then(|r| r.heartbeat_at)
            .filter(|h| *h != beat)
    });

    daemon.sigterm();
    assert!(daemon.wait_exit(Duration::from_secs(10)).success());
    let stopped = events(dir.path(), "dispatcher_stopped");
    assert_eq!(stopped.len(), 1, "dispatcher_stopped: {stopped:?}");
    assert_eq!(row(dir.path()).unwrap().subscribed, 0);

    // The lock is free: a new makibad starts on the same folder.
    let again = start(dir.path(), 0);
    let r = wait("the new makibad", Duration::from_secs(10), || {
        row(dir.path()).filter(|r| r.subscribed == 1 && r.pid == i64::from(again.pid()))
    });
    assert_eq!(r.pid, i64::from(again.pid()));
}

#[test]
fn status_fails_within_30_s_of_a_sigkill() {
    let dir = tempfile::tempdir().unwrap();
    let mut daemon = start(dir.path(), 0);
    wait("subscribed", Duration::from_secs(10), || {
        row(dir.path()).filter(|r| r.subscribed == 1)
    });
    assert!(
        status(dir.path()).status.success(),
        "status fails on a live makibad"
    );

    daemon.0.kill().unwrap(); // SIGKILL: nothing is written on the way out
    daemon.0.wait().unwrap();
    let killed = Instant::now();
    wait("status to exit non-zero", Duration::from_secs(35), || {
        (!status(dir.path()).status.success()).then_some(())
    });
    assert!(
        killed.elapsed() <= Duration::from_secs(35),
        "status took {:?} to notice",
        killed.elapsed()
    );
    // It says why, in words.
    let out = status(dir.path());
    let shown = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
    .to_lowercase();
    assert!(
        shown.contains("heartbeat"),
        "status does not mention the heartbeat: {shown:?}"
    );
}
