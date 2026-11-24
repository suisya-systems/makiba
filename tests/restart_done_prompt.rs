//! Issue 426: the launch step records in `worker_spawned` which line it gave the agent (`prompt`:
//! `resume` for `RESUME_PROMPT`, `unpark` for `UNPARK_PROMPT`, `brief` for the brief), so that the
//! `done_no_report` rule can tell the turn `RESUME_PROMPT` told to stay silent from any other. A
//! set `conversation_id` does not tell it: verification sets one for a fresh launch too.
//!
//! The daemon runs in this process on `Local`, as in `tests/resume_prompt.rs`; `claude` is a
//! stand-in on `PATH` that writes its arguments, one per line, to `args` in its working folder and
//! then waits.

mod common;

use common::local::Local;
use common::makibad;
use makiba::base::Base;
use makiba::daemon::launch::{RESUME_PROMPT, UNPARK_PROMPT};
use makiba::db::writer::{NewEvent, RunUpsert, StateWriter};
use rusqlite::OptionalExtension;
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Ends every terminal when the test ends: the daemon's event thread keeps a clone of `Local`.
struct EndAll(Local);

impl Drop for EndAll {
    fn drop(&mut self) {
        for t in self.0.list().unwrap_or_default() {
            let _ = self.0.end(&t.id);
        }
    }
}

/// A queued document run whose launch `spawn-<task>` waits at `settings_ready`, resuming
/// `resume` if given.
fn queue(w: &StateWriter, root: &Path, task: &str, resume: Option<&str>) -> std::path::PathBuf {
    let work = root.join(task);
    std::fs::create_dir_all(&work).unwrap();
    w.upsert_run(&RunUpsert {
        task_id: task,
        project_slug: "p",
        status: Some("queued"),
        ..Default::default()
    })
    .unwrap();
    w.conn()
        .execute(
            "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree, stage,
                                     conversation_id)
             VALUES ((SELECT id FROM runs WHERE task_id = ?1), 'spawn-' || ?1, 'document',
                     'claude_code', 'the brief of ' || ?1, ?2, 'settings_ready', ?3)",
            rusqlite::params![task, work.to_string_lossy(), resume],
        )
        .unwrap();
    work
}

/// The arguments the stand-in wrote, once it has written them.
fn args(work: &Path) -> Vec<String> {
    let until = Instant::now() + Duration::from_secs(20);
    while Instant::now() < until {
        if let Ok(text) = std::fs::read_to_string(work.join("args")) {
            return text.lines().map(String::from).collect();
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("claude was not started in {}", work.display());
}

/// The payload of the `worker_spawned` of `spawn`, once it is recorded.
fn spawned(db: &Path, spawn: &str) -> Value {
    let until = Instant::now() + Duration::from_secs(20);
    while Instant::now() < until {
        let w = StateWriter::open(db).unwrap();
        let found: Option<String> = w
            .conn()
            .query_row(
                "SELECT payload_json FROM events WHERE kind = 'worker_spawned'
                   AND json_extract(payload_json, '$.spawn_id') = ?1
                 ORDER BY id DESC LIMIT 1",
                [spawn],
                |r| r.get(0),
            )
            .optional()
            .unwrap();
        if let Some(p) = found {
            return serde_json::from_str(&p).unwrap();
        }
        drop(w);
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("no worker_spawned for {spawn}");
}

#[test]
fn worker_spawned_names_the_line_the_launch_was_given() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let claude = bin.join("claude");
    std::fs::write(
        &claude,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > args.part && /bin/mv -f args.part args\nexec sleep 30\n",
    )
    .unwrap();
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
    // SAFETY: the only test of this binary, and no thread of it has started yet.
    unsafe {
        let path = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", format!("{}:{path}", bin.display()));
    }

    let dir = root.join("state");
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("state.db");
    let (resumed, unparked, fresh) = {
        let w = StateWriter::open(&db).unwrap();
        let r = queue(&w, &root, "r", Some("conv-r"));
        let u = queue(&w, &root, "u", Some("conv-u"));
        w.append_event(&NewEvent {
            kind: "run_unparked",
            actor: Some("makibad"),
            payload: Some(&json!({"task_id": "u", "spawn_id": "spawn-u", "reason": "answer"})),
            run_task_id: Some("u"),
            ..Default::default()
        })
        .unwrap();
        let f = queue(&w, &root, "f", None);
        (r, u, f)
    };

    let local = Local::new();
    let _end_all = EndAll(local.clone());
    let stop = Arc::new(AtomicBool::new(false));
    let thread = {
        let (dir, local, stop) = (dir.clone(), local.clone(), Arc::clone(&stop));
        std::thread::spawn(move || makibad::run_until(&dir, &|| Box::new(local.clone()), stop))
    };
    let (r, u, f) = (args(&resumed), args(&unparked), args(&fresh));
    let (pr, pu, pf) = (
        spawned(&db, "spawn-r"),
        spawned(&db, "spawn-u"),
        spawned(&db, "spawn-f"),
    );
    stop.store(true, Ordering::SeqCst);
    thread.join().unwrap().unwrap();

    assert_eq!(r.last().map(String::as_str), Some(RESUME_PROMPT), "{r:?}");
    assert_eq!(u.last().map(String::as_str), Some(UNPARK_PROMPT), "{u:?}");
    assert!(
        !f.iter().any(|x| x == RESUME_PROMPT || x == UNPARK_PROMPT),
        "{f:?}"
    );
    assert_eq!(pr["prompt"], "resume", "{pr}");
    assert_eq!(pu["prompt"], "unpark", "{pu}");
    assert_eq!(pf["prompt"], "brief", "{pf}");
}
