//! M2-2: `makibad` writes down what its organization's terminals report (spec section 10). The
//! daemon runs in this process on `Local`, whose terminals run the test agent, so the whole path is
//! covered: the base's event stream, the monitor behind it, the daemon's writer and the database.
//! One test, because `daemon::run` stops only on a signal, which reaches the whole process.
//!
//! Three kinds of terminal run the same scripted agent (`working`, `blocked:question`, `working`,
//! `done`): one of this organization (a task id, and a run seeded in the database), one with no
//! tags and one of another organization. Only the first is written down.

mod common;

use common::local::Local;
use makiba::base::{Base, LaunchSpec, State, TerminalRef};
use makiba::daemon;
use makiba::db::writer::{RunUpsert, StateWriter};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

const AGENT: &str = env!("CARGO_BIN_EXE_makiba-fake-agent");

fn wait<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let until = Instant::now() + Duration::from_secs(20);
    while Instant::now() < until {
        if let Some(v) = f() {
            return v;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("timed out waiting for {what}");
}

fn reader(dir: &Path) -> Option<Connection> {
    Connection::open_with_flags(dir.join("state.db"), OpenFlags::SQLITE_OPEN_READ_ONLY).ok()
}

fn subscribed(dir: &Path) -> bool {
    reader(dir)
        .and_then(|c| {
            c.query_row("SELECT subscribed FROM dispatcher WHERE id = 1", [], |r| {
                r.get::<_, i64>(0)
            })
            .ok()
        })
        .is_some_and(|s| s == 1)
}

/// What the monitor wrote: `(kind, run_id, payload)` in id order.
fn written(dir: &Path) -> Vec<(String, Option<i64>, Value)> {
    let Some(conn) = reader(dir) else {
        return Vec::new();
    };
    let Ok(mut q) = conn.prepare(
        "SELECT kind, run_id, payload_json FROM events
         WHERE kind IN ('terminal_status', 'terminal_turn', 'terminal_gone') ORDER BY id",
    ) else {
        return Vec::new();
    };
    q.query_map([], |r| {
        let payload: String = r.get(2)?;
        Ok((
            r.get(0)?,
            r.get(1)?,
            serde_json::from_str(&payload).unwrap(),
        ))
    })
    .map(|rows| rows.map(Result::unwrap).collect())
    .unwrap_or_default()
}

fn launch(
    base: &Local,
    name: &str,
    tags: &[(&str, &str)],
    steps: &[&str],
    record: &Path,
) -> TerminalRef {
    let mut argv = vec![AGENT.to_string(), "--record".into()];
    argv.push(record.to_string_lossy().into_owned());
    argv.extend(steps.iter().map(|s| s.to_string()));
    base.launch(&LaunchSpec {
        argv,
        cwd: record.parent().unwrap().to_path_buf(),
        name: name.into(),
        tags: tags
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<BTreeMap<_, _>>(),
        env: BTreeMap::new(),
        unset: Vec::new(),
    })
    .unwrap()
}

/// `working, blocked(question), working, done`, then `last`.
fn script(last: &'static str) -> Vec<&'static str> {
    vec!["working", "blocked:question", "working", "done", last]
}

fn status(task: &str, role: Value, state: &str, kind: Value) -> Value {
    json!({
        "task_id": task, "role": role, "spawn_id": null, "state": state, "kind": kind, "msg": null,
    })
}

#[test]
fn the_daemon_records_the_status_and_exit_of_its_own_terminals_only() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("state");
    std::fs::create_dir_all(&dir).unwrap();
    let org = std::fs::canonicalize(&dir)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let record = tmp.path().join("record");

    // The run the worker belongs to exists before it reports.
    let run_id = {
        let w = StateWriter::open(&dir.join("state.db")).unwrap();
        w.upsert_run(&RunUpsert {
            task_id: "t-worker",
            project_slug: "p",
            // Not `in_use`: the daemon would record an `in_use` run with no terminal as lost (M2-5).
            status: Some("queued"),
            ..Default::default()
        })
        .unwrap()
    };

    let local = Local::new();
    let for_daemon = local.clone();
    let daemon_dir = dir.clone();
    let daemon =
        std::thread::spawn(move || daemon::run(&daemon_dir, &|| Box::new(for_daemon.clone())));
    wait("the daemon to be subscribed", || {
        subscribed(&dir).then_some(())
    });

    // Terminals that are not this organization's: no tags, and another organization's.
    let foreign = [
        launch(&local, "untagged", &[], &script("wait-line"), &record),
        launch(
            &local,
            "other-org",
            &[
                ("makiba.org", "/somewhere/else"),
                ("makiba.task_id", "t-other"),
            ],
            &script("wait-line"),
            &record,
        ),
    ];
    for t in &foreign {
        wait("a foreign agent to be done", || {
            let s = local.get(t).unwrap().status?;
            (s.state == State::Done).then_some(())
        });
    }
    // `end` is a synchronous event on the stream: once the barrier below has been written, so has
    // everything the foreign terminals queued before it.
    for t in &foreign {
        local.end(t).unwrap();
    }

    // This organization's worker runs the script and exits with 7.
    let worker = launch(
        &local,
        "worker",
        &[
            ("makiba.org", org.as_str()),
            ("makiba.role", "worker"),
            ("makiba.task_id", "t-worker"),
        ],
        &script("exit:7"),
        &record,
    );
    wait("the worker's terminal_gone", || {
        written(&dir)
            .iter()
            .any(|(k, ..)| k == "terminal_gone")
            .then_some(())
    });
    // The base removes it after it exited: still one terminal_gone.
    local.end(&worker).unwrap();

    // Barrier: a later terminal of this organization, whose first status is written last.
    launch(
        &local,
        "barrier",
        &[
            ("makiba.org", org.as_str()),
            ("makiba.task_id", "t-barrier"),
        ],
        &["working", "wait-line"],
        &record,
    );
    wait("the barrier's status", || {
        written(&dir)
            .iter()
            .any(|(_, _, p)| p["task_id"] == "t-barrier")
            .then_some(())
    });

    let got = written(&dir);
    let expected = vec![
        (
            "terminal_status",
            Some(run_id),
            status("t-worker", json!("worker"), "working", Value::Null),
        ),
        (
            "terminal_status",
            Some(run_id),
            status("t-worker", json!("worker"), "blocked", json!("question")),
        ),
        (
            "terminal_status",
            Some(run_id),
            status("t-worker", json!("worker"), "working", Value::Null),
        ),
        (
            "terminal_status",
            Some(run_id),
            status("t-worker", json!("worker"), "done", Value::Null),
        ),
        (
            "terminal_gone",
            Some(run_id),
            json!({"task_id": "t-worker", "role": "worker", "spawn_id": null, "reason": "exited", "exit_code": 7}),
        ),
        (
            "terminal_status",
            None,
            status("t-barrier", Value::Null, "working", Value::Null),
        ),
    ]
    .into_iter()
    .map(|(k, r, p)| (k.to_string(), r, p))
    .collect::<Vec<_>>();
    assert_eq!(got, expected);

    // A stop, so the daemon thread ends and says so.
    let ok = std::process::Command::new("kill")
        .args(["-TERM", &std::process::id().to_string()])
        .status()
        .unwrap()
        .success();
    assert!(ok, "kill -TERM failed");
    daemon.join().unwrap().unwrap();
}
