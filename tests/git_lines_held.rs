//! Issue 140: the git guard lines held by facts of the launch, not by reading a command as text.
//! Two holders are built here, both in the settings `makibad` writes for a launch:
//!
//! - B1 (line L-K2): the person's git and gh credential files are denied to a worker and to the
//!   curator (sandbox `denyRead` and `denyWrite`, and `Read` / `Edit` rules for the tools), at
//!   their default places and under the `XDG_CONFIG_HOME` `makibad` has; the secretary keeps its
//!   own GitHub login.
//! - B2 (line L-K3, the hooks path of L-K12): a code launch's settings deny writing the project
//!   clone's `.git/config`, which holds `core.hooksPath`, by makiba's own entry.
//!
//! Each case also checks that `docs/design/lines.md` names the holder, and the Issue of each
//! holder that is decided here but not built. The daemon runs in this process on the stand-in
//! base, as in `tests/worktree_mcp_json_deny.rs`.

use makiba::base::stub::Stub;
use makiba::daemon;
use makiba::daemon::launch::add_launch;
use makiba::db::writer::{RunUpsert, StateWriter};
use makiba::spec::Role;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The settings files written for one code run (worker), one curator and one secretary launch.
struct Written {
    repo: PathBuf,
    xdg: PathBuf,
    worker: Value,
    curator: Value,
    secretary: Value,
}

fn strings(v: &Value, pointer: &str) -> Vec<String> {
    v.pointer(pointer)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

/// The pattern of a permission rule for `path`, as the settings writer forms it.
fn rule(path: &str) -> String {
    if path.starts_with('/') {
        format!("/{path}")
    } else {
        path.to_string()
    }
}

fn git(dir: &Path, args: &[&str]) {
    let st = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@e"])
        .args(args)
        .status()
        .unwrap();
    assert!(st.success(), "git {args:?}");
}

fn wait_json(file: &Path) -> Value {
    let until = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(v) = std::fs::read_to_string(file)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
        {
            return v;
        }
        assert!(Instant::now() < until, "no settings at {}", file.display());
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// The scratch folder, removed when the last case of this binary ends, passing or not.
static SCRATCH: Mutex<Option<tempfile::TempDir>> = Mutex::new(None);
static ENDED: AtomicUsize = AtomicUsize::new(0);
const CASES: usize = 2;

struct Ended;
impl Drop for Ended {
    fn drop(&mut self) {
        if ENDED.fetch_add(1, Ordering::SeqCst) + 1 == CASES {
            let taken = SCRATCH.lock().map(|mut s| s.take()).ok().flatten();
            drop(taken);
        }
    }
}

/// One daemon for both cases: the environment is set once, before any thread of it starts.
fn written() -> &'static Written {
    static WRITTEN: OnceLock<Written> = OnceLock::new();
    WRITTEN.get_or_init(|| {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        *SCRATCH.lock().unwrap() = Some(tmp);
        let dir = root.join("state");
        let repo = root.join("repos/p");
        let xdg = root.join("xdg-config");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "m"]);
        let agent = serde_json::json!([root.join("no-such-agent"), root.join("record")]);
        // SAFETY: set inside the once-initializer, before the daemon's thread starts; the other
        // test of this binary waits on the same initializer and reads no variable meanwhile.
        unsafe {
            std::env::set_var("MAKIBA_PERSON_KEY", root.join("person.key"));
            std::env::set_var("MAKIBA_DOCS_DIR", root.join("docs"));
            std::env::set_var("MAKIBA_WORKERS_DIR", root.join("workers"));
            std::env::set_var("MAKIBA_REPOS_DIR", root.join("repos"));
            std::env::set_var("MAKIBA_FAKE_AGENT", agent.to_string());
            std::env::set_var("XDG_CONFIG_HOME", &xdg);
        }
        let (curator, secretary) = {
            let w = StateWriter::open(&dir.join("state.db")).unwrap();
            w.ensure_project("p", None, None).unwrap();
            w.upsert_run(&RunUpsert {
                task_id: "t1",
                project_slug: "p",
                status: Some("queued"),
                ..Default::default()
            })
            .unwrap();
            w.conn()
                .execute(
                    "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree)
                     VALUES ((SELECT id FROM runs WHERE task_id = 't1'), 'spawn-t1', 'code',
                             'fake', 'the brief', ?1)",
                    [root.join("workers/p/t1").to_string_lossy()],
                )
                .unwrap();
            let folder = |name: &str| {
                let f = root.join(name);
                std::fs::create_dir_all(&f).unwrap();
                f
            };
            let c = add_launch(&w, Role::Curator, &folder("cur"), "b", None, "fake").unwrap();
            let s = add_launch(&w, Role::Secretary, &folder("sec"), "b", None, "fake").unwrap();
            (c, s)
        };
        let daemon_dir = dir.clone();
        std::thread::spawn(move || daemon::run(&daemon_dir, &|| Box::new(Stub::new())));
        let settings = dir.join("settings");
        Written {
            worker: wait_json(&settings.join("t1.json")),
            curator: wait_json(&settings.join(format!("curator-{curator}.json"))),
            secretary: wait_json(&settings.join(format!("secretary-{secretary}.json"))),
            repo,
            xdg,
        }
    })
}

/// The row of `docs/design/lines.md` whose line id is `id`.
fn row(id: &str) -> String {
    let lines =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/design/lines.md"))
            .unwrap();
    lines
        .lines()
        .find(|l| l.starts_with(&format!("| {id} ")))
        .unwrap_or_else(|| panic!("lines.md has no {id} row"))
        .to_string()
}

/// B1. A worker's and the curator's settings deny the person's git and gh credential files, so
/// that no credential to push with or to write to GitHub is readable from their Bash or tools.
#[test]
fn a_workers_settings_deny_reading_the_persons_git_credentials() {
    let _ended = Ended;
    let w = written();
    let xdg = w.xdg.display().to_string();
    let folders = ["~/.ssh".to_string()];
    let files = [
        "~/.git-credentials".to_string(),
        "~/.config/git/credentials".to_string(),
        format!("{xdg}/git/credentials"),
        format!("{xdg}/gh/hosts.yml"),
    ];
    for (who, v) in [("worker", &w.worker), ("curator", &w.curator)] {
        let deny = strings(v, "/permissions/deny");
        for list in ["denyRead", "denyWrite"] {
            let named = strings(v, &format!("/sandbox/filesystem/{list}"));
            for p in folders.iter().chain(&files) {
                assert!(
                    named.contains(p),
                    "{who}: sandbox {list} does not name {p}: {v}"
                );
            }
        }
        for p in &folders {
            let r = format!("Read({}/**)", rule(p));
            assert!(deny.contains(&r), "{who}: no {r}: {v}");
        }
        for p in &files {
            let r = format!("Read({})", rule(p));
            assert!(deny.contains(&r), "{who}: no {r}: {v}");
        }
    }
    // The secretary's own `gh` keeps its stored login.
    let named = strings(&w.secretary, "/sandbox/filesystem/denyRead");
    for p in [&folders[0], &files[3]] {
        assert!(
            !named.contains(p),
            "secretary: {p} is denied: {}",
            w.secretary
        );
    }

    // lines.md: L-K2 names this holder; the network, not held today, names its Issue (#244),
    // as does L-K7, which says the same of every host.
    let k2 = row("L-K2");
    assert!(
        k2.contains(
            "`tests/git_lines_held.rs::a_workers_settings_deny_reading_the_persons_git_credentials`"
        ),
        "L-K2 does not name the credential deny: {k2}"
    );
    assert!(k2.contains("`Open: #244`"), "L-K2 does not name #244: {k2}");
    let k7 = row("L-K7");
    assert!(k7.contains("`Open: #244`"), "L-K7 does not name #244: {k7}");
}

/// B2. A code launch's settings deny writing the project clone's `.git/config` (where
/// `core.hooksPath` names the secret scanner's folder), for Bash and for the tools.
#[test]
fn a_code_launchs_settings_deny_writing_the_clones_git_config() {
    let _ended = Ended;
    let w = written();
    let config = w.repo.join(".git/config").display().to_string();
    assert!(
        strings(&w.worker, "/sandbox/filesystem/denyWrite").contains(&config),
        "sandbox denyWrite does not name {config}: {}",
        w.worker
    );
    let r = format!("Edit({})", rule(&config));
    assert!(
        strings(&w.worker, "/permissions/deny").contains(&r),
        "no {r}: {}",
        w.worker
    );

    // lines.md: L-K3 names this holder, a run that shares no writable `.git` with the clone, and
    // the Issue of the holder decided but not built: a ruleset on each default
    // branch (#242). L-K6 names the whole temporary folder in the write surface (#243).
    let k3 = row("L-K3");
    assert!(
        k3.contains(
            "`tests/git_lines_held.rs::a_code_launchs_settings_deny_writing_the_clones_git_config`"
        ),
        "L-K3 does not name the config deny: {k3}"
    );
    assert!(k3.contains("`Open: #242`"), "L-K3 does not name #242: {k3}");
    assert!(
        k3.contains("`tests/workplace_own_git.rs::a_code_run_has_a_repository_of_its_own_and_its_commit_still_reaches_the_remote`"),
        "L-K3 does not name the run's own repository (#241): {k3}"
    );
    let k6 = row("L-K6");
    assert!(k6.contains("`Open: #243`"), "L-K6 does not name #243: {k6}");
}
