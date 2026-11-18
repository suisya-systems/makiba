//! M2-2a: the records of a terminal say whose terminal it is (`role`, from `makiba.role`), also
//! the removal record, which takes it from the tags the monitor kept. Same setup as
//! `tests/monitor.rs`: the daemon runs in this process on `Local`, and the terminals run the test
//! agent. Three terminals of this organization: a secretary (no task id), a worker, and one with no
//! `makiba.role` tag.

mod common;

use common::local::Local;
use makiba::base::{Base, LaunchSpec};
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

/// `(kind, payload)` of what the monitor wrote, in id order.
fn written(dir: &Path) -> Vec<(String, Value)> {
    let Some(conn) = reader(dir) else {
        return Vec::new();
    };
    let Ok(mut q) = conn.prepare(
        "SELECT kind, payload_json FROM events
         WHERE kind IN ('terminal_status', 'terminal_turn', 'terminal_gone') ORDER BY id",
    ) else {
        return Vec::new();
    };
    q.query_map([], |r| {
        let payload: String = r.get(1)?;
        Ok((r.get(0)?, serde_json::from_str(&payload).unwrap()))
    })
    .map(|rows| rows.map(Result::unwrap).collect())
    .unwrap_or_default()
}

fn launch(base: &Local, name: &str, tags: &[(&str, &str)], steps: &[&str], record: &Path) {
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
    .unwrap();
}

/// The records whose `role` is `role` (JSON null for `Value::Null`), as `(kind, payload)`.
fn of_role(dir: &Path, role: &Value) -> Vec<(String, Value)> {
    written(dir)
        .into_iter()
        .filter(|(_, p)| p.get("role") == Some(role))
        .collect()
}

#[test]
fn records_carry_the_role_of_their_terminal() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("state");
    std::fs::create_dir_all(&dir).unwrap();
    let org = std::fs::canonicalize(&dir)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let record = tmp.path().join("record");

    {
        let w = StateWriter::open(&dir.join("state.db")).unwrap();
        w.upsert_run(&RunUpsert {
            task_id: "t-worker",
            project_slug: "p",
            ..Default::default()
        })
        .unwrap();
    }

    let local = Local::new();
    let for_daemon = local.clone();
    let daemon_dir = dir.clone();
    let daemon =
        std::thread::spawn(move || daemon::run(&daemon_dir, &|| Box::new(for_daemon.clone())));
    wait("the daemon to be subscribed", || {
        subscribed(&dir).then_some(())
    });

    let gone = |role: &Value| {
        of_role(&dir, role)
            .iter()
            .any(|(k, _)| k == "terminal_gone")
            .then_some(())
    };

    // The secretary has no task id; its exit is one terminal_gone.
    launch(
        &local,
        "secretary",
        &[("makiba.org", org.as_str()), ("makiba.role", "secretary")],
        &["working", "done", "exit:0"],
        &record,
    );
    wait("the secretary's terminal_gone", || {
        gone(&json!("secretary"))
    });

    launch(
        &local,
        "worker",
        &[
            ("makiba.org", org.as_str()),
            ("makiba.role", "worker"),
            ("makiba.task_id", "t-worker"),
        ],
        &["working", "done", "exit:7"],
        &record,
    );
    wait("the worker's terminal_gone", || gone(&json!("worker")));

    // No `makiba.role` tag: the role is null, not a guess.
    launch(
        &local,
        "norole",
        &[("makiba.org", org.as_str()), ("makiba.task_id", "t-norole")],
        &["working", "wait-line"],
        &record,
    );
    wait("the status of the terminal without a role", || {
        (!of_role(&dir, &Value::Null).is_empty()).then_some(())
    });

    let secretary = of_role(&dir, &json!("secretary"));
    let kinds: Vec<_> = secretary.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        kinds,
        ["terminal_status", "terminal_status", "terminal_gone"]
    );
    assert!(secretary.iter().all(|(_, p)| p["task_id"] == Value::Null));
    assert_eq!(secretary[2].1["reason"], "exited");

    let worker = of_role(&dir, &json!("worker"));
    assert_eq!(worker.len(), 3);
    assert!(worker.iter().all(|(_, p)| p["task_id"] == "t-worker"));
    assert_eq!(worker[2].1["exit_code"], 7);

    let norole = of_role(&dir, &Value::Null);
    assert_eq!(norole.len(), 1);
    assert_eq!(norole[0].1["task_id"], "t-norole");
    assert_eq!(norole[0].1["state"], "working");

    // Every record has the key at all, so a missing tag is null and not an absent field.
    assert!(written(&dir).iter().all(|(_, p)| p.get("role").is_some()));

    let ok = std::process::Command::new("kill")
        .args(["-TERM", &std::process::id().to_string()])
        .status()
        .unwrap()
        .success();
    assert!(ok, "kill -TERM failed");
    daemon.join().unwrap().unwrap();
}
