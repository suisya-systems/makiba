//! Issue 352: a launch after the executable was replaced names the path on disk, not a deleted
//! file. On Linux `current_exe()` says `<path> (deleted)` once the file at the path was replaced
//! under a running process; `makibad` records its path once at its start and every launch path
//! uses that record.
//!
//! `makibad` runs from a copy of the `makiba` binary in a temporary folder (as in
//! tests/binary_replaced.rs), on the stand-in base, which writes each launch's argv to the file
//! `MAKIBA_STUB_LAUNCHES` names. The copy is replaced while `makibad` runs, then a code run of the
//! `claude_code` agent is queued, with a stand-in `claude` on the path that is never started. The
//! launch's channel-server command, the secret scanner's git hooks and the role hooks' entry point
//! `<share>/makiba` all name the path.

#![cfg(target_os = "linux")]

mod common;

use common::makibad;
use makiba::db::writer::{RunUpsert, StateWriter};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const MAKIBA: &str = env!("CARGO_BIN_EXE_makiba");
const TASK: &str = "t352";

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
/// `bin/` with the copy of `makiba` its `makibad` runs from and a stand-in `claude`.
struct Org {
    _dir: tempfile::TempDir,
    root: PathBuf,
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
        common::exe::copy(Path::new(MAKIBA), &root.join("bin/makiba"));
        common::exe::write(
            &root.join("bin/claude"),
            "#!/bin/sh\necho 'stand-in claude' >&2\nexit 1\n",
        );
        let w = StateWriter::open(&root.join("state/state.db")).unwrap();
        w.ensure_project("p", None, None).unwrap();
        Org { _dir: dir, root }
    }

    /// `<root>/bin/makiba`, the file `makibad` is started from.
    fn exe(&self) -> PathBuf {
        self.root.join("bin/makiba")
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

    /// `makibad` started from `<root>/bin/makiba` (the helper's command with its program swapped),
    /// once it is subscribed and has beaten.
    fn start(&self) -> Daemon {
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
            .env_remove("MAKIBA_FAKE_AGENT");
        let exe = self.exe();
        m.with(|c| {
            let mut from = Command::new(&exe);
            from.args(c.get_args());
            for (k, v) in c.get_envs() {
                match v {
                    Some(v) => from.env(k, v),
                    None => from.env_remove(k),
                };
            }
            std::mem::swap(c, &mut from);
        });
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
                    format!("spawn-{TASK}"),
                    self.root.join("workers/p").join(TASK).to_string_lossy()
                ],
            )
            .unwrap();
    }

    /// The argv of the stand-in base's launch named `name`, once there is one.
    fn launched(&self, name: &str) -> Option<Vec<String>> {
        let text = std::fs::read_to_string(self.launches()).ok()?;
        text.lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .find(|v| v["name"] == name)
            .and_then(|v| serde_json::from_value(v["argv"].clone()).ok())
    }
}

#[test]
fn a_claude_code_launch_after_the_binary_was_replaced_names_the_path_on_disk() {
    let org = Org::new();
    let d = org.start();
    let exe = org.exe();
    let exe_s = exe.display().to_string();

    // A rebuild or an install: a new file renamed over the path.
    let mut other = std::fs::read(MAKIBA).unwrap();
    other.extend_from_slice(b"\nreplaced\n");
    common::exe::write(&exe, &other);

    // The precondition: the running makibad's executable is now a deleted file.
    let running = std::fs::read_link(format!("/proc/{}/exe", d.0.id())).unwrap();
    assert_eq!(
        running,
        PathBuf::from(format!("{exe_s} (deleted)")),
        "the running makibad's executable"
    );

    org.queue();
    let name = format!("w-{TASK}");
    let argv = wait(
        "the launch in MAKIBA_STUB_LAUNCHES",
        Duration::from_secs(60),
        || org.launched(&name),
    );

    // The channel server's command.
    let at = argv
        .iter()
        .position(|a| a == "--mcp-config")
        .unwrap_or_else(|| panic!("no --mcp-config in {argv:?}"));
    let config: Value = serde_json::from_str(&argv[at + 1]).unwrap();
    assert_eq!(
        config["mcpServers"]["makiba-channel"]["command"],
        Value::String(exe_s.clone()),
        "the channel server's command; makibad's log: {}",
        org.log()
    );

    // The secret scanner's git hooks.
    let hooks = org.state().join("git-hooks");
    let pre_commit = std::fs::read_to_string(hooks.join("pre-commit"))
        .unwrap_or_else(|e| panic!("{}: {e}", hooks.join("pre-commit").display()));
    let line = format!("m='{exe_s}'");
    assert!(
        pre_commit.lines().any(|l| l == line),
        "pre-commit lacks the line {line:?}:\n{pre_commit}"
    );
    for entry in std::fs::read_dir(&hooks).unwrap() {
        let p = entry.unwrap().path();
        let text = std::fs::read_to_string(&p).unwrap_or_default();
        assert!(
            !text.contains("(deleted)"),
            "{} names a deleted file:\n{text}",
            p.display()
        );
    }

    // The role hooks' entry point.
    let link = org.root.join("share/makiba");
    let meta =
        std::fs::symlink_metadata(&link).unwrap_or_else(|e| panic!("{}: {e}", link.display()));
    assert!(
        meta.file_type().is_symlink(),
        "{} is not a link",
        link.display()
    );
    assert_eq!(
        std::fs::read_link(&link).unwrap(),
        exe,
        "the entry point's target"
    );
}
