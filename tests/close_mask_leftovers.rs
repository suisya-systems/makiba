//! Issue 318 (incident I-15): a close is not refused by the sandbox's mask placeholders.
//!
//! Claude Code's sandbox masks a fixed list of names inside a worker's worktree. A session that
//! ends badly can leave them on disk as untracked, empty, read-only regular files (`.claude/`
//! holding only such files). Those are not work: a worktree holding only them closes. Anything
//! else still refuses: a real file with one of those names, a tracked path, a symlink, a folder
//! with content, or an empty file with another name.
//!
//! The scaffolding is that of `tests/close.rs`: `makibad` runs in this process on `Local`
//! (tests/common/local.rs), the door runs in this process on the same `state.db` with the
//! person's key, and the project `p` is a git repository whose `main` is pushed to a bare
//! `origin`, so "unpushed" has a remote to be measured against.

mod common;

use common::local::Local;
use common::makibad;
use makiba::base::{Base, LaunchSpec};
use makiba::daemon::door::{Door, Response, issue_key};
use makiba::db::writer::{RunUpsert, StateWriter};
use makiba::spec::{ErrorKind, Role};
use rusqlite::{Connection, OpenFlags};
use serde_json::json;
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(30);

/// The names the sandbox masks inside the worktree, as left on disk in the incident.
const MASKS: [&str; 23] = [
    ".bash_profile",
    ".bashrc",
    ".gitconfig",
    ".gitmodules",
    ".profile",
    ".ripgreprc",
    ".zprofile",
    ".zshrc",
    ".mcp.json",
    ".idea",
    ".vscode",
    ".claude/agents",
    ".claude/commands",
    ".claude/hooks",
    ".claude/launch.json",
    ".claude/loop.md",
    ".claude/output-styles",
    ".claude/routines",
    ".claude/scheduled_tasks.json",
    ".claude/settings.json",
    ".claude/settings.local.json",
    ".claude/skills",
    ".claude/workflows",
];

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@e"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// The root of the organization's folders, made once per process.
fn root() -> &'static Path {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        let root = std::fs::canonicalize(tempfile::tempdir().unwrap().keep()).unwrap();
        let repo = root.join("repos/p");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "first"]);
        let origin = root.join("origin.git");
        git(&root, &["init", "-q", "--bare", &origin.to_string_lossy()]);
        git(
            &repo,
            &["remote", "add", "origin", &origin.to_string_lossy()],
        );
        git(&repo, &["push", "-q", "origin", "main"]);
        git(&repo, &["fetch", "-q", "origin"]);
        // SAFETY: every test calls this before it starts a thread; the first sets them, once.
        unsafe {
            std::env::set_var("MAKIBA_WORKERS_DIR", root.join("workers"));
            std::env::set_var("MAKIBA_DOCS_DIR", root.join("docs"));
            std::env::set_var("MAKIBA_REPOS_DIR", root.join("repos"));
        }
        root
    })
}

fn repo() -> PathBuf {
    root().join("repos/p")
}

/// Every mask name but those in `skip` as an empty read-only regular file in `wt`.
fn placeholders(wt: &Path, skip: &[&str]) {
    for name in MASKS.iter().filter(|n| !skip.contains(n)) {
        let p = wt.join(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o444)).unwrap();
    }
}

/// A running `makibad`: its stop flag and its thread.
type Daemon = (Arc<AtomicBool>, JoinHandle<Result<(), String>>);

struct Org {
    task: String,
    state: PathBuf,
    workplace: PathBuf,
    base: Local,
    door: Door,
    person: String,
    daemon: Option<Daemon>,
}

/// Stops the daemon and ends every terminal when the test ends, passed or failed.
impl Drop for Org {
    fn drop(&mut self) {
        if let Some((stop, d)) = self.daemon.take() {
            stop.store(true, Ordering::SeqCst);
            let _ = d.join();
        }
        for t in self.base.list().unwrap_or_default() {
            let _ = self.base.end(&t.id);
        }
    }
}

/// A code run of `task` with a `verified` launch, its key, its terminal, its worktree of the
/// run's own repository on `makiba/<task>`, its settings file, and the worker's `report done`.
fn org(task: &str) -> Org {
    let root = root();
    let state = root.join(format!("state-{task}"));
    std::fs::create_dir_all(state.join("settings")).unwrap();
    std::fs::write(state.join(format!("settings/{task}.json")), "{}").unwrap();
    let workplace = root.join("workers/p").join(task);
    let gitdir = makiba::daemon::run_repo::dir(&state.join("settings"), task);
    let (branch, hooks) = (format!("makiba/{task}"), state.join("git-hooks"));
    makiba::daemon::run_repo::create(&repo(), &gitdir, &workplace, &branch, "main", &hooks)
        .unwrap();
    let w = StateWriter::open(&state.join("state.db")).unwrap();
    let run = w
        .upsert_run(&RunUpsert {
            task_id: task,
            project_slug: "p",
            status: Some("in_use"),
            ..Default::default()
        })
        .unwrap();
    let org = Org {
        task: task.into(),
        state: state.clone(),
        workplace: workplace.clone(),
        base: Local::new(),
        door: Door::new(
            StateWriter::open(&state.join("state.db")).unwrap(),
            Arc::new(Local::new()),
            "fake".into(),
        ),
        person: issue_key(&w, Role::Person, "person", None, None).unwrap(),
        daemon: None,
    };
    let worker = org.launch(&w, run, &format!("{task}-s1"));
    org.door
        .handle(
            Some(&worker),
            "report",
            json!({"state": "done", "task": task, "text": "finished"}),
            None,
            false,
        )
        .unwrap();
    org
}

impl Org {
    /// A `verified` code `run_launch` row, a live key for it and a terminal carrying its tags.
    /// Returns the key.
    fn launch(&self, w: &StateWriter, run: i64, spawn: &str) -> String {
        w.conn()
            .execute(
                "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree, stage)
                 VALUES (?1, ?2, 'code', 'fake', 'the brief', ?3, 'verified')",
                rusqlite::params![run, spawn, self.workplace.to_string_lossy()],
            )
            .unwrap();
        let key = issue_key(
            w,
            Role::Worker,
            &format!("w-{}", self.task),
            Some(run),
            Some(spawn),
        )
        .unwrap();
        let org = self.state.to_string_lossy().into_owned();
        self.base
            .launch(&LaunchSpec {
                argv: vec!["sleep".into(), "600".into()],
                cwd: root().to_path_buf(),
                name: spawn.into(),
                tags: BTreeMap::from([
                    ("makiba.role".to_string(), "worker".to_string()),
                    ("makiba.task_id".to_string(), self.task.clone()),
                    ("makiba.spawn".to_string(), spawn.to_string()),
                    ("makiba.org".to_string(), org),
                ]),
                env: BTreeMap::new(),
                unset: vec![],
            })
            .unwrap();
        key
    }

    fn start(&mut self) {
        let stop = Arc::new(AtomicBool::new(false));
        let (dir, base, flag) = (self.state.clone(), self.base.clone(), stop.clone());
        let d =
            std::thread::spawn(move || makibad::run_until(&dir, &|| Box::new(base.clone()), flag));
        self.daemon = Some((stop, d));
        let until = Instant::now() + WAIT;
        while Instant::now() < until {
            let up = Connection::open_with_flags(
                self.state.join("state.db"),
                OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .and_then(|c| c.query_row("SELECT subscribed FROM dispatcher", [], |r| r.get(0)));
            if up == Ok(1) {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("makibad did not subscribe within {WAIT:?}");
    }

    fn close(&self) -> Response {
        self.door.handle(
            Some(&self.person),
            "close",
            json!({"task_id": self.task, "abandon": false}),
            None,
            false,
        )
    }

    fn one<T: rusqlite::types::FromSql>(&self, sql: &str) -> T {
        let conn = Connection::open_with_flags(
            self.state.join("state.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    /// The status of the newest close command, once it is no longer `accepted`.
    fn finished(&self) -> String {
        let sql = "SELECT status FROM commands WHERE verb = 'close'
                   AND status IN ('accepted', 'done', 'failed')
                   AND json_extract(result_json, '$.result.closed') IS NULL
                   ORDER BY id DESC LIMIT 1";
        let until = Instant::now() + WAIT;
        while Instant::now() < until {
            let s: String = self.one(sql);
            if s != "accepted" {
                return s;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("the close is still accepted after {WAIT:?}");
    }

    fn terminals(&self) -> usize {
        self.base
            .list()
            .unwrap()
            .iter()
            .filter(|t| t.tags.get("makiba.task_id") == Some(&self.task))
            .count()
    }

    fn live_keys(&self) -> i64 {
        self.one(&format!(
            "SELECT count(*) FROM agents WHERE agent_id = 'w-{}' AND revoked_at IS NULL",
            self.task
        ))
    }

    fn settings(&self) -> PathBuf {
        self.state.join(format!("settings/{}.json", self.task))
    }
}

/// A worktree holding only the sandbox's empty read-only placeholders closes as completed, and
/// the placeholders go with the worktree.
#[test]
fn empty_sandbox_mask_placeholders_do_not_refuse_the_close() {
    let mut org = org("m1");
    placeholders(&org.workplace, &[]);
    let seen = git(
        &org.workplace,
        &["status", "--porcelain", "--untracked-files=all"],
    );
    assert!(!seen.is_empty(), "git sees the leftovers as untracked");
    org.start();
    org.close().unwrap();
    assert_eq!(org.finished(), "done");
    assert_eq!(org.terminals(), 0, "no terminal carries the task's tag");
    assert!(!org.workplace.exists(), "the worktree folder is removed");
    assert!(!org.settings().exists(), "the settings file is deleted");
    assert_eq!(org.live_keys(), 0, "every worker key of the run is revoked");
    let listed = git(&repo(), &["worktree", "list", "--porcelain"]);
    assert!(
        !listed.contains(&*org.workplace.to_string_lossy()),
        "the repository no longer lists the worktree: {listed}"
    );
    assert_eq!(
        org.one::<String>("SELECT status FROM runs"),
        "completed".to_string()
    );
}

/// What a case leaves at its path, read back after the refused close.
#[derive(Debug, PartialEq)]
enum Left {
    Content(Vec<u8>),
    Link(PathBuf),
}

fn left(p: &Path) -> Left {
    let meta = std::fs::symlink_metadata(p).unwrap();
    if meta.file_type().is_symlink() {
        Left::Link(std::fs::read_link(p).unwrap())
    } else {
        Left::Content(std::fs::read(p).unwrap())
    }
}

/// A real file with a mask name, a tracked mask path, a symlink, a folder with content or an
/// empty file with another name still refuses the close, beside every other placeholder, and
/// nothing is removed or ended.
#[test]
fn a_real_file_with_a_mask_name_still_refuses_the_close() {
    type Setup = fn(&Path) -> PathBuf;
    let cases: [(&str, Setup); 7] = [
        ("m2a", |wt| {
            placeholders(wt, &[".mcp.json"]);
            std::fs::write(wt.join(".mcp.json"), r#"{"mcpServers":{}}"#).unwrap();
            wt.join(".mcp.json")
        }),
        ("m2b", |wt| {
            placeholders(wt, &[".claude/settings.json"]);
            std::fs::write(wt.join(".claude/settings.json"), "{}").unwrap();
            wt.join(".claude/settings.json")
        }),
        ("m2c", |wt| {
            placeholders(wt, &[]);
            std::fs::write(wt.join("notes"), "").unwrap();
            wt.join("notes")
        }),
        ("m2d", |wt| {
            placeholders(wt, &[]);
            git(wt, &["add", "-f", ".gitconfig"]);
            wt.join(".gitconfig")
        }),
        ("m2e", |wt| {
            placeholders(wt, &[".vscode"]);
            std::fs::write(wt.join(".vscode"), "x").unwrap();
            git(wt, &["add", "-f", ".vscode"]);
            git(wt, &["commit", "-q", "-m", "a vscode file"]);
            std::fs::write(wt.join(".vscode"), "").unwrap();
            wt.join(".vscode")
        }),
        ("m2f", |wt| {
            placeholders(wt, &[".gitmodules"]);
            let target = wt.parent().unwrap().join("m2f-empty-target");
            std::fs::write(&target, "").unwrap();
            std::os::unix::fs::symlink(&target, wt.join(".gitmodules")).unwrap();
            wt.join(".gitmodules")
        }),
        ("m2g", |wt| {
            placeholders(wt, &[".idea"]);
            std::fs::create_dir_all(wt.join(".idea")).unwrap();
            std::fs::write(wt.join(".idea/workspace.xml"), "<project/>").unwrap();
            wt.join(".idea/workspace.xml")
        }),
    ];
    for (task, setup) in cases {
        let org = org(task);
        let path = setup(&org.workplace);
        let before = left(&path);
        let e = org.close().unwrap_err();
        assert_eq!(e.kind, ErrorKind::Conflict, "{task}: {e:?}");
        assert!(
            e.message.contains("uncommitted changes"),
            "{task}: {}",
            e.message
        );
        assert!(e.hint.contains("--abandon"), "{task}: {}", e.hint);
        assert!(org.workplace.exists(), "{task}: the worktree is kept");
        assert_eq!(left(&path), before, "{task}: {} is kept", path.display());
        assert_eq!(org.terminals(), 1, "{task}: nothing is ended");
        assert_eq!(org.live_keys(), 1, "{task}: no key is revoked");
        assert_ne!(
            org.one::<String>("SELECT status FROM runs"),
            "completed",
            "{task}"
        );
    }
}
