//! M5-6 (Issue 41): `makiba down` and `makiba up` keep the work.
//!
//! The daemon runs in this process on `Local` (as tests/spawn_verified.rs), stopped and started
//! again by a flag (`daemon::run_until`). The test agent is given by `MAKIBA_FAKE_AGENT`; its
//! module reads the conversation id from `.fake-conversation` in the workplace, and a resume runs
//! the steps file named by that id before its script, so an id with no such file is a resume that
//! fails (the agent exits at once and its launch is never verified).

mod common;

use common::local::Local;
use common::makibad;
use makiba::base::stub::Stub;
use makiba::base::{Base, LaunchSpec, Terminal};
use makiba::db::writer::{RunUpsert, StateWriter};
use rusqlite::{Connection, OpenFlags};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const AGENT: &str = env!("CARGO_BIN_EXE_makiba-fake-agent");
const TIMEOUT_MS: u64 = 3000;

fn setup() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let agent = json!([AGENT, "record", "env", "working", "hang"]).to_string();
        // SAFETY: every test calls this before it starts a thread; the first sets them, once.
        unsafe {
            std::env::set_var("MAKIBA_FAKE_AGENT", agent);
            std::env::set_var("MAKIBA_VERIFY_TIMEOUT_MS", TIMEOUT_MS.to_string());
        }
    });
}

fn db(dir: &Path) -> Connection {
    Connection::open_with_flags(dir.join("state.db"), OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
}

fn status(dir: &Path, task: &str) -> String {
    db(dir)
        .query_row("SELECT status FROM runs WHERE task_id = ?1", [task], |r| {
            r.get(0)
        })
        .unwrap()
}

/// (stage, conversation_id, brief) of every launch of `task`, oldest first.
fn launches(dir: &Path, task: &str) -> Vec<(String, Option<String>, String)> {
    let conn = db(dir);
    let mut q = conn
        .prepare(
            "SELECT l.stage, l.conversation_id, l.brief FROM run_launch l
             JOIN runs r ON r.id = l.run_id WHERE r.task_id = ?1 ORDER BY l.id",
        )
        .unwrap();
    q.query_map([task], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// Poll until `f` holds, for up to `secs` seconds.
fn wait_for(secs: u64, mut f: impl FnMut() -> bool) -> bool {
    let until = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < until {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// The terminals of `local` carrying `makiba.task_id = task`.
fn terminals_of(local: &Local, task: &str) -> Vec<Terminal> {
    local
        .list()
        .unwrap()
        .into_iter()
        .filter(|t| t.tags.get("makiba.task_id").map(String::as_str) == Some(task))
        .collect()
}

/// Ends every terminal of the base when the test ends, passed or failed: the daemon's event thread
/// keeps a clone of `Local`, so its terminals would outlive the test otherwise.
struct EndAll(Local);

impl Drop for EndAll {
    fn drop(&mut self) {
        for t in self.0.list().unwrap_or_default() {
            let _ = self.0.end(&t.id);
        }
    }
}

struct Daemon {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<Result<(), String>>,
}

fn start(dir: &Path, local: &Local) -> Daemon {
    let stop = Arc::new(AtomicBool::new(false));
    let (dir, local, flag) = (dir.to_path_buf(), local.clone(), Arc::clone(&stop));
    let thread =
        std::thread::spawn(move || makibad::run_until(&dir, &|| Box::new(local.clone()), flag));
    Daemon { stop, thread }
}

impl Daemon {
    fn stop(self) {
        self.stop.store(true, Ordering::SeqCst);
        self.thread.join().unwrap().unwrap();
    }
}

/// A queued document run `task` whose launch waits at `settings_ready`, in its own workplace,
/// whose agent says its conversation is `conversation`.
fn queue(w: &StateWriter, root: &Path, task: &str, conversation: &str) -> PathBuf {
    let work = root.join(task);
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join(".fake-conversation"), conversation).unwrap();
    w.upsert_run(&RunUpsert {
        task_id: task,
        project_slug: "p",
        status: Some("queued"),
        ..Default::default()
    })
    .unwrap();
    w.conn()
        .execute(
            "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree, stage)
             VALUES ((SELECT id FROM runs WHERE task_id = ?1), 'spawn-' || ?1, 'document',
                     'fake', 'the brief of ' || ?1, ?2, 'settings_ready')",
            [task.to_string(), work.to_string_lossy().into_owned()],
        )
        .unwrap();
    work
}

fn spec(name: &str, tags: &[(&str, &str)]) -> LaunchSpec {
    LaunchSpec {
        argv: vec!["sleep".into(), "60".into()],
        cwd: "/".into(),
        name: name.into(),
        tags: tags
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<BTreeMap<_, _>>(),
        env: BTreeMap::new(),
        unset: vec![],
    }
}

/// After `down` then `up`, both runs that were `in_use` are `in_use` again with one terminal: `a`
/// resumed by its conversation id, `b`, whose resume fails, by a fresh launch with its original
/// brief in its worktree as it is. Terminals without this organization's tag are not ended.
#[test]
fn down_then_up_brings_every_run_back_resumed_or_fresh() {
    setup();
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let dir = root.join("state");
    std::fs::create_dir_all(&dir).unwrap();
    let org = dir.to_string_lossy().into_owned();
    let (work_a, work_b) = {
        let w = StateWriter::open(&dir.join("state.db")).unwrap();
        let a = queue(&w, &root, "a", "conv-a");
        let b = queue(&w, &root, "b", "conv-b");
        (a, b)
    };
    // The conversation `conv-a` can be resumed (its steps run, then the script); `conv-b` cannot.
    std::fs::write(work_a.join("conv-a"), "[]").unwrap();
    std::fs::write(work_b.join("kept"), "work in progress").unwrap();

    let local = Local::new();
    let _end_all = EndAll(local.clone());
    local
        .launch(&spec(
            "foreign",
            &[
                ("makiba.org", "/another/organization"),
                ("makiba.role", "worker"),
                ("makiba.task_id", "a"),
            ],
        ))
        .unwrap();
    local
        .launch(&spec("untagged", &[("makiba.role", "worker")]))
        .unwrap();

    let first = start(&dir, &local);
    assert!(
        wait_for(30, || status(&dir, "a") == "in_use"
            && status(&dir, "b") == "in_use"),
        "not in use before down: {:?} {:?}",
        launches(&dir, "a"),
        launches(&dir, "b")
    );
    first.stop();

    let summary = makiba::down::run(&dir, &local, makiba::down::Options::default()).unwrap();
    assert_eq!(status(&dir, "a"), "suspended");
    assert_eq!(status(&dir, "b"), "suspended");
    assert!(summary.ended.len() == 2, "ended: {:?}", summary.ended);
    let names: Vec<String> = local.list().unwrap().into_iter().map(|t| t.name).collect();
    assert!(names.contains(&"foreign".to_string()), "{names:?}");
    assert!(names.contains(&"untagged".to_string()), "{names:?}");
    assert!(terminals_of(&local, "b").is_empty());

    {
        let w = StateWriter::open(&dir.join("state.db")).unwrap();
        makiba::up::resume(&w).unwrap();
    }
    let second = start(&dir, &local);
    let back = wait_for(TIMEOUT_MS / 1000 + 60, || {
        status(&dir, "a") == "in_use"
            && status(&dir, "b") == "in_use"
            && launches(&dir, "b").len() == 3
            && launches(&dir, "b")[2].0 == "verified"
    });
    let (la, lb) = (launches(&dir, "a"), launches(&dir, "b"));
    second.stop();
    assert!(back, "not back after up: a {la:?} b {lb:?}");

    for task in ["a", "b"] {
        let ours: Vec<Terminal> = terminals_of(&local, task)
            .into_iter()
            .filter(|t| t.tags.get("makiba.org") == Some(&org))
            .collect();
        assert_eq!(ours.len(), 1, "{task}: {ours:?}");
        assert!(ours[0].alive, "{task}: {ours:?}");
    }

    // `a` was resumed: its new launch carries the stored id, and the agent was given it.
    assert_eq!(la.len(), 2, "{la:?}");
    assert_eq!(la[1].0, "verified");
    assert_eq!(la[1].1.as_deref(), Some("conv-a"));
    let rec_a = std::fs::read_to_string(work_a.join("record")).unwrap();
    assert!(rec_a.contains("arg steps:conv-a"), "{rec_a}");

    // `b`'s resume failed; the fresh launch has the original brief and no resume.
    assert_eq!(lb[1].0, "failed", "{lb:?}");
    assert_eq!(lb[1].1.as_deref(), Some("conv-b"), "{lb:?}");
    assert_eq!(lb[2].2, "the brief of b");
    assert_eq!(lb[2].2, lb[0].2);
    let rec_b = std::fs::read_to_string(work_b.join("record")).unwrap();
    assert!(!rec_b.contains("arg steps:conv-b"), "{rec_b}");
    assert_eq!(rec_b.matches("env-end").count(), 2, "{rec_b}");
    assert_eq!(
        std::fs::read_to_string(work_b.join("kept")).unwrap(),
        "work in progress"
    );
}

/// `down` ends only terminals that carry this organization's tag: a worker's always, the
/// secretary's only with `--secretary`; never one without the tag or with another organization's.
#[test]
fn down_never_ends_a_terminal_without_the_organizations_tag() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(tmp.path()).unwrap();
    let org = dir.to_string_lossy().into_owned();
    StateWriter::open(&dir.join("state.db")).unwrap();
    let stub = Stub::new();
    let launch = |name: &str, tags: &[(&str, &str)]| {
        stub.launch(&spec(name, tags)).unwrap();
    };
    launch(
        "no-tag",
        &[("makiba.role", "worker"), ("makiba.task_id", "x")],
    );
    launch(
        "other-org",
        &[("makiba.org", "/elsewhere"), ("makiba.role", "worker")],
    );
    launch(
        "other-secretary",
        &[("makiba.org", "/elsewhere"), ("makiba.role", "secretary")],
    );
    launch("ours", &[("makiba.org", &org), ("makiba.role", "worker")]);
    launch(
        "secretary",
        &[("makiba.org", &org), ("makiba.role", "secretary")],
    );
    let names =
        |s: &Stub| -> Vec<String> { s.list().unwrap().into_iter().map(|t| t.name).collect() };

    makiba::down::run(&dir, &stub, makiba::down::Options::default()).unwrap();
    assert_eq!(
        names(&stub),
        ["no-tag", "other-org", "other-secretary", "secretary"]
    );

    let with_secretary = makiba::down::Options {
        secretary: true,
        ..Default::default()
    };
    makiba::down::run(&dir, &stub, with_secretary).unwrap();
    assert_eq!(names(&stub), ["no-tag", "other-org", "other-secretary"]);
}
