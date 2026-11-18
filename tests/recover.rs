//! M2-5: after a restart `makibad` rebuilds its view from the base's tags (spec sections 6.3 and
//! 10). One daemon is run three times on the same `Local`, state folder and database; between the
//! first two it is stopped (SIGTERM, a clean stop, not a hard crash) while the terminals go on.
//! One test, because `daemon::run` stops only on a signal, which reaches the whole process.
//!
//! Four terminals of this organization run the scripted test agent:
//! - `worker`, with a run (`t-worker`): `working`, wait for a line, `done`. It is sent a line
//!   while the daemon is down, so its `done` is reported to nobody.
//! - `lost`, with a run (`t-lost`): it is ended while the daemon is down.
//! - `secretary`: role `secretary`, no task id. Legitimate, never reported.
//! - `ghost`: role `worker` and a task id, but no run. Unknown: reported once, never ended.

mod common;

use common::local::Local;
use makiba::base::{Base, LaunchSpec, State, TerminalRef};
use makiba::daemon;
use makiba::db::writer::{RunUpsert, StateWriter};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
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

/// Rows of `sql` as `(a, b)`, read-only; empty while the database is not there yet.
fn rows(dir: &Path, sql: &str) -> Vec<(Option<i64>, String)> {
    let Ok(conn) =
        Connection::open_with_flags(dir.join("state.db"), OpenFlags::SQLITE_OPEN_READ_ONLY)
    else {
        return Vec::new();
    };
    let Ok(mut q) = conn.prepare(sql) else {
        return Vec::new();
    };
    q.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .map(|rows| rows.map(Result::unwrap).collect())
        .unwrap_or_default()
}

/// `(run_id, payload)` of the events of `kind`, in id order.
fn events(dir: &Path, kind: &str) -> Vec<(Option<i64>, Value)> {
    rows(
        dir,
        &format!("SELECT run_id, payload_json FROM events WHERE kind = '{kind}' ORDER BY id"),
    )
    .into_iter()
    .map(|(run, p)| (run, serde_json::from_str(&p).unwrap()))
    .collect()
}

/// The states written for the terminal that carries `task` (or none) and `role`.
fn states(dir: &Path, task: Option<&str>, role: &str) -> Vec<String> {
    events(dir, "terminal_status")
        .into_iter()
        .filter(|(_, p)| p["task_id"].as_str() == task && p["role"] == role)
        .map(|(_, p)| p["state"].as_str().unwrap().to_string())
        .collect()
}

fn gone_of(dir: &Path, task: &str) -> Vec<(Option<i64>, Value)> {
    events(dir, "terminal_gone")
        .into_iter()
        .filter(|(_, p)| p["task_id"] == task)
        .collect()
}

fn launch(
    base: &Local,
    name: &str,
    tags: &[(&str, &str)],
    steps: &[&str],
    dir: &Path,
) -> TerminalRef {
    let mut argv = vec![AGENT.to_string(), "--record".into()];
    argv.push(dir.join("record").to_string_lossy().into_owned());
    argv.extend(steps.iter().map(|s| s.to_string()));
    base.launch(&LaunchSpec {
        argv,
        cwd: dir.to_path_buf(),
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

fn state_of(local: &Local, t: &TerminalRef) -> Option<State> {
    local.get(t).ok()?.status.map(|s| s.state)
}

fn start(local: &Local, dir: &Path) -> std::thread::JoinHandle<Result<(), String>> {
    let (for_daemon, dir) = (local.clone(), dir.to_path_buf());
    std::thread::spawn(move || daemon::run(&dir, &|| Box::new(for_daemon.clone())))
}

fn stop(daemon: std::thread::JoinHandle<Result<(), String>>) {
    let ok = std::process::Command::new("kill")
        .args(["-TERM", &std::process::id().to_string()])
        .status()
        .unwrap()
        .success();
    assert!(ok, "kill -TERM failed");
    daemon.join().unwrap().unwrap();
}

/// Wait until `n` `base_resynced` events exist: the view is rebuilt once the last of them is
/// written, which is later than `dispatcher.subscribed`.
fn resynced(dir: &Path, n: usize) -> Vec<Value> {
    wait(&format!("base_resynced number {n}"), || {
        let all = events(dir, "base_resynced");
        (all.len() >= n).then(|| all.into_iter().map(|(_, p)| p).collect())
    })
}

/// What the secretary was told about unknown (ghost) terminals: the notice bodies, the
/// `message_sent` events and the pending deliveries behind them. Notices of other origins (the
/// anomaly pass reports a lost run's `gone`) are not counted: this test is about the ghost.
fn told(dir: &Path) -> (Vec<String>, usize, usize) {
    const GHOST: &str = "m.recipient = 'secretary' AND m.kind = 'notice' \
                         AND m.body LIKE 'Unknown terminal:%'";
    let bodies = rows(
        dir,
        &format!("SELECT m.id, m.body FROM messages m WHERE {GHOST} ORDER BY m.id"),
    )
    .into_iter()
    .map(|(_, b)| b)
    .collect();
    let sent = rows(
        dir,
        &format!(
            "SELECT e.id, 'x' FROM events e JOIN messages m ON m.event_id = e.id
             WHERE e.kind = 'message_sent' AND {GHOST}"
        ),
    )
    .len();
    let pending = rows(
        dir,
        &format!(
            "SELECT count(*), 'x' FROM event_deliveries d JOIN messages m ON m.event_id = d.source_event_id
             WHERE d.recipient = 'secretary' AND d.status = 'pending' AND {GHOST}"
        ),
    )[0]
    .0
    .unwrap() as usize;
    (bodies, sent, pending)
}

#[test]
fn the_daemon_rebuilds_its_view_after_a_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("state");
    std::fs::create_dir_all(&dir).unwrap();
    let org = std::fs::canonicalize(&dir)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let org = org.as_str();

    let (worker_run, lost_run) = {
        let w = StateWriter::open(&dir.join("state.db")).unwrap();
        let run = |task_id| {
            w.upsert_run(&RunUpsert {
                task_id,
                project_slug: "p",
                ..Default::default()
            })
            .unwrap()
        };
        (run("t-worker"), run("t-lost"))
    };

    let local = Local::new();
    let worker = launch(
        &local,
        "worker",
        &[
            ("makiba.org", org),
            ("makiba.role", "worker"),
            ("makiba.task_id", "t-worker"),
        ],
        &["working", "wait-line", "done", "wait-line"],
        tmp.path(),
    );
    let lost = launch(
        &local,
        "lost",
        &[
            ("makiba.org", org),
            ("makiba.role", "worker"),
            ("makiba.task_id", "t-lost"),
        ],
        &["working", "wait-line"],
        tmp.path(),
    );
    let secretary = launch(
        &local,
        "secretary",
        &[("makiba.org", org), ("makiba.role", "secretary")],
        &["working", "wait-line"],
        tmp.path(),
    );
    let ghost = launch(
        &local,
        "ghost",
        &[
            ("makiba.org", org),
            ("makiba.role", "worker"),
            ("makiba.task_id", "t-ghost"),
        ],
        &["working", "wait-line"],
        tmp.path(),
    );
    for t in [&worker, &lost, &secretary, &ghost] {
        wait("an agent to be working", || {
            (state_of(&local, t) == Some(State::Working)).then_some(())
        });
    }

    // First start: the view is built from the tags alone.
    let first = start(&local, &dir);
    let resyncs = resynced(&dir, 1);
    assert_eq!(resyncs[0]["reason"], "start");
    assert_eq!(resyncs[0]["terminals"], 4);
    assert_eq!(states(&dir, Some("t-worker"), "worker"), ["working"]);
    let (bodies, sent, pending) = told(&dir);
    assert_eq!(
        bodies.len(),
        1,
        "the ghost is reported, the secretary is not"
    );
    assert!(
        bodies[0].contains("ghost"),
        "the notice names the terminal: {}",
        bodies[0]
    );
    assert_eq!((sent, pending), (1, 1));
    stop(first);

    // Downtime: the worker is told to go on and reports `done` to nobody; another terminal
    // is removed.
    local.send(&worker, "go on", false).unwrap();
    wait("the worker to be done", || {
        (state_of(&local, &worker) == Some(State::Done)).then_some(())
    });
    local.end(&lost).unwrap();
    assert!(!events(&dir, "terminal_status").is_empty());
    assert_eq!(states(&dir, Some("t-worker"), "worker"), ["working"]);
    assert!(gone_of(&dir, "t-lost").is_empty());

    // Second start: the `done` is recorded, the loss is recorded once, nothing else repeats.
    let second = start(&local, &dir);
    let resyncs = resynced(&dir, 2);
    assert_eq!(resyncs[1]["reason"], "start");
    assert_eq!(resyncs[1]["terminals"], 3);
    let check = |after: &str| {
        assert_eq!(
            states(&dir, Some("t-worker"), "worker"),
            ["working", "done"],
            "{after}"
        );
        assert_eq!(
            states(&dir, Some("t-lost"), "worker"),
            ["working"],
            "{after}"
        );
        assert_eq!(states(&dir, None, "secretary"), ["working"], "{after}");
        assert_eq!(
            states(&dir, Some("t-ghost"), "worker"),
            ["working"],
            "{after}"
        );
        let gone = gone_of(&dir, "t-lost");
        assert_eq!(gone.len(), 1, "{after}: one terminal_gone for the lost run");
        assert_eq!(gone[0].0, Some(lost_run), "{after}");
        assert_eq!(
            events(&dir, "terminal_gone").len(),
            1,
            "{after}: no other loss"
        );
        assert_eq!(told(&dir).1, 1, "{after}: the ghost is reported once");
        assert_eq!((told(&dir).0.len(), told(&dir).2), (1, 1), "{after}");
        let done = events(&dir, "terminal_status")
            .into_iter()
            .find(|(_, p)| p["state"] == "done")
            .unwrap();
        assert_eq!(done.0, Some(worker_run), "{after}");
    };
    check("after the restart");

    // A Resync while the daemon runs changes nothing either, and is written down.
    local.resync();
    let resyncs = resynced(&dir, 3);
    assert_eq!(resyncs[2]["reason"], "resync");
    assert_eq!(resyncs[2]["terminals"], 3);
    check("after a Resync");
    stop(second);

    // Third start: the ghost is still alive, still unreported twice, still not ended.
    let third = start(&local, &dir);
    resynced(&dir, 4);
    check("after a second restart");
    assert_eq!(state_of(&local, &ghost), Some(State::Working));
    assert!(local.get(&ghost).unwrap().alive);
    stop(third);
}
