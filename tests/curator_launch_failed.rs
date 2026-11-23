//! Issue 333: a curator launch that failed is ended at once through the curator's settle (its key
//! revoked, its own notice after the end), instead of living until the curator's time limit; and
//! the failed-launch notice says what was seen, the terminal's name and address, and what ends it.
//!
//! The daemon runs in this process on `Local` with the test agent, as tests/curator.rs.

mod common;

use common::local::Local;
use common::makibad;
use makiba::base::{Base, Terminal};
use makiba::db::writer::{NewEvent, RunUpsert, StateWriter};
use rusqlite::{Connection, OpenFlags};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const AGENT: &str = env!("CARGO_BIN_EXE_makiba-fake-agent");
const VERIFY_MS: u64 = 2000;
/// Far longer than the test waits: an end seen here is not the time limit's.
const LIMIT_MS: u64 = 120_000;
const WAIT: Duration = Duration::from_secs(40);

struct Org {
    dir: PathBuf,
    base: Local,
    stop: Arc<AtomicBool>,
    daemon: Option<JoinHandle<Result<(), String>>>,
    _held: makibad::Knowledge,
    _tmp: tempfile::TempDir,
}

/// The daemon is stopped and every terminal ended when the test ends, passed or failed.
impl Drop for Org {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(d) = self.daemon.take() {
            let _ = d.join();
        }
        for t in self.base.list().unwrap_or_default() {
            let _ = self.base.end(&t.id);
        }
    }
}

impl Org {
    fn curators(&self) -> Vec<Terminal> {
        self.base
            .list()
            .unwrap()
            .into_iter()
            .filter(|t| t.tags.get("makiba.role").map(String::as_str) == Some("curator"))
            .collect()
    }

    fn count(&self, sql: &str) -> i64 {
        self.db().query_row(sql, [], |r| r.get(0)).unwrap()
    }

    fn db(&self) -> Connection {
        db(&self.dir)
    }
}

fn db(dir: &Path) -> Connection {
    Connection::open_with_flags(dir.join("state.db"), OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
}

fn until(what: &str, f: impl Fn() -> bool) {
    let end = Instant::now() + WAIT;
    while Instant::now() < end {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("timed out waiting for: {what}");
}

/// A daemon over five raw notes whose curator never reports a status, and one close recorded.
fn org() -> Org {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let (dir, knowledge) = (root.join("state"), root.join("knowledge"));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(knowledge.join("raw")).unwrap();
    for i in 0..5 {
        std::fs::write(knowledge.join(format!("raw/n{i}.md")), "a note").unwrap();
    }
    drop(StateWriter::open(&dir.join("state.db")).unwrap());
    let held = makibad::use_knowledge(&knowledge);
    // SAFETY: this file holds one test, so nothing else in this process reads them meanwhile.
    unsafe {
        std::env::set_var(
            "MAKIBA_FAKE_AGENT",
            json!([AGENT, "record", "hang"]).to_string(),
        );
        std::env::set_var("MAKIBA_LAUNCH_AGENT", "fake");
        std::env::set_var("MAKIBA_CURATOR_TIMEOUT_MS", LIMIT_MS.to_string());
        std::env::set_var("MAKIBA_VERIFY_TIMEOUT_MS", VERIFY_MS.to_string());
    }
    let base = Local::new();
    let stop = Arc::new(AtomicBool::new(false));
    let daemon = {
        let (dir, base, stop) = (dir.clone(), base.clone(), Arc::clone(&stop));
        std::thread::spawn(move || makibad::run_until(&dir, &|| Box::new(base.clone()), stop))
    };
    // A close, as the door writes it; the daemon writes too, so a busy database is tried again.
    let close = || -> Result<(), makiba::db::Error> {
        let w = StateWriter::open(&dir.join("state.db"))?;
        w.transaction(|w| {
            w.upsert_run(&RunUpsert {
                task_id: "t1",
                project_slug: "p",
                status: Some("completed"),
                ..Default::default()
            })?;
            w.append_event(&NewEvent {
                kind: "run_closed",
                actor: Some("person"),
                payload: Some(&json!({"task_id": "t1"})),
                run_task_id: Some("t1"),
                ..Default::default()
            })
            .map(|_| ())
        })
    };
    until("a close recorded", || close().is_ok());
    Org {
        dir,
        base,
        stop,
        daemon: Some(daemon),
        _held: held,
        _tmp: tmp,
    }
}

/// The id and body of the one notice whose body starts with `start`.
fn notice(org: &Org, start: &str) -> (i64, String) {
    org.db()
        .query_row(
            "SELECT id, body FROM messages WHERE kind = 'notice' AND substr(body, 1, length(?1)) = ?1",
            [start],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
}

#[test]
fn a_failed_curator_launch_is_ended_at_once_and_the_notice_says_what_was_seen() {
    let org = org();
    until("a curator terminal", || org.curators().len() == 1);
    let first = org.curators().remove(0);
    until("the launch failed", || {
        org.count("SELECT count(*) FROM run_launch WHERE role = 'curator' AND stage = 'failed'")
            == 1
    });

    // Ended at once, well before its time limit; no key left.
    until("the curator ended", || org.curators().is_empty());
    until("the end notice", || {
        org.count(
            "SELECT count(*) FROM messages WHERE kind = 'notice'
             AND body LIKE 'The curator whose launch failed was ended%'",
        ) == 1
    });
    assert_eq!(
        org.count("SELECT count(*) FROM agents WHERE role = 'curator' AND revoked_at IS NULL"),
        0
    );
    assert_eq!(
        org.count(
            "SELECT count(*) FROM messages WHERE kind = 'notice'
             AND body LIKE 'The curator was ended after its time limit%'"
        ),
        0
    );

    // The failed-launch notice: what was seen, the terminal, what ends it; the end's after it.
    let (failed_id, failed) = notice(&org, "Launch of curator");
    assert_eq!(
        failed,
        format!(
            "Launch of curator failed at stage launched: no working status or turn of its \
             terminal within {VERIFY_MS} ms of the launch. Seen: never any status. Its terminal: \
             curator at {}. It is ended at once, its key revoked.",
            first.id.0
        )
    );
    let (ended_id, _) = notice(&org, "The curator whose launch failed");
    assert!(ended_id > failed_id);
    let payload: String = org
        .db()
        .query_row(
            "SELECT payload_json FROM events WHERE kind = 'worker_spawn_failed'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let payload: serde_json::Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(payload["seen"], "no_status");
    assert_eq!(
        payload["terminals"],
        json!([{"name": "curator", "address": first.id.0}])
    );
    // One launch only: no second curator after the failed one ended.
    assert_eq!(
        org.count("SELECT count(*) FROM run_launch WHERE role = 'curator'"),
        1
    );
}
