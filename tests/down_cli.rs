//! Issue 41: `makiba down` from the person's shell stops a running `makibad` before it records the
//! state, and refuses inside an agent. `makibad` runs on the in-process stand-in base without a
//! socket (`MAKIBA_DOOR=none`), in a temporary state folder.

use std::path::Path;
use std::process::{Child, Command, Output};
use std::time::{Duration, Instant};

fn makiba(state: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_makiba"));
    c.args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", state)
        .env("MAKIBA_STATE_DIR", state)
        .env("MAKIBA_BASE", "stub")
        .env("MAKIBA_DOOR", "none")
        .env("MAKIBA_PORT", "0");
    c
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// Kills the daemon if the test did not stop it.
struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn down_stops_makibad_and_suspends_the_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path();
    let mut daemon = Daemon(makiba(state, &["daemon"]).spawn().unwrap());
    let subscribed = || {
        rusqlite::Connection::open(state.join("state.db"))
            .and_then(|c| {
                c.query_row("SELECT subscribed FROM dispatcher WHERE id = 1", [], |r| {
                    r.get::<_, i64>(0)
                })
            })
            .is_ok_and(|s| s == 1)
    };
    let until = Instant::now() + Duration::from_secs(20);
    while !subscribed() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(subscribed(), "makibad did not subscribe");
    {
        let w = makiba::db::writer::StateWriter::open(&state.join("state.db")).unwrap();
        w.upsert_run(&makiba::db::writer::RunUpsert {
            task_id: "t1",
            project_slug: "p",
            status: Some("in_use"),
            ..Default::default()
        })
        .unwrap();
    }

    let out = makiba(state, &["down"]).output().unwrap();
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("makibad: stopped"), "{}", text(&out));
    assert!(text(&out).contains("suspended: t1"), "{}", text(&out));
    let exited = daemon.0.try_wait().unwrap();
    assert!(exited.is_some(), "makibad still runs after down");

    // Run again with makibad gone: nothing to stop, the state stays.
    let out = makiba(state, &["down"]).output().unwrap();
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        text(&out).contains("makibad: not running"),
        "{}",
        text(&out)
    );
}

#[test]
fn down_refuses_inside_an_agent_and_bad_arguments() {
    let tmp = tempfile::tempdir().unwrap();
    let out = makiba(tmp.path(), &["down"])
        .env("CLAUDE_CODE_SESSION_ID", "x")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(text(&out).contains("login shell"), "{}", text(&out));
    assert!(
        !tmp.path().join("state.db").exists(),
        "down wrote the state"
    );
    let out = makiba(tmp.path(), &["down", "--bogus"]).output().unwrap();
    assert!(!out.status.success());
    assert!(text(&out).contains("usage: makiba down"), "{}", text(&out));
}
