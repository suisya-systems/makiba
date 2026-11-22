//! Issue 41: a resumed Claude Code worker is given a fixed line in place of its brief, since its
//! conversation already holds the task; a launch that is not a resume is given the brief. The
//! daemon runs in this process on `Local`; `claude` is a stand-in on `PATH` that writes its
//! arguments, one per line, to `args` in its working folder and then waits.

mod common;

use common::local::Local;
use common::makibad;
use makiba::base::Base;
use makiba::daemon::launch::RESUME_PROMPT;
use makiba::db::writer::{RunUpsert, StateWriter};
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

/// A queued document run whose launch waits at `settings_ready`, resuming `resume` if given.
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

#[test]
fn a_resume_is_given_the_fixed_line_and_a_first_launch_its_brief() {
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
    let (resumed, fresh) = {
        let w = StateWriter::open(&dir.join("state.db")).unwrap();
        (
            queue(&w, &root, "r", Some("conv-r")),
            queue(&w, &root, "f", None),
        )
    };

    let local = Local::new();
    let _end_all = EndAll(local.clone());
    let stop = Arc::new(AtomicBool::new(false));
    let thread = {
        let (dir, local, stop) = (dir.clone(), local.clone(), Arc::clone(&stop));
        std::thread::spawn(move || makibad::run_until(&dir, &|| Box::new(local.clone()), stop))
    };
    let (r, f) = (args(&resumed), args(&fresh));
    stop.store(true, Ordering::SeqCst);
    thread.join().unwrap().unwrap();

    let after = |a: &[String], flag: &str| {
        a.iter()
            .position(|x| x == flag)
            .and_then(|i| a.get(i + 1).cloned())
    };
    assert_eq!(r.last().map(String::as_str), Some(RESUME_PROMPT), "{r:?}");
    assert!(!r.iter().any(|x| x == "the brief of r"), "{r:?}");
    assert_eq!(after(&r, "--resume").as_deref(), Some("conv-r"));
    assert_eq!(
        f[f.len().saturating_sub(3)..].join("\n"),
        concat!(
            "the brief of f\n\n",
            include_str!("../roles/locale/artifact/en.md")
        )
        .trim(),
        "{f:?}"
    );
    assert!(!f.iter().any(|x| x == RESUME_PROMPT), "{f:?}");
    assert!(after(&f, "--session-id").is_some(), "{f:?}");
}
