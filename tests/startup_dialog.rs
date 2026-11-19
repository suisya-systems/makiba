//! M3-5 (Issue 28): startup dialogs. The daemon runs in this process on `Local` (as
//! tests/launch.rs); a queued run's launch waits at `settings_ready` and the launch (M3-4) takes it
//! to `launched`. The test agent is given by `MAKIBA_FAKE_AGENT`: it runs the steps of
//! `steps.json` in its working folder (the launch's workplace) and records the keys it receives in
//! `record` there, so each launch has its own script and record. Its `menu` step draws the dialog
//! frames Claude Code 2.1.326 showed in probe P10 (docs/design/probes.md; the workspace path
//! replaced), with the statuses its second run measured on 2.1.327: the folder-trust dialog is
//! shown with the status blocked, permission, "trust this folder", which goes once it is passed;
//! the development-channel dialog with no status. A key is allowed only with no status, or with
//! that status while the folder-trust dialog is shown and matched whole (the owner's decision on
//! Issue 28, 2026-11-19).

mod common;

use common::local::Local;
use makiba::base::{Base, State, TerminalRef};
use makiba::daemon;
use makiba::db::writer::{RunUpsert, StateWriter};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Once;
use std::time::{Duration, Instant};

const AGENT: &str = env!("CARGO_BIN_EXE_makiba-fake-agent");

fn setup() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let agent = json!([AGENT, "record", "steps:steps.json"]).to_string();
        // SAFETY: every test calls this before it starts a thread; the first sets it, once.
        unsafe { std::env::set_var("MAKIBA_FAKE_AGENT", agent) };
    });
}

fn fence() -> String {
    "\u{2500}".repeat(80)
}

/// The folder-trust dialog of P10 with its two option lines, the cursor on the first.
fn trust(no: &str, yes: &str) -> String {
    let menu = json!({
        "above": [
            "", fence(), " Accessing workspace:", "", " /work/repo", "",
            " Quick safety check: Is this a project you created or one you trust? (Like your own \
             code, a well-known open source project, or work from your team). If not, take a \
             moment to review what's in this",
            " folder first.", "",
            " Claude Code'll be able to read, edit, and execute files here.", "",
            " Security guide", "",
        ],
        "options": [no, yes],
        "at": 0,
        "indent": " ",
        "below": ["", " Enter to confirm \u{b7} Esc to cancel"],
    });
    format!("menu:{menu}")
}

/// The development-channel dialog of P10, the cursor on line 1.
fn channel(one: &str, two: &str) -> String {
    let menu = json!({
        "above": [
            "", fence(), "  WARNING: Loading development channels", "",
            "  --dangerously-load-development-channels is for local channel development only. Do \
             not use this option to run channels you have downloaded off the internet.",
            "", "  Please use --channels to run a list of approved channels.", "",
            "  Channels: server:makiba-channel", "",
        ],
        "options": [format!("1. {one}"), format!("2. {two}")],
        "at": 0,
        "indent": "  ",
        "below": ["", "  Enter to confirm \u{b7} Esc to cancel"],
    });
    format!("menu:{menu}")
}

const NO: &str = "No, exit";
const YES: &str = "Yes, I trust this folder";
const LINE_1: &str = "I am using this for local development";
const LINE_2: &str = "Exit";
/// The status the base reports while the folder-trust dialog is shown (P10, second run).
const TRUST_STATUS: &str = "blocked:permission:trust this folder";

/// Ends the terminals when the test ends, passed or failed.
struct Ended(Local, Vec<TerminalRef>);

impl Drop for Ended {
    fn drop(&mut self) {
        for t in &self.1 {
            let _ = self.0.end(t);
        }
    }
}

struct Org {
    dir: PathBuf,
    work: PathBuf,
    local: Local,
    _ended: Ended,
    _tmp: tempfile::TempDir,
}

/// A daemon on `Local` with one launch per `(task, steps)` at `settings_ready`.
fn org(launches: &[(&str, Vec<String>)]) -> Org {
    setup();
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let dir = root.join("state");
    std::fs::create_dir_all(&dir).unwrap();
    let w = StateWriter::open(&dir.join("state.db")).unwrap();
    for (task, steps) in launches {
        let work = root.join(task);
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("steps.json"), json!(steps).to_string()).unwrap();
        w.upsert_run(&RunUpsert {
            task_id: task,
            project_slug: "p",
            // Not `in_use`: an `in_use` run with no terminal is recorded as lost (M2-5).
            status: Some("queued"),
            ..Default::default()
        })
        .unwrap();
        w.conn()
            .execute(
                "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree, stage)
                 VALUES ((SELECT id FROM runs WHERE task_id = ?1), 'spawn-' || ?1, 'document',
                         'fake', 'the brief', ?2, 'settings_ready')",
                [task.to_string(), work.to_string_lossy().into_owned()],
            )
            .unwrap();
    }
    drop(w);
    let local = Local::new();
    let ended = Ended(
        local.clone(),
        launches
            .iter()
            .map(|(t, _)| TerminalRef(format!("w-{t}")))
            .collect(),
    );
    let for_daemon = local.clone();
    let daemon_dir = dir.clone();
    std::thread::spawn(move || daemon::run(&daemon_dir, &|| Box::new(for_daemon.clone())));
    Org {
        dir,
        work: root,
        local,
        _ended: ended,
        _tmp: tmp,
    }
}

impl Org {
    fn db(&self) -> Option<Connection> {
        Connection::open_with_flags(self.dir.join("state.db"), OpenFlags::SQLITE_OPEN_READ_ONLY)
            .ok()
    }

    fn stage(&self, task: &str) -> Option<String> {
        self.db()?
            .query_row(
                "SELECT l.stage FROM run_launch l JOIN runs r ON r.id = l.run_id
                 WHERE r.task_id = ?1",
                [task],
                |r| r.get(0),
            )
            .ok()
    }

    /// Wait up to 20 seconds for `task`'s launch to reach `stage`.
    fn reaches(&self, task: &str, stage: &str) {
        let until = Instant::now() + Duration::from_secs(20);
        while Instant::now() < until {
            if self.stage(task).as_deref() == Some(stage) {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        assert_eq!(
            self.stage(task).as_deref(),
            Some(stage),
            "{task}: screen {:?}, record {:?}",
            self.local.read(&TerminalRef(format!("w-{task}")), 0),
            self.record(task)
        );
    }

    /// Wait up to 20 seconds for `task`'s launch to reach one of `stages`.
    fn reaches_one_of(&self, task: &str, stages: &[&str]) {
        let until = Instant::now() + Duration::from_secs(20);
        while Instant::now() < until {
            if self
                .stage(task)
                .is_some_and(|s| stages.contains(&s.as_str()))
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("{task}: still {:?}", self.stage(task));
    }

    /// What the test agent of `task` received, one row per key, and its choices.
    fn record(&self, task: &str) -> Vec<String> {
        std::fs::read_to_string(self.work.join(task).join("record"))
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
    }

    /// The payloads of the `worker_spawn_failed` events.
    fn spawn_failed(&self) -> Vec<Value> {
        let conn = self.db().unwrap();
        let mut q = conn
            .prepare("SELECT payload_json FROM events WHERE kind = 'worker_spawn_failed'")
            .unwrap();
        q.query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|p| serde_json::from_str(&p.unwrap()).unwrap())
            .collect()
    }
}

/// Long enough for several passes of the daemon, each of which could press a key.
fn a_while() {
    std::thread::sleep(Duration::from_millis(1500));
}

fn steps(steps: &[&str]) -> Vec<String> {
    steps.iter().map(|s| s.to_string()).collect()
}

#[test]
fn a_folder_trust_dialog_with_the_cursor_on_no_exit_ends_with_the_trust_line_chosen() {
    let org = org(&[(
        "t1",
        steps(&[TRUST_STATUS, &trust(NO, YES), "none", "idle", "hang"]),
    )]);
    org.reaches("t1", "dialog_passed");
    // The cursor was moved down onto the trust line, then Enter; line 2 is the trust line.
    assert_eq!(org.record("t1"), ["key down", "key enter", "chose 2"]);
    let t = org.local.get(&TerminalRef("w-t1".into())).unwrap();
    assert!(t.alive, "the agent is not running");
    assert_eq!(t.status.map(|s| s.state), Some(State::Idle));
}

#[test]
fn the_same_dialog_with_one_option_line_reworded_is_not_answered_and_the_launch_fails() {
    let org = org(&[(
        "t1",
        steps(&[
            TRUST_STATUS,
            &trust("No, leave", YES),
            "none",
            "idle",
            "hang",
        ]),
    )]);
    org.reaches("t1", "failed");
    a_while();
    assert!(
        org.record("t1").is_empty(),
        "a key was sent: {:?}",
        org.record("t1")
    );
    let failed = org.spawn_failed();
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert_eq!(failed[0]["spawn_id"], "spawn-t1");
    let screen = failed[0]["screen"].as_str().unwrap_or_default();
    assert!(
        screen.contains("No, leave") && screen.contains(YES),
        "the screen is not recorded: {screen:?}"
    );
}

#[test]
fn no_key_is_ever_sent_to_a_terminal_that_has_a_status() {
    // Read with the owner's decision: other than the folder-trust dialog's own status while that
    // dialog is shown. Each agent reports a status first, then shows a dialog of the list.
    let status_then = |status: &str, menu: &str| steps(&[status, menu, "hang"]);
    let trust = trust(NO, YES);
    let org = org(&[
        ("t1", status_then("idle", &trust)),
        ("t2", status_then("working", &trust)),
        ("t3", status_then("done", &trust)),
        ("t4", status_then("blocked:permission:allow Bash?", &trust)),
        // The folder-trust dialog's status, but the development-channel dialog on the screen.
        ("t5", status_then(TRUST_STATUS, &channel(LINE_1, LINE_2))),
    ]);
    for task in ["t1", "t2", "t3", "t4"] {
        // An agent whose first status is `working` goes on to `verified` (M3-6).
        org.reaches_one_of(task, &["dialog_passed", "verified"]);
    }
    a_while();
    for (task, line) in [
        ("t1", NO),
        ("t2", NO),
        ("t3", NO),
        ("t4", NO),
        ("t5", LINE_2),
    ] {
        assert!(
            org.record(task).is_empty(),
            "{task}: a key was sent: {:?}",
            org.record(task)
        );
        let screen = org
            .local
            .read(&TerminalRef(format!("w-{task}")), 0)
            .unwrap();
        assert!(
            screen.iter().any(|l| l.ends_with(line)),
            "{task}: the dialog is not shown: {screen:?}"
        );
    }
}

#[test]
fn the_development_channel_dialog_after_the_trust_dialog_ends_with_line_1_chosen() {
    let org = org(&[
        (
            "t1",
            steps(&[
                TRUST_STATUS,
                &trust(NO, YES),
                "none",
                &channel(LINE_1, LINE_2),
                "idle",
                "hang",
            ]),
        ),
        (
            "t2",
            steps(&[
                TRUST_STATUS,
                &trust(NO, YES),
                "none",
                &channel(LINE_1, "Quit"),
                "idle",
                "hang",
            ]),
        ),
    ]);
    org.reaches("t1", "dialog_passed");
    assert_eq!(
        org.record("t1"),
        ["key down", "key enter", "chose 2", "key enter", "chose 1"]
    );
    // The same dialog with one option line reworded: the trust dialog is passed, this one is not.
    org.reaches("t2", "failed");
    a_while();
    assert_eq!(org.record("t2"), ["key down", "key enter", "chose 2"]);
    let failed = org.spawn_failed();
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert_eq!(failed[0]["spawn_id"], "spawn-t2");
    assert!(
        failed[0]["screen"]
            .as_str()
            .is_some_and(|s| s.contains("2. Quit")),
        "{failed:?}"
    );
}
