//! Issue 90: the records tell terminals of the same role apart (spec sections 6.3 and 10). A
//! terminal's identity is the value of its `makiba.spawn` tag, carried as `spawn_id` in
//! `terminal_status`, `terminal_turn` and `terminal_gone`; the recovery of M2-5 looks the history
//! up by it. No terminal here is launched: a scripted base lists exactly the terminals a boot is
//! given, so what the base says (status, turn, exit) is exact. One test, because `daemon::run`
//! stops only on a signal, which reaches the whole process. The daemon is booted six times on one
//! state folder:
//!
//! 1. an untagged secretary (legacy, no identity): its records carry `spawn_id` null;
//! 2. the first tagged secretary (`sec-1`) with the same status as the legacy one and a lower
//!    turn: it does not inherit the untagged history;
//! 3. the secretary is replaced: `sec-1` is retained dead, `sec-2` is new and starts with the
//!    status `sec-1` ended with and a turn lower than `sec-1`'s;
//! 4. `sec-2` is retained dead;
//! 5. a retained dead replacement `sec-3` that never reported a status: its gone must be written
//!    although the last status or gone record of the role is the old generation's gone, and no
//!    status of its own is written in between that could make it look new;
//! 6. nothing changed: nothing is written twice.

mod common;

use makiba::base::version::PINNED;
use makiba::base::{
    Base, BaseDenies, BaseError, BaseEvent, BaseInfo, Detail, EventStream, LaunchSpec, State,
    Status, Terminal, TerminalRef,
};
use makiba::daemon;
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

/// A base that lists the same terminals for every question and, once subscribed, reports them as
/// one `Resync` and nothing after.
struct Scripted(Vec<Terminal>);

fn unsupported<T>() -> Result<T, BaseError> {
    Err(BaseError::Other(Detail {
        kind: "unsupported".into(),
        message: "the scripted base only lists".into(),
        hint: None,
    }))
}

impl Base for Scripted {
    fn name(&self) -> &'static str {
        "scripted"
    }
    fn hello(&self) -> Result<BaseInfo, BaseError> {
        Ok(BaseInfo {
            name: "scripted".into(),
            version: PINNED.into(),
        })
    }
    fn launch(&self, _: &LaunchSpec) -> Result<TerminalRef, BaseError> {
        unsupported()
    }
    fn list(&self) -> Result<Vec<Terminal>, BaseError> {
        Ok(self.0.clone())
    }
    fn get(&self, t: &TerminalRef) -> Result<Terminal, BaseError> {
        self.0
            .iter()
            .find(|x| &x.id == t)
            .cloned()
            .map_or_else(unsupported, Ok)
    }
    fn read(&self, _: &TerminalRef, _: usize) -> Result<Vec<String>, BaseError> {
        unsupported()
    }
    fn keys(&self, _: &TerminalRef, _: &[&str]) -> Result<(), BaseError> {
        unsupported()
    }
    fn send(&self, _: &TerminalRef, _: &str, _: bool) -> Result<(), BaseError> {
        unsupported()
    }
    fn end(&self, _: &TerminalRef) -> Result<(), BaseError> {
        unsupported()
    }
    fn events(&self) -> Result<EventStream, BaseError> {
        let (tx, rx) = channel();
        tx.send(BaseEvent::Resync(self.0.clone())).unwrap();
        std::mem::forget(tx); // the stream stays open and silent
        Ok(Box::new(rx.into_iter()))
    }
    fn start(&self) -> Result<(), BaseError> {
        Ok(())
    }
    fn worker_denies(&self) -> BaseDenies {
        BaseDenies::default()
    }
}

/// One secretary terminal of organization `org`. `spawn` is its `makiba.spawn` tag, if any;
/// `state` its status, if it reported one; `exit` is `Some(code)` for a retained dead terminal.
fn secretary(
    org: &str,
    id: &str,
    spawn: Option<&str>,
    state: Option<State>,
    turn: Option<u64>,
    exit: Option<i32>,
) -> Terminal {
    let mut tags = BTreeMap::from([
        ("makiba.org".to_string(), org.to_string()),
        ("makiba.role".to_string(), "secretary".to_string()),
    ]);
    if let Some(spawn) = spawn {
        tags.insert("makiba.spawn".into(), spawn.into());
    }
    Terminal {
        id: TerminalRef(id.into()),
        name: id.into(),
        tags,
        status: state.map(|state| Status {
            state,
            kind: None,
            msg: None,
        }),
        turn,
        alive: exit.is_none(),
        exit_code: exit,
    }
}

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

/// The payloads of the events of `kind`, in id order, read-only; empty while there is no database.
fn events(dir: &Path, kind: &str) -> Vec<Value> {
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
        .map(|rows| {
            rows.map(|p| serde_json::from_str(&p.unwrap()).unwrap())
                .collect()
        })
        .unwrap_or_default()
}

/// How many `terminal_status`, `terminal_turn` and `terminal_gone` are written so far.
fn written(dir: &Path) -> usize {
    ["terminal_status", "terminal_turn", "terminal_gone"]
        .iter()
        .map(|k| events(dir, k).len())
        .sum()
}

/// The records of `kind` that carry `spawn_id` `spawn` (`None`: null), as `field` of each.
fn of(dir: &Path, kind: &str, spawn: Option<&str>, field: &str) -> Vec<Value> {
    events(dir, kind)
        .into_iter()
        .filter(|p| p.get("spawn_id").map(|v| v.as_str()) == Some(spawn))
        .map(|p| p[field].clone())
        .collect()
}

/// Boot the daemon on `dir` with a base that lists `list`, and wait until its view is rebuilt
/// (`base_resynced` number `n`, which is written last), then stop it.
fn boot(dir: &Path, list: Vec<Terminal>, n: usize) {
    let dir_for_daemon = dir.to_path_buf();
    let daemon = std::thread::spawn(move || {
        daemon::run(&dir_for_daemon, &|| Box::new(Scripted(list.clone())))
    });
    wait(&format!("base_resynced number {n}"), || {
        (events(dir, "base_resynced").len() >= n).then_some(())
    });
    let ok = std::process::Command::new("kill")
        .args(["-TERM", &std::process::id().to_string()])
        .status()
        .unwrap()
        .success();
    assert!(ok, "kill -TERM failed");
    daemon.join().unwrap().unwrap();
}

#[test]
fn the_records_tell_terminals_of_the_same_role_apart() {
    use State::Working;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("state");
    std::fs::create_dir_all(&dir).unwrap();
    let org = std::fs::canonicalize(&dir)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let org = org.as_str();
    let sec = |id, spawn, state, turn, exit| secretary(org, id, spawn, state, turn, exit);

    // 1. A secretary without the tag: its records say so with an explicit null.
    boot(
        &dir,
        vec![sec("legacy", None, Some(Working), Some(5), None)],
        1,
    );
    let status = events(&dir, "terminal_status");
    assert_eq!(status.len(), 1);
    assert_eq!(
        status[0].get("spawn_id"),
        Some(&Value::Null),
        "the field is there, null: {}",
        status[0]
    );
    assert_eq!(of(&dir, "terminal_turn", None, "turn"), [5]);

    // 2. The first tagged secretary reports what the legacy one reported last, at a lower turn.
    //    It does not take the untagged history for its own; the legacy one stays as recorded.
    boot(
        &dir,
        vec![
            sec("legacy", None, Some(Working), Some(5), None),
            sec("sec-1", Some("sec-1"), Some(Working), Some(3), None),
        ],
        2,
    );
    assert_eq!(
        of(&dir, "terminal_status", Some("sec-1"), "state"),
        ["working"]
    );
    assert_eq!(of(&dir, "terminal_turn", Some("sec-1"), "turn"), [3]);
    assert_eq!(of(&dir, "terminal_status", None, "state"), ["working"]);
    assert_eq!(of(&dir, "terminal_turn", None, "turn"), [5]);

    // 3. The secretary is replaced and the daemon restarted. `sec-1` is retained dead; `sec-2`
    //    is new, its first status equals the last one of `sec-1` and its turn is lower.
    boot(
        &dir,
        vec![
            sec("sec-1", Some("sec-1"), Some(Working), Some(3), Some(1)),
            sec("sec-2", Some("sec-2"), Some(Working), Some(1), None),
        ],
        3,
    );
    assert_eq!(
        of(&dir, "terminal_status", Some("sec-2"), "state"),
        ["working"],
        "the new terminal's first status is recorded although it equals the old one's last"
    );
    assert_eq!(of(&dir, "terminal_turn", Some("sec-2"), "turn"), [1]);
    assert_eq!(of(&dir, "terminal_gone", Some("sec-1"), "exit_code"), [1]);
    assert_eq!(
        of(&dir, "terminal_status", Some("sec-1"), "state"),
        ["working"]
    );
    assert_eq!(of(&dir, "terminal_turn", Some("sec-1"), "turn"), [3]);

    // 4. `sec-2` is retained dead: the gone of `sec-1` does not suppress its own.
    boot(
        &dir,
        vec![sec("sec-2", Some("sec-2"), Some(Working), Some(1), Some(0))],
        4,
    );
    assert_eq!(of(&dir, "terminal_gone", Some("sec-2"), "exit_code"), [0]);

    // 5. `sec-3` never reported a status and is retained dead: nothing but its own gone can be
    //    written for it, and the last status or gone of the role is the gone of `sec-2`.
    boot(
        &dir,
        vec![sec("sec-3", Some("sec-3"), None, None, Some(2))],
        5,
    );
    assert_eq!(
        of(&dir, "terminal_gone", Some("sec-3"), "exit_code"),
        [2],
        "the old generation's gone does not hide the gone of a replacement without a status"
    );
    assert!(of(&dir, "terminal_status", Some("sec-3"), "state").is_empty());
    assert_eq!(
        of(&dir, "terminal_gone", Some("sec-3"), "reason"),
        ["exited"]
    );

    // 6. Nothing changed: a restart with the same terminals writes nothing again.
    let before = written(&dir);
    boot(
        &dir,
        vec![
            sec("sec-2", Some("sec-2"), Some(Working), Some(1), Some(0)),
            sec("sec-3", Some("sec-3"), None, None, Some(2)),
        ],
        6,
    );
    assert_eq!(
        written(&dir),
        before,
        "an unchanged generation writes nothing twice"
    );
    for (spawn, gone) in [("sec-1", 1), ("sec-2", 1), ("sec-3", 1)] {
        assert_eq!(
            of(&dir, "terminal_gone", Some(spawn), "reason").len(),
            gone,
            "one terminal_gone for {spawn}"
        );
    }
}
