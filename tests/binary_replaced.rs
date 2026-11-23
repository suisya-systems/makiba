//! Issue 340: `makibad` shows when the executable it was started from is replaced under it.
//!
//! Each test copies the `makiba` binary into a temporary folder and starts `makibad` from the
//! copy (on the stand-in base), so that the file can be replaced without touching the test's own
//! binary. A replacement is what a rebuild or an install does: a new file renamed over the path
//! (a running executable cannot be written in place). `status` is `makiba status --json` run with
//! the test's own binary.
//!
//! - A file of other content at the path: one `binary_replaced` anomaly in `status` and one notice
//!   to the secretary, and no second one later; the next start of `makibad` clears it.
//! - No change, or a file of the same content: nothing.
//! - A client of the door built from another build than the running `makibad` says so when a call
//!   fails (the door's socket: these two tests need a Unix socket).

mod common;

use common::makibad;
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const MAKIBA: &str = env!("CARGO_BIN_EXE_makiba");
const SAME_CONTENT: &str = "was replaced by a file of the same content";

/// `makibad::daemon(root)` started from `exe` instead of the test's own binary, such as a copy in
/// a temporary folder: the same arguments and environment (the knowledge variable among them), so
/// it still runs only through the helper's checked `spawn`.
fn daemon_from(exe: &Path, root: &Path) -> makibad::Makibad {
    let mut m = makibad::daemon(root);
    m.with(|c| {
        let mut from = Command::new(exe);
        from.args(c.get_args());
        for (k, v) in c.get_envs() {
            match v {
                Some(v) => from.env(k, v),
                None => from.env_remove(k),
            };
        }
        std::mem::swap(c, &mut from);
    });
    m
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn digest_of(path: &Path) -> String {
    sha256_hex(&std::fs::read(path).unwrap())
}

fn short(digest: &str) -> &str {
    &digest[..12]
}

/// A scratch organization: its folders, and the copy of `makiba` its `makibad` runs from.
struct Org {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    door: bool,
}

impl Org {
    fn new(door: bool) -> Org {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        for d in ["home", "state", "config", "data", "run", "xdg-state", "bin"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        std::fs::set_permissions(root.join("run"), std::fs::Permissions::from_mode(0o700)).unwrap();
        Org {
            _tmp: tmp,
            root,
            door,
        }
    }

    /// `<root>/bin/makiba`, the file `makibad` is started from.
    fn exe(&self) -> PathBuf {
        self.root.join("bin/makiba")
    }

    fn state(&self) -> PathBuf {
        self.root.join("state")
    }

    fn door_path(&self) -> PathBuf {
        self.root.join("run/makiba/door.sock")
    }

    fn key_file(&self) -> PathBuf {
        self.root.join("config/makiba/person.key")
    }

    fn stderr_file(&self) -> PathBuf {
        self.root.join("makibad.err")
    }

    /// Put `bytes` at `<root>/bin/makiba` as an install does: a new executable file renamed over
    /// the path.
    ///
    /// The executable is written by a `cp` child, never by this process (Issue 374). A descriptor
    /// this process opens for writing is inherited by every child another test thread forks until
    /// that child execs, and exec of a file some process holds open for writing fails with
    /// ETXTBSY. A rename does not help: the inherited descriptor still points at the renamed
    /// inode. So the bytes go to a side file this process writes (which is never run), `cp`
    /// writes the executable from it, and once `cp` has exited no process holds a write
    /// descriptor to it. The mode and the rename go by path and open nothing.
    fn install(&self, bytes: &[u8]) {
        let staged = self.root.join("bin/makiba.bytes");
        let new = self.root.join("bin/makiba.new");
        std::fs::write(&staged, bytes).unwrap();
        let cp = Command::new("cp")
            .arg(&staged)
            .arg(&new)
            .stdin(Stdio::null())
            .status()
            .unwrap();
        assert!(
            cp.success(),
            "cp {} {}: {cp}",
            staged.display(),
            new.display()
        );
        std::fs::set_permissions(&new, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::rename(&new, self.exe()).unwrap();
    }

    /// The environment of every `makiba` this test runs: the scratch folder, nothing inherited.
    fn env(&self, c: &mut Command) {
        c.env("HOME", self.root.join("home"))
            .env("MAKIBA_STATE_DIR", self.state())
            .env("XDG_STATE_HOME", self.root.join("xdg-state"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("XDG_RUNTIME_DIR", self.root.join("run"))
            .env("MAKIBA_PERSON_KEY", self.key_file())
            .env("MAKIBA_PORT", "0")
            .env("MAKIBA_BASE", "stub")
            .env("MAKIBA_STUB_SUBSCRIBE_DELAY_MS", "0");
        for v in ["MAKIBA_SHARE_DIR", "MAKIBA_WORKERS_DIR", "MAKIBA_DOCS_DIR"] {
            c.env_remove(v);
        }
        if self.door {
            c.env_remove("MAKIBA_DOOR");
        } else {
            c.env("MAKIBA_DOOR", "none");
        }
    }

    /// Start `makibad` from `<root>/bin/makiba` and wait until it is subscribed and has beaten.
    fn start(&self) -> Daemon {
        let err = std::fs::File::create(self.stderr_file()).unwrap();
        let mut c = daemon_from(&self.exe(), &self.root);
        c.with(|c| self.env(c));
        let child = c
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(err)
            .spawn()
            .unwrap();
        let d = Daemon(child);
        let pid = i64::from(d.0.id());
        wait("makibad to run", Duration::from_secs(30), || {
            let conn = self.db()?;
            conn.query_row(
                "SELECT 1 FROM dispatcher WHERE id = 1 AND pid = ?1 AND subscribed = 1
                   AND heartbeat_at IS NOT NULL",
                [pid],
                |_| Ok(()),
            )
            .ok()
        });
        d
    }

    fn db(&self) -> Option<Connection> {
        Connection::open_with_flags(
            self.state().join("state.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .ok()
    }

    /// `makiba` (the test's own binary) with this organization's environment.
    fn cli(&self, args: &[&str]) -> Output {
        let mut c = Command::new(MAKIBA);
        c.args(args);
        self.env(&mut c);
        c.stdin(Stdio::null()).output().unwrap()
    }

    fn status(&self) -> Value {
        let out = self.cli(&["status", "--json"]);
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "status --json printed no JSON ({e}): {}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        })
    }

    /// The `binary_replaced` entries of `status`'s open anomalies.
    fn shown(&self) -> Vec<Value> {
        self.status()["anomalies"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|a| a["rule"] == json!("binary_replaced"))
            .collect()
    }

    /// The payloads of the events of `kind` whose rule is `binary_replaced`.
    fn events(&self, kind: &str) -> Vec<Value> {
        let conn = self.db().unwrap();
        let mut q = conn
            .prepare(
                "SELECT payload_json FROM events WHERE kind = ?1
                   AND json_extract(payload_json, '$.rule') = 'binary_replaced' ORDER BY id",
            )
            .unwrap();
        q.query_map([kind], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|p| serde_json::from_str(&p.unwrap()).unwrap())
            .collect()
    }

    /// The notices: (message id, event id, sender, recipient, body).
    fn notices(&self) -> Vec<(i64, i64, String, String, String)> {
        let conn = self.db().unwrap();
        let mut q = conn
            .prepare(
                "SELECT id, event_id, sender, recipient, body FROM messages
                 WHERE kind = 'notice' ORDER BY id",
            )
            .unwrap();
        q.query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
    }

    fn stderr(&self) -> String {
        std::fs::read_to_string(self.stderr_file()).unwrap_or_default()
    }

    /// Nothing of a replacement anywhere: no raise, nothing in `status`, no notice.
    fn assert_nothing(&self) {
        assert_eq!(self.events("anomaly_raised"), Vec::<Value>::new());
        assert_eq!(self.shown(), Vec::<Value>::new());
        assert_eq!(self.notices(), Vec::new(), "no notice expected");
    }

    /// The door's answer to a raw request on its socket, or `None` while it cannot be had.
    fn ask_raw(&self, req: &Value) -> Option<Value> {
        let key = std::fs::read_to_string(self.key_file()).ok()?;
        let mut req = req.clone();
        req["key"] = json!(key.trim());
        let mut s = UnixStream::connect(self.door_path()).ok()?;
        s.set_read_timeout(Some(Duration::from_secs(10))).ok()?;
        writeln!(s, "{req}").ok()?;
        let mut line = String::new();
        BufReader::new(s).read_line(&mut line).ok()?;
        serde_json::from_str(&line).ok()
    }

    /// The `build` the door's socket answers with, once it does.
    fn door_build(&self) -> String {
        let req = json!({"v": 1, "verb": "show", "args": {"id": 999999}});
        wait("an answer with build", Duration::from_secs(60), || {
            self.ask_raw(&req)?["build"].as_str().map(String::from)
        })
    }
}

/// A `makibad` that is killed when the test ends, so a failed test leaves nothing running.
struct Daemon(Child);

impl Daemon {
    fn stop(&mut self) {
        let ok = Command::new("kill")
            .args(["-TERM", &self.0.id().to_string()])
            .status()
            .unwrap()
            .success();
        assert!(ok, "kill -TERM failed");
        let until = Instant::now() + Duration::from_secs(15);
        while Instant::now() < until {
            if self.0.try_wait().unwrap().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("makibad did not exit");
    }
}

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

fn original() -> Vec<u8> {
    std::fs::read(MAKIBA).unwrap()
}

/// The original bytes with more at the end: another build, still a runnable binary.
fn other_build() -> Vec<u8> {
    let mut b = original();
    b.extend_from_slice(b"\nreplaced\n");
    b
}

#[test]
fn replaced_binary_shows_once_in_status_and_reaches_the_secretary() {
    let org = Org::new(false);
    org.install(&original());
    let _d = org.start();
    let new = other_build();
    org.install(&new);

    let first = wait("binary_replaced in status", Duration::from_secs(60), || {
        org.shown().into_iter().next()
    });
    assert_eq!(first["task_id"], Value::Null);
    assert!(
        first["since"].as_str().is_some_and(|s| !s.is_empty()),
        "since: {first}"
    );

    // Five looks more: still once.
    std::thread::sleep(Duration::from_secs(5));
    assert_eq!(org.shown().len(), 1, "status: {}", org.status());
    assert_eq!(org.events("anomaly_raised").len(), 1);
    let notices = org.notices();
    assert_eq!(notices.len(), 1, "notices: {notices:?}");
    let (_, event, sender, recipient, body) = &notices[0];
    assert_eq!(sender, "makibad");
    assert_eq!(recipient, "secretary");
    let was = sha256_hex(&original());
    let now = sha256_hex(&new);
    for part in [
        "binary_replaced",
        &org.exe().display().to_string(),
        short(&was),
        short(&now),
        "makiba down",
        "makiba up",
    ] {
        assert!(body.contains(part), "the notice lacks {part:?}: {body}");
    }
    let delivery: i64 = org
        .db()
        .unwrap()
        .query_row(
            "SELECT count(*) FROM event_deliveries
             WHERE source_event_id = ?1 AND recipient = 'secretary'",
            [event],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(delivery, 1, "the notice's delivery to the secretary");
}

#[test]
fn unchanged_binary_shows_nothing() {
    let org = Org::new(false);
    org.install(&original());
    let _d = org.start();
    std::thread::sleep(Duration::from_secs(5));
    org.assert_nothing();
}

#[test]
fn same_content_reinstalled_shows_nothing() {
    let org = Org::new(false);
    org.install(&original());
    let _d = org.start();
    org.install(&original());

    let line = format!("makibad: {} {SAME_CONTENT}", org.exe().display());
    wait("the same-content line", Duration::from_secs(60), || {
        org.stderr().contains(&line).then_some(())
    });
    org.assert_nothing();
}

#[test]
fn a_new_start_clears_it() {
    let org = Org::new(false);
    org.install(&original());
    let mut d = org.start();
    org.install(&other_build());
    wait("binary_replaced in status", Duration::from_secs(60), || {
        org.shown().into_iter().next()
    });
    d.stop();

    // The file on disk is what runs now.
    let _again = org.start();
    wait(
        "binary_replaced gone from status",
        Duration::from_secs(15),
        || org.shown().is_empty().then_some(()),
    );
    assert_eq!(org.events("anomaly_cleared").len(), 1);
}

#[test]
fn a_client_of_another_build_says_so() {
    let org = Org::new(true);
    let other = other_build();
    org.install(&other);
    let _d = org.start();

    let build = org.door_build();
    assert_eq!(build, sha256_hex(&other));
    let mine = digest_of(Path::new(MAKIBA));
    assert_ne!(build, mine);

    let out = org.cli(&["show", "999999"]);
    assert!(!out.status.success(), "show 999999 must fail");
    let err = String::from_utf8_lossy(&out.stderr);
    for part in [short(&build), short(&mine), "makiba down", "makiba up"] {
        assert!(err.contains(part), "the error lacks {part:?}: {err}");
    }
}

#[test]
fn a_client_of_the_same_build_says_nothing() {
    let org = Org::new(true);
    org.install(&original());
    let _d = org.start();

    let build = org.door_build();
    assert_eq!(build, digest_of(Path::new(MAKIBA)));

    let out = org.cli(&["show", "999999"]);
    assert!(!out.status.success(), "show 999999 must fail");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!err.contains("another build"), "the error: {err}");
}
