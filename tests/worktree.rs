//! M3-2: a taken run gets its workplace and its per-task settings (issue M3-2, spec sections 2,
//! 5.3 and 9). The tests run the real binary (`makiba daemon` on the stand-in base): a queued run
//! is taken by `makibad` (M3-1, the `run_taken` event) and then prepared; nothing here starts a
//! terminal. Preparation is run a second time for the same launch by putting its stage back to
//! `planned` while its `run_taken` event stays, which is what a restart after a cut-short pass
//! looks like.

mod common;

use common::makibad;
use makiba::db::writer::{RunUpsert, StateWriter};
use rusqlite::Connection;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// A `makibad` that is killed when the test ends, so a failed test leaves nothing running.
struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A state folder, a project repository with one commit on `main`, and the places of spec
/// section 2 under one temporary folder.
struct Org {
    _dir: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    conn: Connection,
}

impl Org {
    fn new() -> Org {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let repo = root.join("repos/p");
        std::fs::create_dir_all(repo.join("src/deep")).unwrap();
        std::fs::write(repo.join("README.md"), "readme\n").unwrap();
        std::fs::write(repo.join("src/deep/a.txt"), "a\n").unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "first"]);
        let state = root.join("state");
        std::fs::create_dir_all(&state).unwrap();
        let w = StateWriter::open(&state.join("state.db")).unwrap();
        w.ensure_project("p", None, None).unwrap();
        drop(w);
        let conn = Connection::open(state.join("state.db")).unwrap();
        conn.busy_timeout(Duration::from_secs(5)).unwrap();
        Org {
            _dir: dir,
            root,
            repo,
            conn,
        }
    }

    /// A `queued` run with its `planned` launch, as the door leaves it (spec section 5.2).
    fn queue(&self, task: &str, kind: &str) {
        // The workplace as spec section 2 names it, for the row the door will make.
        let place = match kind {
            "document" => self.root.join("docs").join(task),
            _ => self.root.join("workers/p").join(task),
        };
        let w = StateWriter::open(&self.root.join("state/state.db")).unwrap();
        w.upsert_run(&RunUpsert {
            task_id: task,
            project_slug: "p",
            status: Some("queued"),
            ..Default::default()
        })
        .unwrap();
        drop(w);
        self.conn
            .execute(
                "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree)
                 VALUES ((SELECT id FROM runs WHERE task_id = ?1), ?2, ?3, 'fake', 'b', ?4)",
                rusqlite::params![task, format!("spawn-{task}"), kind, place.to_string_lossy()],
            )
            .unwrap();
    }

    fn start(&self) -> Daemon {
        Daemon(
            makibad::daemon(&self.root)
                .env("MAKIBA_STATE_DIR", self.root.join("state"))
                // The door (M3-3) in the scratch folder too, on a port of the system's choosing.
                .env("MAKIBA_DOOR", "none")
                .env("MAKIBA_PORT", "0")
                .env("MAKIBA_PERSON_KEY", self.root.join("key/person.key"))
                .env("MAKIBA_BASE", "stub")
                .env("MAKIBA_WORKERS_DIR", self.root.join("workers"))
                .env("MAKIBA_REPOS_DIR", self.root.join("repos"))
                .env("MAKIBA_DOCS_DIR", self.root.join("docs"))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        )
    }

    /// `(stage, worktree)` of the task's launch.
    fn launch(&self, task: &str) -> (String, PathBuf) {
        self.conn
            .query_row(
                "SELECT stage, worktree FROM run_launch
                 WHERE run_id = (SELECT id FROM runs WHERE task_id = ?1)",
                [task],
                |r| Ok((r.get(0)?, PathBuf::from(r.get::<_, String>(1)?))),
            )
            .unwrap()
    }

    /// Wait until the task's launch is `settings_ready`; its workplace.
    fn prepared(&self, task: &str) -> PathBuf {
        let until = Instant::now() + Duration::from_secs(20);
        loop {
            let (stage, worktree) = self.launch(task);
            if stage == "settings_ready" {
                return worktree;
            }
            assert!(
                stage != "failed",
                "the launch failed: {:?}",
                self.reason(task)
            );
            assert!(Instant::now() < until, "still {stage}");
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn reason(&self, task: &str) -> Option<String> {
        self.conn
            .query_row(
                "SELECT failed_reason FROM run_launch
                 WHERE run_id = (SELECT id FROM runs WHERE task_id = ?1)",
                [task],
                |r| r.get(0),
            )
            .unwrap()
    }

    fn event_count(&self) -> i64 {
        self.conn
            .query_row("SELECT count(*) FROM events", [], |r| r.get(0))
            .unwrap()
    }
}

/// Every file under `dir` (relative, `/`-separated), not looking into a `.git` entry.
fn files(dir: &Path, base: &Path, out: &mut BTreeSet<String>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let e = e.unwrap();
        if e.file_name() == ".git" {
            continue;
        }
        let p = e.path();
        if p.is_dir() {
            files(&p, base, out);
        } else {
            out.insert(
                p.strip_prefix(base)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
}

/// Issue M3-2, first and second items: "After preparation `git status` in the worktree is clean
/// and `git worktree list` in the project shows it", and "No file exists in the worktree that is
/// not in the project's tree at that commit".
#[test]
fn a_prepared_worktree_is_clean_listed_and_holds_only_the_projects_files() {
    let org = Org::new();
    org.queue("t1", "code");
    let _d = org.start();
    let worktree = org.prepared("t1");

    assert_eq!(worktree, org.root.join("workers/p/t1"));
    assert_eq!(
        git(
            &worktree,
            &["status", "--porcelain", "--untracked-files=all"]
        ),
        ""
    );
    // Issue 241: the run has a repository of its own, not a worktree of the project's.
    let listed = git(&org.repo, &["worktree", "list", "--porcelain"]);
    assert!(
        !listed
            .lines()
            .any(|l| l == format!("worktree {}", worktree.display())),
        "{listed}"
    );
    assert_eq!(
        git(
            &worktree,
            &["rev-parse", "--path-format=absolute", "--git-dir"]
        )
        .trim(),
        org.root.join("state/git/t1").display().to_string()
    );

    let mut on_disk = BTreeSet::new();
    files(&worktree, &worktree, &mut on_disk);
    let tracked: BTreeSet<String> = git(&org.repo, &["ls-tree", "-r", "--name-only", "main"])
        .lines()
        .map(String::from)
        .collect();
    assert!(!on_disk.is_empty());
    assert!(
        on_disk.is_subset(&tracked),
        "extra files: {:?}",
        on_disk.difference(&tracked)
    );

    // The settings file is outside the worktree, and the sandbox is on in it.
    let settings = std::fs::read_to_string(org.root.join("state/settings/t1.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&settings).unwrap();
    assert_eq!(v["sandbox"]["enabled"], true);
}

/// Issue M3-2, third item: "Running preparation twice for the same launch changes nothing the
/// second time."
#[test]
fn preparing_twice_changes_nothing_the_second_time() {
    let org = Org::new();
    org.queue("t2", "code");
    let _d = org.start();
    let worktree = org.prepared("t2");
    let settings = org.root.join("state/settings/t2.json");
    let snapshot = || {
        let mut on_disk = BTreeSet::new();
        files(&worktree, &worktree, &mut on_disk);
        (
            org.launch("t2"),
            org.event_count(),
            git(&org.repo, &["worktree", "list", "--porcelain"]),
            git(&org.repo, &["for-each-ref"]),
            git(
                &worktree,
                &["status", "--porcelain", "--untracked-files=all"],
            ),
            on_disk,
            std::fs::read_to_string(&settings).unwrap(),
        )
    };
    let before = snapshot();

    // The launch is prepared again: its stage goes back, its `run_taken` event stays.
    org.conn
        .execute("UPDATE run_launch SET stage = 'planned'", [])
        .unwrap();
    assert_eq!(org.prepared("t2"), worktree);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(before, snapshot());
}

/// Issue M3-2, fourth item: "A `document` launch creates the working folder with `out/` and
/// touches no git repository".
#[test]
fn a_document_launch_gets_a_folder_with_out_and_no_git() {
    let org = Org::new();
    let refs = git(&org.repo, &["for-each-ref"]);
    let listed = git(&org.repo, &["worktree", "list", "--porcelain"]);
    org.queue("d1", "document");
    let _d = org.start();
    let folder = org.prepared("d1");

    assert_eq!(folder, org.root.join("docs/d1"));
    assert!(folder.join("out").is_dir());
    assert_eq!(std::fs::read_dir(folder.join("out")).unwrap().count(), 0);
    assert!(!folder.join(".git").exists());
    assert!(!org.root.join("workers").exists());
    assert_eq!(git(&org.repo, &["for-each-ref"]), refs);
    assert_eq!(git(&org.repo, &["worktree", "list", "--porcelain"]), listed);
}
