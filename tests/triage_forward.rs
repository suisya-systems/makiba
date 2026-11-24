//! M5-5 (Issue 40): after a close, and at most once per interval, `makibad` runs the work
//! discovery scan and forwards a changed result to the secretary.
//!
//! The daemon runs in this process on the stand-in base. A close is the `run_closed` event the
//! door writes, appended here directly. `gh` is a stand-in script (`MAKIBA_GH`) that answers
//! `issue list` / `pr list` for `--repo github.com/<owner>/<name>` from files in a folder named by
//! the repository and refuses any other call, so no test reaches the network. The interval is cut
//! to `INTERVAL` (`MAKIBA_TRIAGE_INTERVAL_MS`).

mod common;

use common::makibad;
use makiba::base::Base;
use makiba::base::stub::Stub;
use makiba::db::writer::{NewEvent, RunUpsert, StateWriter};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Once, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const INTERVAL: Duration = Duration::from_millis(1500);

const STAND_IN: &str = r#"#!/bin/sh
# gh <issue|pr> list --repo github.com/<owner>/<name> --state <s> [...]: the file of that read.
what="$1 $2"; shift 2
case "$what" in "issue list"|"pr list") ;; *) echo "unexpected call: $what" >&2; exit 2 ;; esac
repo=; state=
while [ $# -gt 0 ]; do
  case "$1" in
    --repo) repo=$2; shift 2 ;;
    --state) state=$2; shift 2 ;;
    --limit|--json|--search|--base) shift 2 ;;
    *) echo "unexpected argument: $1" >&2; exit 2 ;;
  esac
done
case "$repo" in github.com/*/*) ;; *) echo "unexpected repo: $repo" >&2; exit 2 ;; esac
r=${repo#github.com/}
dir="$FAKE_GH_DIR/${r%%/*}_${r#*/}"
mkdir -p "$dir"
echo "$what $state" >> "$dir/calls"
f="$dir/${what%% *}-$state.json"
if [ -e "$f" ]; then cat "$f"; else echo '[]'; fi
"#;

static HOME: OnceLock<tempfile::TempDir> = OnceLock::new();
static INIT: Once = Once::new();

/// The shared folder: `gh` stand-in and its answers, the goal ledgers, the repositories.
fn init() -> PathBuf {
    INIT.call_once(|| {
        let home = HOME.get_or_init(|| tempfile::tempdir().unwrap());
        let script = home.path().join("gh");
        common::exe::write(&script, STAND_IN);
        for d in ["answers", "goals", "repos"] {
            std::fs::create_dir_all(home.path().join(d)).unwrap();
        }
        // SAFETY: before this call returns no test of this file has started a thread of its own.
        unsafe {
            std::env::set_var("MAKIBA_GH", &script);
            std::env::set_var("FAKE_GH_DIR", home.path().join("answers"));
            std::env::set_var("MAKIBA_GOALS_DIR", home.path().join("goals"));
            std::env::set_var("MAKIBA_REPOS_DIR", home.path().join("repos"));
            std::env::set_var(
                "MAKIBA_TRIAGE_INTERVAL_MS",
                INTERVAL.as_millis().to_string(),
            );
        }
    });
    HOME.get().unwrap().path().to_path_buf()
}

/// How the project's GitHub repository is found.
enum Origin {
    /// `projects.origin_url`.
    Url,
    /// No URL: the `origin` remote of the project's repository under `MAKIBA_REPOS_DIR`.
    GitRemote,
}

/// One organization with one project `<name>`, whose repository is `o/<name>`, and a run `t1`.
struct Org {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    w: StateWriter,
}

impl Org {
    fn new(name: &str, origin: Origin, ledger: bool, issues: &[Value]) -> Org {
        let home = init();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("state");
        std::fs::create_dir_all(&dir).unwrap();
        let w = StateWriter::open(&dir.join("state.db")).unwrap();
        let repo = format!("o/{name}");
        match origin {
            Origin::Url => {
                w.ensure_project(name, None, Some(&format!("https://github.com/{repo}")))
                    .unwrap();
            }
            Origin::GitRemote => {
                let r = home.join("repos").join(name);
                std::fs::create_dir_all(&r).unwrap();
                git(&r, &["init", "-q"]);
                git(
                    &r,
                    &[
                        "remote",
                        "add",
                        "origin",
                        &format!("git@github.com:{repo}.git"),
                    ],
                );
            }
        }
        w.upsert_run(&RunUpsert {
            task_id: "t1",
            project_slug: name,
            title: Some("T"),
            status: Some("in_use"),
            ..Default::default()
        })
        .unwrap();
        if ledger {
            let g = home.join("goals/o");
            std::fs::create_dir_all(&g).unwrap();
            std::fs::write(
                g.join(format!("{name}.md")),
                "# Goals\n\n## G1 It runs its own development\n- unmet if: nothing is proposed\n\n\
                 ## G2 It is quiet\n- unmet if: it is loud\n",
            )
            .unwrap();
        }
        let answers = home.join("answers").join(format!("o_{name}"));
        std::fs::create_dir_all(&answers).unwrap();
        std::fs::write(
            answers.join("issue-open.json"),
            serde_json::to_string(issues).unwrap(),
        )
        .unwrap();
        Org { _tmp: tmp, dir, w }
    }

    /// A close, as the door writes it. Returns the event's id.
    fn close(&self) -> i64 {
        self.w
            .append_event(&NewEvent {
                kind: "run_closed",
                actor: Some("secretary"),
                payload: Some(&json!({"task_id": "t1"})),
                run_task_id: Some("t1"),
                ..Default::default()
            })
            .unwrap()
    }

    fn db(&self) -> Connection {
        Connection::open(self.dir.join("state.db")).unwrap()
    }

    fn scans(&self) -> Vec<Value> {
        let db = self.db();
        let mut stmt = db
            .prepare(
                "SELECT payload_json FROM events WHERE kind = 'work_discovery_scanned' ORDER BY id",
            )
            .unwrap();
        stmt.query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|t| serde_json::from_str(&t.unwrap()).unwrap())
            .collect()
    }

    /// The bodies of the notices to the secretary that carry no task and wait for delivery.
    fn notices(&self) -> Vec<String> {
        let db = self.db();
        let mut stmt = db
            .prepare(
                "SELECT m.body FROM messages m
                 JOIN events e ON e.id = m.event_id AND e.kind = 'message_sent'
                 JOIN event_deliveries d ON d.source_event_id = m.event_id
                   AND d.recipient = 'secretary'
                 WHERE m.kind = 'notice' AND m.sender = 'makibad'
                   AND m.recipient = 'secretary' AND m.run_id IS NULL
                 ORDER BY m.id",
            )
            .unwrap();
        stmt.query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    fn start(&self) -> Daemon {
        let d = Daemon::start(&self.dir);
        let db = self.db();
        wait("makibad to start", || {
            db.query_row(
                "SELECT COUNT(*) FROM events WHERE kind = 'dispatcher_started'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
                > 0
        });
        d
    }
}

fn git(dir: &Path, args: &[&str]) {
    let s = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .status()
        .unwrap();
    assert!(s.success(), "git {args:?}");
}

/// `makibad` in this process, stopped when dropped.
struct Daemon {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<(), String>>>,
}

impl Daemon {
    fn start(dir: &Path) -> Daemon {
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (dir, stop) = (dir.to_path_buf(), Arc::clone(&stop));
            std::thread::spawn(move || {
                makibad::run_until(&dir, &|| Box::new(Stub::new()) as Box<dyn Base>, stop)
            })
        };
        Daemon {
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn wait(what: &str, mut ok: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(20);
    while Instant::now() < until {
        if ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("timed out waiting for {what}");
}

fn issue(number: i64, title: &str, body: &str) -> Value {
    json!({"number": number, "title": title, "body": body, "labels": [],
           "updatedAt": format!("2026-11-{:02}T10:00:00Z", 10 + number), "createdAt": "2026-11-01T10:00:00Z",
           "milestone": null, "comments": []})
}

/// Definition of done 1: two scans with the same candidates queue one notice.
#[test]
fn two_scans_with_the_same_candidates_queue_one_notice() {
    let org = Org::new(
        "same",
        Origin::Url,
        true,
        &[
            issue(
                41,
                "Propose the next task",
                "Do it.\n\n## Goal clause\n\n- G1 It runs its own development\n",
            ),
            issue(42, "Tidy the logs", "Make them shorter."),
        ],
    );
    let _d = org.start();
    org.close();
    wait("the first scan", || org.scans().len() == 1);
    let first = &org.scans()[0];
    assert_eq!(first["trigger"], json!("close"), "{first}");
    assert_eq!(first["status"], json!("candidates_found"), "{first}");
    assert_eq!(
        first["candidate_refs"],
        json!("o/same#41,o/same#42"),
        "{first}"
    );
    assert_eq!(first["notified"], json!(true), "{first}");
    let notices = org.notices();
    assert_eq!(notices.len(), 1, "{notices:?}");
    let body = &notices[0];
    let (a, b) = (
        body.find("o/same#41").expect(body),
        body.find("o/same#42").expect(body),
    );
    assert!(a < b, "the Issue naming G1 comes first: {body}");
    assert!(body.contains("G1 It runs its own development"), "{body}");
    assert!(body.contains("names no clause"), "{body}");

    // The same answers after the interval: a second scan, no second notice.
    org.close();
    wait("the second scan", || org.scans().len() == 2);
    let second = &org.scans()[1];
    assert_eq!(
        second["candidate_refs"], first["candidate_refs"],
        "{second}"
    );
    assert_eq!(second["notified"], json!(false), "{second}");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(org.notices().len(), 1, "{:?}", org.notices());
}

/// Definition of done 2: a project with no goal ledger is reported as such, once. Its repository
/// is found through the `origin` remote of its clone, as for every project no URL was given for.
#[test]
fn a_project_with_no_goal_ledger_is_reported_once() {
    let org = Org::new(
        "noledger",
        Origin::GitRemote,
        false,
        &[issue(7, "Something", "Body.")],
    );
    let _d = org.start();
    org.close();
    wait("the first scan", || org.scans().len() == 1);
    let first = &org.scans()[0];
    assert_eq!(first["status"], json!("no_candidates"), "{first}");
    assert_eq!(first["diagnostics"], json!("unset:o/noledger"), "{first}");
    assert_eq!(first["notified"], json!(true), "{first}");
    let notices = org.notices();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(
        notices[0].contains("No goal ledger: o/noledger"),
        "{}",
        notices[0]
    );

    org.close();
    wait("the second scan", || org.scans().len() == 2);
    assert_eq!(org.scans()[1]["notified"], json!(false));
    std::thread::sleep(Duration::from_millis(300));
    let notices = org.notices();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(
        notices
            .iter()
            .filter(|n| n.contains("No goal ledger: o/noledger"))
            .count(),
        1
    );
}

/// A second close inside the interval runs no scan until the interval has passed; then the scan
/// it was deferred to covers it.
#[test]
fn a_close_inside_the_interval_waits_for_its_end() {
    let org = Org::new("interval", Origin::Url, true, &[issue(3, "One", "Body.")]);
    let _d = org.start();
    org.close();
    wait("the first scan", || org.scans().len() == 1);
    let started = Instant::now();
    let second = org.close();
    std::thread::sleep(Duration::from_millis(500));
    assert!(started.elapsed() < INTERVAL);
    assert_eq!(org.scans().len(), 1, "a scan ran inside the interval");
    wait("the deferred scan", || org.scans().len() == 2);
    assert_eq!(org.scans()[1]["through_close_id"], json!(second));
    // Nothing more is due.
    std::thread::sleep(INTERVAL + Duration::from_millis(300));
    assert_eq!(org.scans().len(), 2);
}
