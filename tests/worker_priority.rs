//! Issue 626: a worker's program runs below `makibad`'s priority when the organization asks for
//! it. With `MAKIBA_WORKER_NICE` and `MAKIBA_WORKER_BUILD_JOBS`, the launch's argv begins with
//! `nice -n <n>` (by the absolute path `PATH` finds), its environment carries `CARGO_BUILD_JOBS`,
//! and its `worker_spawned` records the values as `priority`. Without them the argv, the
//! environment and the event are as before.
//!
//! `makibad` runs on the stand-in base, which writes each launch's name, argv and env to the file
//! `MAKIBA_STUB_LAUNCHES` names. A code run of the `claude_code` agent is queued, with a stand-in
//! `claude` on the path that is never started: only the record is read.

#![cfg(target_os = "linux")]

mod common;

use common::makibad;
use makiba::db::writer::{RunUpsert, StateWriter};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const TASK: &str = "t626";

fn git_out(dir: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn git(dir: &Path, args: &[&str]) {
    let out = git_out(dir, args);
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A `makibad` that is killed when the test ends, so a failed test leaves nothing running.
struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn wait<T>(what: &str, within: Duration, mut f: impl FnMut() -> Option<T>) -> T {
    let until = Instant::now() + within;
    while Instant::now() < until {
        if let Some(v) = f() {
            return v;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("timed out waiting for {what}");
}

/// A scratch organization: the clone `repos/p` pushed to a bare `origin.git`, its folders, and
/// `bin/` with a stand-in `claude`.
struct Org {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

/// What the stand-in base recorded for the launch, and its `worker_spawned` payload.
struct Launched {
    argv: Vec<String>,
    env: serde_json::Map<String, Value>,
    spawned: Value,
}

impl Org {
    fn new() -> Org {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        for d in [
            "bin",
            "home",
            "state",
            "config",
            "data",
            "run",
            "xdg-state",
            "tmp",
            "workers",
            "docs",
        ] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        let clone = root.join("repos/p");
        std::fs::create_dir_all(&clone).unwrap();
        std::fs::write(clone.join("README.md"), "readme\n").unwrap();
        git(&clone, &["init", "-q", "-b", "main"]);
        git(&clone, &["add", "README.md"]);
        git(&clone, &["commit", "-q", "-m", "first"]);
        let origin = root.join("origin.git");
        git(&root, &["init", "-q", "--bare", &origin.to_string_lossy()]);
        git(
            &clone,
            &["remote", "add", "origin", &origin.to_string_lossy()],
        );
        git(&clone, &["push", "-q", "origin", "main"]);
        git(&clone, &["fetch", "-q", "origin"]);
        common::exe::write(
            &root.join("bin/claude"),
            "#!/bin/sh\necho 'stand-in claude' >&2\nexit 1\n",
        );
        let w = StateWriter::open(&root.join("state/state.db")).unwrap();
        w.ensure_project("p", None, None).unwrap();
        Org { _dir: dir, root }
    }

    fn state(&self) -> PathBuf {
        self.root.join("state")
    }

    fn launches(&self) -> PathBuf {
        self.root.join("launches.jsonl")
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.root.join("makibad.log")).unwrap_or_default()
    }

    fn conn(&self) -> Option<Connection> {
        let c = Connection::open_with_flags(
            self.state().join("state.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .ok()?;
        c.busy_timeout(Duration::from_secs(5)).ok()?;
        Some(c)
    }

    /// `makibad` with the worker priority `setting` (none when empty), once it is subscribed and
    /// has beaten.
    fn start(&self, setting: &[(&str, &str)]) -> Daemon {
        let log = std::fs::File::create(self.root.join("makibad.log")).unwrap();
        let path = format!(
            "{}:{}",
            self.root.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut m = makibad::daemon(&self.root);
        m.env("PATH", path)
            .env("HOME", self.root.join("home"))
            .env("MAKIBA_STATE_DIR", self.state())
            .env("XDG_STATE_HOME", self.root.join("xdg-state"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("XDG_RUNTIME_DIR", self.root.join("run"))
            .env("MAKIBA_PERSON_KEY", self.root.join("config/person.key"))
            .env("MAKIBA_DOOR", "none")
            .env("MAKIBA_PORT", "0")
            .env("MAKIBA_BASE", "stub")
            .env("MAKIBA_STUB_SUBSCRIBE_DELAY_MS", "0")
            .env("MAKIBA_STUB_LAUNCHES", self.launches())
            .env("MAKIBA_SHARE_DIR", self.root.join("share"))
            .env("MAKIBA_WORKERS_DIR", self.root.join("workers"))
            .env("MAKIBA_REPOS_DIR", self.root.join("repos"))
            .env("MAKIBA_DOCS_DIR", self.root.join("docs"))
            .env("TMPDIR", self.root.join("tmp"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("MAKIBA_FAKE_AGENT")
            .env_remove("MAKIBA_WORKER_NICE")
            .env_remove("MAKIBA_WORKER_BUILD_JOBS")
            .env_remove("CARGO_BUILD_JOBS");
        for (k, v) in setting {
            m.env(k, v);
        }
        let child = m
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .unwrap();
        let d = Daemon(child);
        let pid = i64::from(d.0.id());
        wait("makibad to run", Duration::from_secs(30), || {
            self.conn()?
                .query_row(
                    "SELECT 1 FROM dispatcher WHERE id = 1 AND pid = ?1 AND subscribed = 1
                       AND heartbeat_at IS NOT NULL",
                    [pid],
                    |_| Ok(()),
                )
                .ok()
        });
        d
    }

    fn spawn_id(&self) -> String {
        format!("spawn-{TASK}")
    }

    /// A queued code run of `TASK` by `claude_code`, and its planned launch.
    fn queue(&self) {
        let w = StateWriter::open(&self.state().join("state.db")).unwrap();
        w.upsert_run(&RunUpsert {
            task_id: TASK,
            project_slug: "p",
            status: Some("queued"),
            ..Default::default()
        })
        .unwrap();
        w.conn()
            .execute(
                "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree)
                 VALUES ((SELECT id FROM runs WHERE task_id = ?1), ?2, 'code', 'claude_code', 'b', ?3)",
                rusqlite::params![
                    TASK,
                    self.spawn_id(),
                    self.root.join("workers/p").join(TASK).to_string_lossy()
                ],
            )
            .unwrap();
    }

    /// The stand-in base's record of the launch named `w-TASK`, once there is one.
    fn record(&self) -> Option<(Vec<String>, serde_json::Map<String, Value>)> {
        let name = format!("w-{TASK}");
        let text = std::fs::read_to_string(self.launches()).ok()?;
        let v = text
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .find(|v| v["name"] == name.as_str())?;
        let argv = serde_json::from_value(v["argv"].clone()).ok()?;
        // A record without `env` (a stand-in before Issue 626) is an empty environment.
        let env = v["env"].as_object().cloned().unwrap_or_default();
        Some((argv, env))
    }

    /// The payload of the launch's `worker_spawned`, once it is recorded.
    fn spawned(&self) -> Option<Value> {
        let text: String = self
            .conn()?
            .query_row(
                "SELECT payload_json FROM events WHERE kind = 'worker_spawned'
                   AND json_extract(payload_json, '$.spawn_id') = ?1
                 ORDER BY id DESC LIMIT 1",
                [self.spawn_id()],
                |r| r.get(0),
            )
            .ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Start `makibad` with `setting`, queue the run, and read what its launch carried.
    fn launch(&self, setting: &[(&str, &str)]) -> Launched {
        let _d = self.start(setting);
        self.queue();
        let (argv, env) = wait(
            "the launch in MAKIBA_STUB_LAUNCHES",
            Duration::from_secs(60),
            || self.record(),
        );
        let spawned = wait(
            "the launch's worker_spawned",
            Duration::from_secs(60),
            || self.spawned(),
        );
        Launched { argv, env, spawned }
    }
}

fn file_name(arg: &str) -> String {
    Path::new(arg)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[test]
fn a_worker_launch_with_the_setting_begins_with_nice_and_bounds_the_jobs() {
    let org = Org::new();
    let l = org.launch(&[
        ("MAKIBA_WORKER_NICE", "10"),
        ("MAKIBA_WORKER_BUILD_JOBS", "2"),
    ]);
    let argv = &l.argv;
    let log = org.log();
    assert!(argv.len() > 4, "argv {argv:?}; makibad's log: {log}");
    assert!(
        Path::new(&argv[0]).is_absolute() && file_name(&argv[0]) == "nice",
        "argv[0] is not nice by its absolute path: {argv:?}; makibad's log: {log}"
    );
    assert_eq!(argv[1..3], ["-n", "10"], "argv {argv:?}");
    assert_eq!(file_name(&argv[3]), "claude", "argv {argv:?}");
    assert!(
        argv[4..].iter().any(|a| a == "--session-id"),
        "the agent's argv follows nice: {argv:?}"
    );
    assert_eq!(
        l.env.get("CARGO_BUILD_JOBS"),
        Some(&json!("2")),
        "the launch's env: {:?}",
        l.env
    );
    assert_eq!(
        l.spawned["priority"],
        json!({"nice": 10, "build_jobs": 2}),
        "worker_spawned: {}",
        l.spawned
    );
}

#[test]
fn a_worker_launch_without_the_setting_is_as_today() {
    let org = Org::new();
    let l = org.launch(&[]);
    let argv = &l.argv;
    assert_eq!(
        file_name(&argv[0]),
        "claude",
        "argv {argv:?}; makibad's log: {}",
        org.log()
    );
    assert!(
        !argv.iter().any(|a| file_name(a) == "nice"),
        "no nice in {argv:?}"
    );
    assert!(
        !l.env.contains_key("CARGO_BUILD_JOBS"),
        "the launch's env: {:?}",
        l.env
    );
    assert!(
        l.spawned.get("priority").is_none(),
        "worker_spawned: {}",
        l.spawned
    );
    assert_eq!(
        l.spawned["prompt"], "brief",
        "worker_spawned: {}",
        l.spawned
    );
}
