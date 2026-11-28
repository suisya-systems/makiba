//! Issue 408: `makiba range approve <id> --read <read id>` approves the read the person looked at,
//! not a newer one. The read named is passed through to the door, which refuses a read that is not
//! the range's newest complete read as `stale` and approves nothing.
//!
//! Each test seeds one `proposed` range with two complete reads, `older` then `newer`, whose
//! `through` (0) is past no trigger, so the range is not read again before the approval.
//!
//! 1. In this process: the command line is parsed by the clap definition, made a request by
//!    `makiba::cli::request`, and sent to a door on the stand-in base with the person key.
//! 2. The real `makiba daemon` with the person's socket, and the real `makiba` binary as the
//!    person. `MAKIBA_GH` is a stand-in that fails, so the read the approval triggers is a failed
//!    read that reaches no network and changes nothing asserted here. The test starts once
//!    `makibad` has recorded its merge approval mode, the one the preview reads (Issue 638): its
//!    heartbeat and its door come before that record.
//! 3. Issue 638: the same, with `makibad` held after its heartbeat and its door and before it
//!    records its mode (the hold point `tick`, debug builds). The approval says the mode is not
//!    known because `makibad` is starting, approves nothing (exit 5), and once `makibad` has
//!    recorded its mode the same line approves.
//! 4. Issue 638: a `state.db` the preview cannot open is `unavailable` (exit 6), not `conflict`.

mod common;

use clap::Parser;
use common::makibad;
use makiba::base::stub::Stub;
use makiba::cli::{Makiba, request};
use makiba::daemon::door::{Door, issue_key};
use makiba::db::writer::{NewEvent, StateWriter};
use makiba::spec::{ErrorKind, Role};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

const MAKIBA: &str = env!("CARGO_BIN_EXE_makiba");

/// The range seeded: its id and its two complete reads, `older` then `newer`.
struct Seeded {
    id: i64,
    older: i64,
    newer: i64,
}

/// A project `p` (GitHub `o/p`), a `proposed` range of it and two complete reads of that range.
fn seed(w: &StateWriter) -> Seeded {
    let project = w
        .ensure_project("p", None, Some("https://github.com/o/p"))
        .unwrap();
    w.conn()
        .execute(
            "INSERT INTO ranges (project_id, issues_json, at_once, budget_tasks, brief_head,
                                 proposed_by)
             VALUES (?1, '[7]', 1, 1, 'Do it.', 'secretary')",
            [project],
        )
        .unwrap();
    let id = w.conn().last_insert_rowid();
    let read = || {
        w.append_event(&NewEvent {
            kind: "range_read",
            actor: Some("makibad"),
            payload: Some(&json!({"range_id": id, "complete": true, "through": 0})),
            ..Default::default()
        })
        .unwrap()
    };
    let older = read();
    let newer = read();
    assert!(older < newer);
    Seeded { id, older, newer }
}

/// The range's status and `approved_read_id`.
fn range_row(db: &Path, id: i64) -> (String, Option<i64>) {
    let c = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    c.query_row(
        "SELECT status, approved_read_id FROM ranges WHERE id = ?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .unwrap()
}

/// The `read_id` of each `range_approved` event of range `id`, oldest first.
fn approvals(db: &Path, id: i64) -> Vec<Value> {
    let c = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let mut q = c
        .prepare(
            "SELECT payload_json FROM events WHERE kind = 'range_approved'
               AND json_extract(payload_json, '$.range_id') = ?1 ORDER BY id",
        )
        .unwrap();
    q.query_map([id], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|p| serde_json::from_str::<Value>(&p.unwrap()).unwrap()["read_id"].clone())
        .collect()
}

/// The door's verb and arguments of `makiba range approve <id> --read <read> --mode <mode> --yes`,
/// with the mode of the door in this process (Issue 326: the approval carries the mode shown).
fn approve_request(id: i64, read: i64) -> (&'static str, Value) {
    let (id, read) = (id.to_string(), read.to_string());
    let mode = makiba::daemon::gate::merge_approval_mode();
    let m = Makiba::try_parse_from([
        "makiba", "range", "approve", &id, "--read", &read, "--mode", mode, "--yes",
    ])
    .unwrap_or_else(|e| panic!("the command line is refused: {e}"));
    request(m.verb).unwrap()
}

/// 1. The read named on the command line goes to the door; only the newest read approves.
#[test]
fn the_named_read_is_passed_to_the_door_and_only_the_newest_approves() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("state.db");
    let w = StateWriter::open(&db).unwrap();
    let s = seed(&w);
    let person = issue_key(&w, Role::Person, "person", None, None).unwrap();
    let door = Door::new(w, Arc::new(Stub::new()), "fake".into());

    let (verb, args) = approve_request(s.id, s.older);
    assert_eq!(verb, "range");
    assert_eq!(args["action"], json!("approve"), "{args}");
    assert_eq!(args["id"], json!(s.id), "{args}");
    assert_eq!(args["read"], json!(s.older), "{args}");
    let r = door.handle(Some(&person), verb, args, None, true);
    assert_eq!(
        r.as_ref().err().map(|e| e.kind),
        Some(ErrorKind::Stale),
        "an older read was not refused as stale: {r:?}"
    );
    assert_eq!(range_row(&db, s.id), ("proposed".to_string(), None));
    assert!(approvals(&db, s.id).is_empty(), "an older read approved");

    let (verb, args) = approve_request(s.id, s.newer);
    assert_eq!(args["read"], json!(s.newer), "{args}");
    let r = door
        .handle(Some(&person), verb, args, None, true)
        .unwrap_or_else(|e| panic!("the newest read was refused: {e:?}"));
    assert_eq!(r["status"], json!("active"), "{r}");
    assert_eq!(range_row(&db, s.id), ("active".to_string(), Some(s.newer)));
    assert_eq!(approvals(&db, s.id), vec![json!(s.newer)]);
}

// ---- the real binary ---------------------------------------------------------------------------

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// One organization run by `makiba daemon` on the stand-in base with the person's socket open,
/// stopped when dropped.
struct Daemon {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    port: u16,
    child: Child,
}

impl Daemon {
    fn env(&self, c: &mut Command) {
        let root = &self.root;
        c.env("HOME", root.join("home"))
            .env("MAKIBA_STATE_DIR", root.join("state"))
            .env("XDG_STATE_HOME", root.join("xdg-state"))
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_DATA_HOME", root.join("data"))
            .env("XDG_RUNTIME_DIR", root.join("run"))
            .env("MAKIBA_DOCS_DIR", root.join("docs"))
            .env("MAKIBA_WORKERS_DIR", root.join("workers"))
            .env("MAKIBA_PORT", self.port.to_string())
            .env("MAKIBA_BASE", "stub")
            .env("MAKIBA_STUB_SUBSCRIBE_DELAY_MS", "0")
            .env("MAKIBA_MAX_CONCURRENT_WORKERS", "0")
            .env("MAKIBA_GH", root.join("gh"));
        for v in [
            "MAKIBA_DOOR",
            "MAKIBA_PERSON_KEY",
            "MAKIBA_SHARE_DIR",
            "MAKIBA_LAUNCH_AGENT",
            "MAKIBA_FAKE_AGENT",
        ] {
            c.env_remove(v);
        }
    }

    /// Seeds the range before `makibad` starts, so its first look finds the reads. Unheld, it
    /// returns once `makibad` has recorded its merge approval mode; `hold`, once `makibad` is held
    /// at its first tick, after its heartbeat and its door and before it records the mode.
    fn start(hold: bool) -> (Daemon, Seeded) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        for d in ["home", "state", "config", "data", "run", "xdg-state"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        // not an executable: the socket folder's mode, owner only
        std::fs::set_permissions(root.join("run"), std::fs::Permissions::from_mode(0o700)).unwrap();
        // A `gh` that fails: a read of the range records a failed read and goes nowhere.
        common::exe::write(
            &root.join("gh"),
            "#!/bin/sh\necho \"no gh in this test\" >&2\nexit 1\n",
        );
        let w = StateWriter::open(&root.join("state/state.db")).unwrap();
        let s = seed(&w);
        drop(w);
        if hold {
            std::fs::write(root.join("state/makibad.hold-at"), "tick").unwrap();
        }
        let mut d = Daemon {
            _tmp: tmp,
            root,
            port: free_port(),
            child: Command::new("true").spawn().unwrap(),
        };
        let mut c = makibad::daemon(&d.root);
        c.with(|c| d.env(c));
        d.child = c
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let door = d.root.join("run/makiba/door.sock");
        let until = Instant::now() + Duration::from_secs(15);
        let mut open = false;
        while Instant::now() < until {
            if UnixStream::connect(&door).is_ok()
                && TcpStream::connect(("127.0.0.1", d.port)).is_ok()
                && d.heartbeat()
                && (!hold || d.held().exists())
            {
                open = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(open, "the door did not open");
        if !hold {
            d.wait_mode();
        }
        (d, s)
    }

    /// The file `makibad` writes where it is held, and waits on until it is removed.
    fn held(&self) -> PathBuf {
        self.root.join("state/makibad.held")
    }

    /// Let a held `makibad` go on.
    fn release(&self) {
        std::fs::remove_file(self.held()).unwrap();
    }

    /// The person's status read of the ledger, as the preview reads it.
    fn status(&self) -> Value {
        makiba::status_local::read(&self.db_path()).unwrap()
    }

    /// Wait, bounded, until `makibad`'s merge approval mode is recorded, as the person's status
    /// reads it: what `range approve` fixes (Issue 638).
    fn wait_mode(&self) {
        let until = Instant::now() + Duration::from_secs(20);
        while Instant::now() < until {
            let mode = self.status()["merge_approval_mode"].clone();
            if mode == json!("human") || mode == json!("acceptance") {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!(
            "makibad did not record its merge approval mode: {}",
            self.status()
        );
    }

    fn db_path(&self) -> PathBuf {
        self.root.join("state/state.db")
    }

    fn heartbeat(&self) -> bool {
        Connection::open_with_flags(self.db_path(), OpenFlags::SQLITE_OPEN_READ_ONLY)
            .and_then(|c| {
                c.query_row(
                    "SELECT heartbeat_at IS NOT NULL FROM dispatcher WHERE id = 1",
                    [],
                    |r| r.get::<_, bool>(0),
                )
            })
            .unwrap_or(false)
    }

    /// The person's `makiba`, with its input closed.
    fn cli(&self, args: &[&str]) -> Output {
        let mut c = Command::new(MAKIBA);
        self.env(&mut c);
        c.args(args).stdin(Stdio::null()).output().unwrap()
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status();
        let until = Instant::now() + Duration::from_secs(5);
        while Instant::now() < until {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn text(o: &Output) -> String {
    format!(
        "exit {:?}\nstdout:\n{}\nstderr:\n{}",
        o.status.code(),
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// 2. `makiba range approve <id> --read <read> --yes` over the person's socket: an older read is
///    `stale` (exit 7) and approves nothing; the newest approves the range on that read.
#[test]
fn makiba_range_approve_with_read_over_the_persons_socket() {
    let (d, s) = Daemon::start(false);
    let db = d.db_path();
    let id = s.id.to_string();

    let out = d.cli(&[
        "range",
        "approve",
        &id,
        "--read",
        &s.older.to_string(),
        "--yes",
    ]);
    assert_eq!(out.status.code(), Some(7), "{}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stdout).contains(&format!("Read {} at ", s.newer)),
        "the preview does not print the newest read's id: {}",
        text(&out)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("stale"),
        "{}",
        text(&out)
    );
    assert_eq!(range_row(&db, s.id), ("proposed".to_string(), None));
    assert!(approvals(&db, s.id).is_empty(), "an older read approved");

    let out = d.cli(&[
        "range",
        "approve",
        &id,
        "--read",
        &s.newer.to_string(),
        "--yes",
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert_eq!(range_row(&db, s.id), ("active".to_string(), Some(s.newer)));
    assert_eq!(approvals(&db, s.id), vec![json!(s.newer)]);
}

/// 3. Issue 638: `makibad` held after its heartbeat and its door, before it records its merge
///    approval mode. The approval says the mode is not known because `makibad` is starting, and
///    approves nothing; once `makibad` has recorded its mode, the same line approves.
#[test]
fn range_approve_while_makibad_starts_says_so_and_approves_once_it_has_started() {
    let (d, s) = Daemon::start(true);
    let db = d.db_path();
    let status = d.status();
    assert_eq!(status["merge_approval_mode"], Value::Null, "{status}");
    assert_eq!(status["makibad"]["subscribed"], json!(false), "{status}");
    let id = s.id.to_string();
    let newer = s.newer.to_string();
    let line = ["range", "approve", &id, "--read", &newer, "--yes"];

    let starting = d.cli(&line);
    assert_eq!(starting.status.code(), Some(5), "{}", text(&starting));
    assert!(
        String::from_utf8_lossy(&starting.stdout).contains("Merge approval mode: unknown"),
        "{}",
        text(&starting)
    );
    let err = String::from_utf8_lossy(&starting.stderr);
    assert!(
        err.contains("makibad is starting and has not recorded it yet"),
        "the answer does not say makibad is starting: {}",
        text(&starting)
    );
    assert!(
        err.contains("run this again once makiba status shows"),
        "the answer does not say to run it again: {}",
        text(&starting)
    );
    assert_eq!(range_row(&db, s.id), ("proposed".to_string(), None));
    assert!(approvals(&db, s.id).is_empty(), "approved with no mode");

    d.release();
    d.wait_mode();
    let started = d.cli(&line);
    assert_eq!(started.status.code(), Some(0), "{}", text(&started));
    let stdout = String::from_utf8_lossy(&started.stdout);
    assert!(
        stdout.contains(&format!("Read {} at ", s.newer)),
        "{}",
        text(&started)
    );
    assert!(
        !stdout.contains("Merge approval mode: unknown"),
        "{}",
        text(&started)
    );
    assert_eq!(range_row(&db, s.id), ("active".to_string(), Some(s.newer)));
    assert_eq!(approvals(&db, s.id), vec![json!(s.newer)]);
}

/// 4. Issue 638: a `state.db` the preview cannot open is `unavailable` (exit 6), with SQLite's
///    message, not `conflict`.
#[test]
fn range_approve_on_a_state_db_it_cannot_open_is_unavailable() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let state = root.join("state");
    std::fs::create_dir_all(&state).unwrap();
    let missing = Command::new(MAKIBA)
        .env("HOME", root.join("home"))
        .env("MAKIBA_STATE_DIR", &state)
        .env("XDG_STATE_HOME", root.join("xdg-state"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_RUNTIME_DIR", root.join("run"))
        .env_remove("MAKIBA_DOOR")
        .env_remove("MAKIBA_PERSON_KEY")
        .args(["range", "approve", "1", "--yes"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(missing.status.code(), Some(6), "{}", text(&missing));
    let err = String::from_utf8_lossy(&missing.stderr);
    assert!(err.contains("cannot open"), "{}", text(&missing));
    assert!(
        err.contains(&state.join("state.db").display().to_string()),
        "{}",
        text(&missing)
    );
    assert!(
        !state.join("state.db").exists(),
        "the preview made a state.db"
    );
}
