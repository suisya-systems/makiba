//! Issue 624: a slow tick of `makibad`'s loop says how long it took and which pass held it.
//!
//! The loop times every pass and the heartbeat. A tick of 5 s or longer writes one line to
//! `makibad.log`, `makibad: a tick took <s> s; slowest: <pass> <s> s`, and appends one
//! `tick_slow` event; a quicker tick leaves nothing.
//!
//! What is real: the binary as `makiba daemon` on the stand-in base (`MAKIBA_BASE=stub`), its
//! standard error in `<state>/makibad.log`. The slow tick is made by a debug build's hold point
//! (`pass:close` in `<state>/makibad.hold-at`), not by the runner's speed: the test releases it
//! only once both its own monotonic clock and the database's clock (the one that writes the
//! heartbeat) have passed the threshold since the hold began. Durations are asserted only as at
//! least the threshold.

mod common;

use common::makibad;
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

/// makibad's threshold (`SLOW_TICK`), in seconds.
const SLOW_TICK_S: f64 = 5.0;
/// How long past the threshold the hold is kept.
const MARGIN_S: f64 = 0.5;

/// A state folder in a temporary root, and the `makibad` started on it.
struct Org {
    root: tempfile::TempDir,
    child: Option<Child>,
}

impl Org {
    fn new() -> Org {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("state/makiba")).unwrap();
        Org { root, child: None }
    }

    fn path(&self, p: &str) -> PathBuf {
        self.root.path().join(p)
    }

    fn state(&self) -> PathBuf {
        self.path("state/makiba")
    }

    fn log(&self) -> String {
        fs::read_to_string(self.state().join("makibad.log")).unwrap_or_default()
    }

    /// `makiba daemon` on this state folder, no door, its standard error in `makibad.log`.
    fn start(&mut self) {
        let log = fs::File::create(self.state().join("makibad.log")).unwrap();
        let mut c = makibad::daemon(self.root.path());
        c.with(|c| {
            c.env_clear()
                .env(makibad::KNOWLEDGE_ENV, makibad::knowledge(self.root.path()))
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", self.path("home"))
                .env("XDG_CONFIG_HOME", self.path("config"))
                .env("XDG_STATE_HOME", self.path("state"))
                .env("MAKIBA_STATE_DIR", self.state())
                .env("MAKIBA_PERSON_KEY", self.path("config/makiba/person.key"))
                .env("MAKIBA_BASE", "stub")
                .env("MAKIBA_STUB_SUBSCRIBE_DELAY_MS", "0")
                .env("MAKIBA_DOOR", "none")
                .env("MAKIBA_PORT", "0")
                .stdin(Stdio::null());
        });
        let child = c.stdout(Stdio::null()).stderr(log).spawn().unwrap();
        self.child = Some(child);
    }

    /// End `makibad` cleanly (SIGTERM) and wait for it.
    fn stop(&mut self) {
        let mut child = self.child.take().expect("makibad was started");
        // SAFETY: kill(2) of a child this test started.
        unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
        let until = Instant::now() + Duration::from_secs(30);
        while Instant::now() < until {
            if child.try_wait().unwrap().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = child.kill();
        let _ = child.wait();
        panic!("makibad did not end within 30 s of SIGTERM");
    }

    fn read(&self) -> Option<Connection> {
        Connection::open_with_flags(
            self.state().join("state.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .ok()
    }

    /// The database's clock now, as a Julian day.
    fn db_now(&self) -> f64 {
        self.read()
            .unwrap()
            .query_row("SELECT julianday('now')", [], |r| r.get(0))
            .unwrap()
    }

    /// Whether the loop has written a heartbeat after the one of its start.
    fn beat_since_start(&self) -> bool {
        self.read()
            .and_then(|c| {
                c.query_row(
                    "SELECT julianday(heartbeat_at) > julianday(started_at)
                     FROM dispatcher WHERE id = 1",
                    [],
                    |r| r.get::<_, bool>(0),
                )
                .ok()
            })
            .unwrap_or(false)
    }

    /// The (actor, payload) of the events of `kind`, oldest first.
    fn events(&self, kind: &str) -> Vec<(Option<String>, Value)> {
        let Some(conn) = self.read() else {
            return Vec::new();
        };
        let Ok(mut q) =
            conn.prepare("SELECT actor, payload_json FROM events WHERE kind = ?1 ORDER BY id")
        else {
            return Vec::new();
        };
        q.query_map([kind], |r| {
            Ok((r.get::<_, Option<String>>(0)?, r.get::<_, String>(1)?))
        })
        .unwrap()
        .map(|row| {
            let (actor, p) = row.unwrap();
            (actor, serde_json::from_str(&p).unwrap())
        })
        .collect()
    }
}

impl Drop for Org {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
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

/// The lines of `log` that say a tick was slow.
fn slow_lines(log: &str) -> Vec<&str> {
    log.lines().filter(|l| l.contains("a tick took")).collect()
}

#[test]
fn a_tick_held_in_a_pass_logs_one_line_and_one_event_naming_it() {
    let mut org = Org::new();
    fs::write(org.state().join("makibad.hold-at"), "pass:close").unwrap();
    org.start();
    let held = org.state().join("makibad.held");
    wait("the close pass to be held", Duration::from_secs(60), || {
        held.exists().then_some(())
    });
    let since = Instant::now();
    let db_since = org.db_now();
    wait(
        "the threshold to pass by both clocks",
        Duration::from_secs(60),
        || {
            let db = (org.db_now() - db_since) * 86400.0;
            let mono = since.elapsed().as_secs_f64();
            (db >= SLOW_TICK_S + MARGIN_S && mono >= SLOW_TICK_S + MARGIN_S).then_some(())
        },
    );
    fs::remove_file(&held).unwrap();
    wait("a tick_slow event", Duration::from_secs(60), || {
        (!org.events("tick_slow").is_empty()).then_some(())
    });
    org.stop();

    let log = org.log();
    let lines = slow_lines(&log);
    assert_eq!(
        lines.len(),
        1,
        "not one slow-tick line in makibad.log:\n{log}"
    );
    let line = lines[0];
    let rest = line
        .strip_prefix("makibad: a tick took ")
        .unwrap_or_else(|| panic!("the line does not start as it should: {line}"));
    let (total, slowest) = rest
        .split_once(" s; slowest: close ")
        .unwrap_or_else(|| panic!("the line does not name close as the slowest: {line}"));
    let slowest = slowest
        .strip_suffix(" s")
        .unwrap_or_else(|| panic!("the line does not end in seconds: {line}"));
    let total: f64 = total.parse().unwrap();
    let slowest: f64 = slowest.parse().unwrap();
    assert!(total >= SLOW_TICK_S, "the tick's total is {total}: {line}");
    assert!(slowest >= SLOW_TICK_S, "close's time is {slowest}: {line}");

    let events = org.events("tick_slow");
    assert_eq!(events.len(), 1, "not one tick_slow event: {events:?}");
    let (actor, p) = &events[0];
    assert_eq!(actor.as_deref(), Some("makibad"), "{p}");
    assert_eq!(p["slowest"]["pass"], "close", "{p}");
    assert!(p["slowest"]["ms"].as_u64().unwrap() >= 5000, "{p}");
    assert!(p["total_ms"].as_u64().unwrap() >= 5000, "{p}");
    assert!(
        p["passes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["pass"] == "close" && e["ms"].as_u64().is_some_and(|ms| ms >= 5000)),
        "the passes do not name close: {p}"
    );
}

#[test]
fn with_no_hold_no_tick_is_slow() {
    let mut org = Org::new();
    org.start();
    wait(
        "dispatcher_started and a heartbeat of the loop",
        Duration::from_secs(60),
        || (!org.events("dispatcher_started").is_empty() && org.beat_since_start()).then_some(()),
    );
    org.stop();

    let log = org.log();
    assert!(
        slow_lines(&log).is_empty(),
        "a slow-tick line with no hold:\n{log}"
    );
    let events = org.events("tick_slow");
    assert!(
        events.is_empty(),
        "a tick_slow event with no hold: {events:?}"
    );
}
