//! M3-1: `makibad` takes `queued` runs, oldest `dispatched_at` first, while the runs `in_use`
//! plus the launches not yet `verified` or `failed` are fewer than `max_concurrent_workers`
//! (issue M3-1; the organization setting is read from `MAKIBA_MAX_CONCURRENT_WORKERS` until
//! `config` exists, default 4).
//!
//! The tests run the real binary (`makiba daemon` on the stand-in base). Nothing here launches a
//! terminal (M3-2 to M3-6 are other Issues): a run is taken when `makibad` records one
//! `run_taken` event for it, whose payload names the `task_id` and the `spawn_id` of the
//! `run_launch` row the later steps will carry on from. The seeded launch rows stay `planned`,
//! so a taken run keeps counting as a launch that is not yet `verified` or `failed`.

use makiba::db::writer::{RunUpsert, StateWriter};
use rusqlite::{Connection, params};
use serde_json::Value;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const MAKIBA: &str = env!("CARGO_BIN_EXE_makiba");

/// A `makibad` that is killed when the test ends, so a failed test leaves nothing running.
struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Org {
    dir: tempfile::TempDir,
    conn: Connection,
}

impl Org {
    /// A state folder whose database already holds the schema.
    fn new() -> Org {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        drop(StateWriter::open(&path).unwrap());
        let conn = Connection::open(&path).unwrap();
        conn.busy_timeout(Duration::from_secs(5)).unwrap();
        Org { dir, conn }
    }

    /// A run with its first launch row, `planned` as the door leaves it (spec section 5.2).
    fn run(&self, task: &str, status: &str, dispatched_at: &str, stage: &str) {
        let w = StateWriter::open(&self.dir.path().join("state.db")).unwrap();
        w.upsert_run(&RunUpsert {
            task_id: task,
            project_slug: "p",
            status: Some(status),
            ..Default::default()
        })
        .unwrap();
        drop(w);
        self.conn
            .execute(
                "UPDATE runs SET dispatched_at = ?2 WHERE task_id = ?1",
                params![task, dispatched_at],
            )
            .unwrap();
        self.conn
            .execute(
                "INSERT INTO run_launch (run_id, spawn_id, agent, brief, worktree, stage)
                 VALUES ((SELECT id FROM runs WHERE task_id = ?1), ?2, 'fake', 'b', '/w', ?3)",
                params![task, format!("spawn-{task}"), stage],
            )
            .unwrap();
    }

    /// The launch and the run of `task` end: nothing is waiting on a terminal for it any more.
    fn finish(&self, task: &str) {
        self.conn
            .execute(
                "UPDATE run_launch SET stage = 'failed', failed_reason = 'test'
                 WHERE run_id = (SELECT id FROM runs WHERE task_id = ?1)",
                [task],
            )
            .unwrap();
        self.conn
            .execute(
                "UPDATE runs SET status = 'failed' WHERE task_id = ?1",
                [task],
            )
            .unwrap();
    }

    /// The task ids of the `run_taken` events, in the order they were written.
    fn taken(&self) -> Vec<String> {
        let mut st = self
            .conn
            .prepare("SELECT payload_json FROM events WHERE kind = 'run_taken' ORDER BY id")
            .unwrap();
        st.query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|p| {
                let v: Value = serde_json::from_str(&p.unwrap()).unwrap();
                v["task_id"].as_str().unwrap_or_default().to_string()
            })
            .collect()
    }

    fn start(&self, capacity: Option<&str>) -> Daemon {
        let mut c = Command::new(MAKIBA);
        c.arg("daemon")
            .env("MAKIBA_STATE_DIR", self.dir.path())
            .env("MAKIBA_BASE", "stub")
            .env_remove("MAKIBA_MAX_CONCURRENT_WORKERS")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(n) = capacity {
            c.env("MAKIBA_MAX_CONCURRENT_WORKERS", n);
        }
        Daemon(c.spawn().unwrap())
    }

    /// What was taken once `n` runs were, or after `within` when fewer were.
    fn taken_when(&self, n: usize, within: Duration) -> Vec<String> {
        let until = Instant::now() + within;
        loop {
            let t = self.taken();
            if t.len() >= n || Instant::now() >= until {
                return t;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Several passes of `makibad` go by with nothing more taken than `already`.
    fn still_taken(&self, already: usize) -> Vec<String> {
        std::thread::sleep(Duration::from_millis(1500));
        let t = self.taken();
        assert!(t.len() >= already, "a taken run went missing: {t:?}");
        t
    }
}

const WAIT: Duration = Duration::from_secs(15);

fn ids(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// The runs are written in an order that is neither their time order nor their id order.
fn three_queued(org: &Org) {
    org.run("t-late", "queued", "2026-11-18T10:00:30.000Z", "planned");
    org.run("t-early", "queued", "2026-11-18T10:00:10.000Z", "planned");
    org.run("t-mid", "queued", "2026-11-18T10:00:20.000Z", "planned");
}

/// Issue M3-1: "With capacity 2 and three queued runs, two launches start and the third waits."
/// Issue M3-1: "Runs are taken in `dispatched_at` order." (the two oldest, the newest waits).
#[test]
fn capacity_two_takes_the_two_oldest_and_the_third_waits() {
    let org = Org::new();
    three_queued(&org);
    let _d = org.start(Some("2"));

    let taken = org.taken_when(2, WAIT);
    assert_eq!(taken, ids(&["t-early", "t-mid"]));
    assert_eq!(org.still_taken(2), ids(&["t-early", "t-mid"]));
}

/// Issue M3-1: "When one finishes, the third starts on the next pass."
#[test]
fn when_one_finishes_the_third_is_taken() {
    let org = Org::new();
    three_queued(&org);
    let _d = org.start(Some("2"));
    assert_eq!(org.taken_when(2, WAIT), ids(&["t-early", "t-mid"]));
    assert_eq!(org.still_taken(2), ids(&["t-early", "t-mid"]));

    org.finish("t-early");
    assert_eq!(
        org.taken_when(3, WAIT),
        ids(&["t-early", "t-mid", "t-late"])
    );
}

/// Issue M3-1: "`max_concurrent_workers` (organization setting, default 4)": with no setting,
/// five queued runs give four launches and the fifth waits.
#[test]
fn the_default_capacity_is_four() {
    let org = Org::new();
    for (i, t) in ["t1", "t2", "t3", "t4", "t5"].iter().enumerate() {
        org.run(
            t,
            "queued",
            &format!("2026-11-18T10:00:0{i}.000Z"),
            "planned",
        );
    }
    let _d = org.start(None);

    assert_eq!(org.taken_when(4, WAIT), ids(&["t1", "t2", "t3", "t4"]));
    assert_eq!(org.still_taken(4), ids(&["t1", "t2", "t3", "t4"]));
}
