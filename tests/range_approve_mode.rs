//! Issue 326: a range approval shows and binds the merge approval mode it fixes.
//!
//! `status` shows, for a `proposed` range, the merge approval mode approving it now would fix
//! (`ranges[].approves_under`) and the range it replaces (`ranges[].replaces`). `range approve`
//! carries the mode the person was shown (`approval_mode`, `makiba range approve --mode`); when
//! `makibad`'s mode differs, the approval is refused as `stale` and nothing is approved. Read and
//! approved under one mode, the range records that mode.
//!
//! Each test seeds, before any door or `makibad` opens the state: a project `p` (GitHub `o/p`), a
//! range A `active` (approved by the person under `human`), a range B `proposed` that replaces A,
//! and one complete read of B whose `through` (0) is past no trigger.
//!
//! 1. In this process: a door on the stand-in base with the person key, the process environment
//!    switched between `human` and `acceptance`.
//! 2. and 3. The real `makiba daemon` with the person's socket, restarted under another mode on the
//!    same state, and the real `makiba` binary as the person. `MAKIBA_GH` is a stand-in that fails,
//!    so a read `makibad` starts is a failed read that changes no complete read.
//!
//! Every test holds `ENV`: test 1 changes this process's environment, and the others spawn
//! children, which must not read it while it changes.

mod common;

use common::makibad;
use makiba::base::stub::Stub;
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
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

const MAKIBA: &str = env!("CARGO_BIN_EXE_makiba");
const MODE_ENV: &str = "MAKIBA_MERGE_APPROVAL_MODE";

static ENV: Mutex<()> = Mutex::new(());

/// The ranges seeded: A (active), B (proposed, replaces A) and B's complete read.
struct Seeded {
    a: i64,
    b: i64,
    read: i64,
}

fn seed(w: &StateWriter) -> Seeded {
    let project = w
        .ensure_project("p", None, Some("https://github.com/o/p"))
        .unwrap();
    w.conn()
        .execute(
            "INSERT INTO ranges (project_id, issues_json, at_once, budget_tasks, brief_head,
                                 proposed_by, status, approval_mode, approved_by, approved_at)
             VALUES (?1, '[5]', 1, 1, 'Do it.', 'secretary', 'active', 'human', 'person',
                     strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
            [project],
        )
        .unwrap();
    let a = w.conn().last_insert_rowid();
    w.conn()
        .execute(
            "INSERT INTO ranges (project_id, issues_json, at_once, budget_tasks, brief_head,
                                 proposed_by, replaces)
             VALUES (?1, '[7]', 1, 1, 'Do it again.', 'secretary', ?2)",
            [project, a],
        )
        .unwrap();
    let b = w.conn().last_insert_rowid();
    let read = w
        .append_event(&NewEvent {
            kind: "range_read",
            actor: Some("makibad"),
            payload: Some(&json!({"range_id": b, "complete": true, "through": 0})),
            ..Default::default()
        })
        .unwrap();
    Seeded { a, b, read }
}

/// A range's `status`, `approval_mode`, `approved_read_id` and `ended_reason`.
fn row(db: &Path, id: i64) -> (String, Option<String>, Option<i64>, Option<String>) {
    let c = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    c.query_row(
        "SELECT status, approval_mode, approved_read_id, ended_reason FROM ranges WHERE id = ?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )
    .unwrap()
}

/// The payload of each `range_approved` event of range `id`, oldest first.
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
        .map(|p| serde_json::from_str::<Value>(&p.unwrap()).unwrap())
        .collect()
}

/// Range `id` of a status object's `ranges[]`.
fn range_of(status: &Value, id: i64) -> Value {
    status["ranges"]
        .as_array()
        .unwrap_or_else(|| panic!("status has no ranges[]: {status}"))
        .iter()
        .find(|r| r["id"] == json!(id))
        .cloned()
        .unwrap_or_else(|| panic!("range {id} not listed: {status}"))
}

/// 1. In this process: shown under `human`, the approval is refused once the mode is
///    `acceptance`; shown and approved under `acceptance`, it records `acceptance`.
#[test]
fn in_process_the_mode_shown_binds_the_approval() {
    let _env = ENV.lock().unwrap_or_else(PoisonError::into_inner);
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("state.db");
    let w = StateWriter::open(&db).unwrap();
    let s = seed(&w);
    let person = issue_key(&w, Role::Person, "person", None, None).unwrap();
    let door = Door::new(w, Arc::new(Stub::new()), "fake".into());
    let approve = |args: Value| door.handle(Some(&person), "range", args, None, true);

    // SAFETY: under `ENV`; no other test of this file runs or spawns while it is held.
    unsafe { std::env::set_var(MODE_ENV, "human") };
    let status = door
        .handle(Some(&person), "status", json!({}), None, true)
        .unwrap_or_else(|e| panic!("status refused: {e:?}"));
    let b = range_of(&status, s.b);
    assert_eq!(b["approves_under"], json!("human"), "{b}");
    assert_eq!(b["replaces"], json!(s.a), "{b}");
    let a = range_of(&status, s.a);
    assert_eq!(a["approves_under"], Value::Null, "{a}");
    assert_eq!(a["replaces"], Value::Null, "{a}");

    // SAFETY: as above.
    unsafe { std::env::set_var(MODE_ENV, "acceptance") };
    let r =
        approve(json!({"action": "approve", "id": s.b, "read": s.read, "approval_mode": "human"}));
    let e = r.expect_err("an approval shown under human passed under acceptance");
    assert_eq!(e.kind, ErrorKind::Stale, "{e:?}");
    assert!(
        e.message.contains("human") && e.message.contains("acceptance"),
        "the refusal does not name both modes: {e:?}"
    );
    assert_eq!(row(&db, s.b), ("proposed".into(), None, None, None));
    assert!(approvals(&db, s.b).is_empty(), "an approval was recorded");
    assert_eq!(row(&db, s.a).0, "active");

    for args in [
        json!({"action": "approve", "id": s.b, "read": s.read}),
        json!({"action": "approve", "id": s.b, "read": s.read, "approval_mode": "Human"}),
    ] {
        let e = approve(args.clone()).expect_err("an approval with no mode shown passed");
        assert_eq!(e.kind, ErrorKind::Usage, "{args}: {e:?}");
    }
    assert_eq!(row(&db, s.b), ("proposed".into(), None, None, None));
    assert!(approvals(&db, s.b).is_empty(), "an approval was recorded");

    let r = approve(
        json!({"action": "approve", "id": s.b, "read": s.read, "approval_mode": "acceptance"}),
    )
    .unwrap_or_else(|e| panic!("the approval under the mode shown was refused: {e:?}"));
    // SAFETY: as above.
    unsafe { std::env::remove_var(MODE_ENV) };
    assert_eq!(r["status"], json!("active"), "{r}");
    assert_eq!(r["approval_mode"], json!("acceptance"), "{r}");
    assert_eq!(
        row(&db, s.b),
        (
            "active".into(),
            Some("acceptance".into()),
            Some(s.read),
            None
        )
    );
    let events = approvals(&db, s.b);
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(
        events[0]["approval_mode"],
        json!("acceptance"),
        "{events:?}"
    );
    let a = row(&db, s.a);
    assert_eq!((a.0.as_str(), a.3.as_deref()), ("ended", Some("replaced")));
}

// ---- the real binary ---------------------------------------------------------------------------

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// One organization's folders and state, seeded; `makibad` started on it with `start`.
struct Org {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    port: u16,
    s: Seeded,
}

/// A running `makiba daemon`, stopped when dropped.
struct Daemon(Child);

impl Org {
    fn new() -> Org {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        for d in ["home", "state", "config", "data", "run", "xdg-state"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        // not an executable: the socket folder's mode, owner only
        std::fs::set_permissions(root.join("run"), std::fs::Permissions::from_mode(0o700)).unwrap();
        // A `gh` that fails: a read of a range records a failed read and goes nowhere.
        common::exe::write(
            &root.join("gh"),
            "#!/bin/sh\necho \"no gh in this test\" >&2\nexit 1\n",
        );
        let w = StateWriter::open(&root.join("state/state.db")).unwrap();
        let s = seed(&w);
        drop(w);
        Org {
            _tmp: tmp,
            root,
            port: free_port(),
            s,
        }
    }

    /// The environment of `makibad` and of the person's `makiba`, with the merge approval mode
    /// given: `makibad`'s, or the person's shell's.
    fn env(&self, c: &mut Command, mode: &str) {
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
            .env("MAKIBA_GH", root.join("gh"))
            .env(MODE_ENV, mode);
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

    fn db_path(&self) -> PathBuf {
        self.root.join("state/state.db")
    }

    /// Starts `makibad` under `mode` and waits until its door is open and the person's status
    /// reads `mode` as `makibad`'s, recorded by this start.
    fn start(&mut self, mode: &str) -> Daemon {
        self.port = free_port();
        let mut c = makibad::daemon(&self.root);
        c.with(|c| self.env(c, mode));
        let d = Daemon(
            c.stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let door = self.root.join("run/makiba/door.sock");
        let until = Instant::now() + Duration::from_secs(20);
        while Instant::now() < until {
            if UnixStream::connect(&door).is_ok()
                && TcpStream::connect(("127.0.0.1", self.port)).is_ok()
                && self.status()["merge_approval_mode"] == json!(mode)
            {
                return d;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("makibad under {mode} did not open its door and record its mode");
    }

    /// The person's `makiba`, its shell under `human`, with its input closed.
    fn cli(&self, args: &[&str]) -> Output {
        let mut c = Command::new(MAKIBA);
        self.env(&mut c, "human");
        c.args(args).stdin(Stdio::null()).output().unwrap()
    }

    /// The person's `makiba status --json`; null when it does not answer an object.
    fn status(&self) -> Value {
        let out = self.cli(&["status", "--json"]);
        serde_json::from_slice(&out.stdout).unwrap_or(Value::Null)
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = Command::new("kill")
            .args(["-TERM", &self.0.id().to_string()])
            .status();
        let until = Instant::now() + Duration::from_secs(5);
        while Instant::now() < until {
            if let Ok(Some(_)) = self.0.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
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

/// 2. The person reads the proposal while `makibad` runs under `human`; `makibad` is restarted
///    under `acceptance`; the approval naming the mode shown is `stale` (exit 7) and approves
///    nothing. Shown again and approved under `acceptance`, the range records `acceptance`.
#[test]
fn shown_under_human_refused_after_a_restart_under_acceptance() {
    let _env = ENV.lock().unwrap_or_else(PoisonError::into_inner);
    let mut org = Org::new();
    let (db, b, read) = (org.db_path(), org.s.b, org.s.read);
    let id = b.to_string();

    let d = org.start("human");
    let shown = range_of(&org.status(), b);
    assert_eq!(shown["approves_under"], json!("human"), "{shown}");
    assert_eq!(shown["replaces"], json!(org.s.a), "{shown}");
    drop(d);

    let _d = org.start("acceptance");
    let out = org.cli(&[
        "range",
        "approve",
        &id,
        "--read",
        &read.to_string(),
        "--mode",
        "human",
        "--yes",
    ]);
    assert_eq!(out.status.code(), Some(7), "{}", text(&out));
    let stderr = String::from_utf8_lossy(&out.stderr);
    for word in ["stale", "human", "acceptance"] {
        assert!(stderr.contains(word), "no {word:?}: {}", text(&out));
    }
    assert_eq!(row(&db, b).0, "proposed");
    assert!(approvals(&db, b).is_empty(), "an approval was recorded");

    let out = org.cli(&["range", "approve", &id, "--yes"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stdout)
            .contains("Merge approval mode: acceptance (makibad's)"),
        "the preview does not show makibad's mode: {}",
        text(&out)
    );
    let r = row(&db, b);
    assert_eq!(
        (r.0.as_str(), r.1.as_deref()),
        ("active", Some("acceptance"))
    );
    let events = approvals(&db, b);
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(
        events[0]["approval_mode"],
        json!("acceptance"),
        "{events:?}"
    );
}

/// 3. Read and approved while `makibad` runs under `human`: the range records `human`.
#[test]
fn read_and_approved_under_human_records_human() {
    let _env = ENV.lock().unwrap_or_else(PoisonError::into_inner);
    let mut org = Org::new();
    let (db, b, read) = (org.db_path(), org.s.b, org.s.read);
    let _d = org.start("human");
    let out = org.cli(&[
        "range",
        "approve",
        &b.to_string(),
        "--read",
        &read.to_string(),
        "--mode",
        "human",
        "--yes",
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let r = row(&db, b);
    assert_eq!(
        (r.0.as_str(), r.1.as_deref(), r.2),
        ("active", Some("human"), Some(read))
    );
    let events = approvals(&db, b);
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["approval_mode"], json!("human"), "{events:?}");
}
