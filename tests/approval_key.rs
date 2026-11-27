//! Issue 590 (acceptance approval 3/5, `docs/design/acceptance-approval.md` sections 3 and 8): the
//! approval key, the ed25519 key `makiba merge approve` will sign an acceptance approval with.
//!
//! - `makiba up` makes it with `ssh-keygen` at `$MAKIBA_APPROVAL_KEY` (default `approval.key`
//!   beside the person key), mode 0600 in a folder of mode 0700, through a staging folder
//!   `<key>.new`, and prints its line for `.github/acceptance-approvers`. It does not make it while
//!   a `makibad` runs or while a live agent was launched without its deny rule.
//! - `makiba approval-key` prints that line and changes nothing.
//! - Every role's settings deny the key and its staging folder, the static role files at the
//!   default place and the settings written at launch at the resolved place, default or custom.
//! - The reader's read boundary (Issue 262) and the read surface (Issue 168) do not show it.
//!
//! One test: it changes this process's environment in turn and runs `makibad` in process, as
//! `tests/base_config_denied.rs` does. `ssh-keygen` is the system's.

mod common;

use common::makibad;
use makiba::base::stub::Stub;
use makiba::daemon::door::{Door, issue_key};
use makiba::daemon::launch::add_launch;
use makiba::db::writer::{RunUpsert, StateWriter};
use makiba::spec::{ErrorKind, Role};
use serde_json::{Value, json};
use std::fs::{self, OpenOptions, Permissions};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const MAKIBA: &str = env!("CARGO_BIN_EXE_makiba");
/// The allowed-signers line's head (docs/design/acceptance-approval.md section 3).
const SIGNER: &str = "makiba-owner namespaces=\"makiba-acceptance-approval\" ";
const FINDINGS: &str = "FINDINGS: blocker=0 major=0 minor=0";
const WAIT: Duration = Duration::from_secs(30);
const THE_WAY: &str = "run makiba down --secretary, then makiba up";

// ---- small helpers ---------------------------------------------------------------------------

fn mode(p: &Path) -> u32 {
    fs::metadata(p).unwrap().permissions().mode() & 0o777
}

fn chmod(p: &Path, m: u32) {
    fs::set_permissions(p, Permissions::from_mode(m)).unwrap(); // not an executable: the key file (0644) and its folder (0755), run by nothing
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn temp_root() -> PathBuf {
    fs::canonicalize(tempfile::tempdir().unwrap().keep()).unwrap()
}

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

fn wait<T>(mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let until = Instant::now() + WAIT;
    while Instant::now() < until {
        if let Some(v) = f() {
            return Some(v);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    None
}

/// An ed25519 key made by the system's `ssh-keygen` at `path`, with no `.pub` left beside it.
fn make_key(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let out = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-C", "test", "-f"])
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(out.status.success(), "ssh-keygen: {}", text(&out));
    let _ = fs::remove_file(format!("{}.pub", path.display()));
}

/// The public half of the private key at `path`: its type and base64, without the comment.
fn public_of(path: &Path) -> String {
    let out = Command::new("ssh-keygen")
        .arg("-y")
        .arg("-f")
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(out.status.success(), "ssh-keygen -y: {}", text(&out));
    let line = String::from_utf8_lossy(&out.stdout).to_string();
    line.split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ")
}

/// The base64 lines of a key file that are long enough to be masked (16 characters or more).
fn body_lines(key: &str) -> Vec<String> {
    key.lines()
        .filter(|l| !l.starts_with("-----") && l.len() >= 16)
        .map(String::from)
        .collect()
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

/// `what`'s settings deny reading and writing the file `key` and the folder `staging`, for the
/// sandbox and for the file tools.
fn assert_denies(what: &str, v: &Value, key: &str, staging: &str) {
    let fs_ = &v["sandbox"]["filesystem"];
    for list in ["denyRead", "denyWrite"] {
        let l = strings(&fs_[list]);
        for p in [key, staging] {
            assert!(
                l.iter().any(|e| e == p),
                "{what}: sandbox.filesystem.{list} does not name {p}: {v}"
            );
        }
    }
    let rule = |p: &str| {
        if p.starts_with('/') {
            format!("/{p}")
        } else {
            p.to_string()
        }
    };
    let deny = strings(&v["permissions"]["deny"]);
    for r in [
        format!("Read({})", rule(key)),
        format!("Edit({})", rule(key)),
        format!("Read({}/**)", rule(staging)),
        format!("Edit({}/**)", rule(staging)),
    ] {
        assert!(
            deny.contains(&r),
            "{what}: permissions.deny has no {r}: {v}"
        );
    }
}

// ---- `makiba up` with a stand-in base ---------------------------------------------------------

/// What `claude --version` prints for the lowest supported version.
fn in_range() -> String {
    let (a, b, c) = makiba::agents::version::LOWEST;
    format!("{a}.{b}.{c} (Claude Code)")
}

fn script(path: &Path, body: &str) {
    common::exe::write(path, format!("#!/bin/sh\n{body}\n"));
}

/// An organization's folders for `makiba up`: a stand-in base `hc` and a stand-in `claude`.
struct Up {
    root: PathBuf,
}

impl Up {
    fn new() -> Up {
        let root = temp_root();
        let bin = root.join("bin");
        fs::create_dir_all(&bin).unwrap();
        script(&bin.join("claude"), &format!("echo '{}'", in_range()));
        script(&bin.join("hc"), "exit 0");
        Up { root }
    }

    fn path(&self, p: &str) -> PathBuf {
        self.root.join(p)
    }

    fn state(&self) -> PathBuf {
        self.path("state/makiba")
    }

    fn default_key(&self) -> PathBuf {
        self.path("config/makiba/approval.key")
    }

    fn env(&self, c: &mut Command) {
        c.env_clear()
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.path("bin").display()),
            )
            .env("HOME", self.path("home"))
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("XDG_STATE_HOME", self.path("state"))
            .env("HC_BIN", self.path("bin/hc"))
            .env("MAKIBA_BASE", "stub")
            .env("MAKIBA_DOOR", "none")
            .env("MAKIBA_PORT", "0")
            .stdin(Stdio::null());
    }

    fn up(&self, extra: &[(&str, &str)]) -> Output {
        let mut c = makibad::up(&self.root);
        c.arg("--no-secretary");
        c.env_clear()
            .env(makibad::KNOWLEDGE_ENV, makibad::knowledge(&self.root))
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.path("bin").display()),
            )
            .env("HOME", self.path("home"))
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("XDG_STATE_HOME", self.path("state"))
            .env("HC_BIN", self.path("bin/hc"))
            .env("MAKIBA_BASE", "stub")
            .env("MAKIBA_DOOR", "none")
            .env("MAKIBA_PORT", "0");
        for (k, v) in extra {
            c.env(k, v);
        }
        c.output().unwrap()
    }

    fn approval_key(&self, extra: &[(&str, &str)]) -> Output {
        let mut c = Command::new(MAKIBA);
        c.arg("approval-key");
        self.env(&mut c);
        for (k, v) in extra {
            c.env(k, v);
        }
        c.output().unwrap()
    }

    fn daemon_pid(&self) -> Option<i32> {
        fs::read_to_string(self.state().join("makibad.lock"))
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    /// End the `makibad` an `up` started, and wait until its lock is free.
    fn stop_daemon(&self) {
        let Some(pid) = self.daemon_pid() else {
            return;
        };
        unsafe { libc::kill(pid, libc::SIGTERM) };
        let until = Instant::now() + Duration::from_secs(10);
        while Instant::now() < until && unsafe { libc::kill(pid, 0) } == 0 {
            std::thread::sleep(Duration::from_millis(50));
        }
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(self.state().join("makibad.lock"))
            .unwrap();
        assert!(
            wait(|| lock.try_lock().ok()).is_some(),
            "makibad {pid} still holds its lock"
        );
    }

    /// A live agent key of `role` whose launch's settings file was written without the approval
    /// key's deny rule (a launch of a makiba from before Issue 590).
    fn plant_old_agent(&self, role: Role, agent_id: &str, spawn: &str) {
        fs::create_dir_all(self.state().join("settings")).unwrap();
        let w = StateWriter::open(&self.state().join("state.db")).unwrap();
        issue_key(&w, role, agent_id, None, Some(spawn)).unwrap();
        w.conn()
            .execute(
                "INSERT INTO run_launch (role, spawn_id, kind, agent, brief, worktree, stage)
                 VALUES (?1, ?2, 'document', 'fake', 'the brief', ?3, 'verified')",
                rusqlite::params![role.as_str(), spawn, self.path(agent_id).to_string_lossy()],
            )
            .unwrap();
        fs::write(
            self.state()
                .join(format!("settings/{}-{spawn}.json", role.as_str())),
            json!({"sandbox": {"filesystem": {"denyRead": ["/nothing"], "denyWrite": []}},
                   "permissions": {"deny": []}})
            .to_string(),
        )
        .unwrap();
    }

    fn revoke(&self, spawn: &str) {
        let w = StateWriter::open(&self.state().join("state.db")).unwrap();
        w.conn()
            .execute(
                "UPDATE agents SET revoked_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
                 WHERE spawn_id = ?1",
                [spawn],
            )
            .unwrap();
    }
}

impl Drop for Up {
    fn drop(&mut self) {
        if let Some(pid) = self.daemon_pid() {
            unsafe { libc::kill(pid, libc::SIGTERM) };
        }
    }
}

/// The line `up` prints for a key it made or kept.
fn key_line(key: &Path, how: &str) -> String {
    format!(
        "approval key: {} ({how}); its line for .github/acceptance-approvers: {SIGNER}{}",
        key.display(),
        public_of(key)
    )
}

fn has_line(out: &Output, line: &str) -> bool {
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .any(|l| l == line)
}

// ---- 1 to 7: making the key ------------------------------------------------------------------

fn up_makes_the_key_and_approval_key_prints_it() {
    let s = Up::new();
    let key = s.default_key();
    let staging = s.path("config/makiba/approval.key.new");

    // 1. A fresh `up` makes the key at the default place.
    let out = s.up(&[]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(mode(&key), 0o600, "{}", text(&out));
    assert_eq!(mode(key.parent().unwrap()), 0o700);
    let first = fs::read_to_string(&key).unwrap();
    // The armor line, built here so that no line of this file is shaped like a private key.
    let armor = ["-----BEGIN OPENSSH", " PRIVATE KEY-----"].concat();
    assert!(first.starts_with(&armor), "{first:.40}");
    assert!(!s.path("config/makiba/approval.key.pub").exists());
    assert!(
        !staging.exists() || fs::read_dir(&staging).unwrap().count() == 0,
        "a copy of the key is left in {}",
        staging.display()
    );
    let created = key_line(&key, "created");
    assert!(
        has_line(&out, &created),
        "no line {created:?}: {}",
        text(&out)
    );

    // 2. A second `up` keeps it, makes it private again, and removes a leftover of a crash.
    chmod(&key, 0o644);
    chmod(key.parent().unwrap(), 0o755);
    fs::create_dir_all(&staging).unwrap();
    fs::write(staging.join("key-1"), &first).unwrap();
    let out = s.up(&[]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(fs::read_to_string(&key).unwrap(), first);
    assert_eq!(mode(&key), 0o600);
    assert_eq!(mode(key.parent().unwrap()), 0o700);
    assert!(
        !staging.join("key-1").exists(),
        "the leftover is still there"
    );
    let kept = key_line(&key, "kept");
    assert!(has_line(&out, &kept), "no line {kept:?}: {}", text(&out));

    // 3. `makiba approval-key` prints the line and changes nothing.
    let out = s.approval_key(&[]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("{SIGNER}{}\n", public_of(&key))
    );
    assert_eq!(fs::read_to_string(&key).unwrap(), first);
    assert_eq!(mode(&key), 0o600);
    let absent = s.path("absent/approval.key");
    let out = s.approval_key(&[("MAKIBA_APPROVAL_KEY", absent.to_str().unwrap())]);
    assert_eq!(
        out.status.code(),
        Some(ErrorKind::NotFound.exit_code()),
        "{}",
        text(&out)
    );
    assert!(!absent.exists(), "approval-key made a key");
    assert!(
        !absent.parent().unwrap().exists(),
        "approval-key made a folder"
    );

    // 4. A custom place, with no makibad running and no live agent.
    s.stop_daemon();
    let custom = s.path("elsewhere/keys/approval.key");
    let out = s.up(&[("MAKIBA_APPROVAL_KEY", custom.to_str().unwrap())]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(mode(&custom), 0o600, "{}", text(&out));
    assert_eq!(mode(custom.parent().unwrap()), 0o700);
    assert!(
        has_line(&out, &key_line(&custom, "created")),
        "{}",
        text(&out)
    );
    assert_eq!(fs::read_to_string(&key).unwrap(), first);
}

fn up_does_not_make_the_key_beside_an_agent_that_may_read_it() {
    // 5. A live agent launched without the deny rule: no key; once it is gone, the key.
    let s = Up::new();
    s.plant_old_agent(Role::Secretary, "secretary", "sec-old");
    let out = s.up(&[]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        !s.default_key().exists(),
        "the key was made: {}",
        text(&out)
    );
    let said = text(&out);
    assert!(
        said.contains(&format!(
            "approval key: not made: agents launched without its deny rule: secretary; {THE_WAY}"
        )),
        "{said}"
    );
    s.stop_daemon();
    s.revoke("sec-old");
    let out = s.up(&[]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(s.default_key().exists(), "no key: {}", text(&out));
    assert!(
        has_line(&out, &key_line(&s.default_key(), "created")),
        "{}",
        text(&out)
    );

    // 7. The key exists and a live agent was launched without its deny rule: a warning.
    s.stop_daemon();
    s.plant_old_agent(Role::Curator, "curator", "cur-old");
    let out = s.up(&[]);
    assert!(out.status.success(), "{}", text(&out));
    let said = text(&out);
    assert!(
        said.contains(&format!(
            "approval key: warning: agents launched without its deny rule: curator; {THE_WAY}"
        )),
        "{said}"
    );
    s.stop_daemon();

    // 6. A makibad running (this test holds its lock): no key.
    let r = Up::new();
    fs::create_dir_all(r.state()).unwrap();
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(r.state().join("makibad.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    let out = r.up(&[]);
    let said = text(&out);
    assert!(!r.default_key().exists(), "the key was made: {said}");
    assert!(
        said.contains(&format!(
            "approval key: not made: makibad is running; {THE_WAY}"
        )),
        "{said}"
    );
    drop(lock);
}

// ---- 8: the static role files -----------------------------------------------------------------

fn the_role_files_deny_the_default_place() {
    for role in ["secretary", "worker", "curator"] {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("roles/{role}/settings.json"));
        let v: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_denies(
            role,
            &v,
            "~/.config/makiba/approval.key",
            "~/.config/makiba/approval.key.new",
        );
    }
}

// ---- 9: the settings written at launch --------------------------------------------------------

/// `makibad` in process prepares a secretary, a worker (a document run) and a curator in the
/// state folder `state`; returns their settings once all are written, and stops it.
fn prepared_settings(root: &Path, state: &Path) -> Vec<(String, Value)> {
    fs::create_dir_all(state).unwrap();
    for f in ["secretary", "curator", "docs/t2"] {
        fs::create_dir_all(root.join(f)).unwrap();
    }
    let (s, c) = {
        let w = StateWriter::open(&state.join("state.db")).unwrap();
        w.ensure_project("p", None, None).unwrap();
        w.upsert_run(&RunUpsert {
            task_id: "t2",
            project_slug: "p",
            status: Some("queued"),
            ..Default::default()
        })
        .unwrap();
        w.conn()
            .execute(
                "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree)
                 VALUES ((SELECT id FROM runs WHERE task_id = 't2'), 'spawn-t2', 'document',
                         'fake', 'the brief', ?1)",
                [root.join("docs/t2").to_string_lossy()],
            )
            .unwrap();
        let s = add_launch(
            &w,
            Role::Secretary,
            &root.join("secretary"),
            "b",
            None,
            "fake",
        )
        .unwrap();
        let c = add_launch(&w, Role::Curator, &root.join("curator"), "b", None, "fake").unwrap();
        (s, c)
    };
    let stop = Arc::new(AtomicBool::new(false));
    let daemon = {
        let (dir, stop) = (state.to_path_buf(), stop.clone());
        std::thread::spawn(move || {
            let _ = makibad::run_until(&dir, &|| Box::new(Stub::new()), stop);
        })
    };
    let files = [
        ("worker", state.join("settings/t2.json")),
        (
            "secretary",
            state.join(format!("settings/secretary-{s}.json")),
        ),
        ("curator", state.join(format!("settings/curator-{c}.json"))),
    ];
    let read: Vec<(String, Value)> = files
        .iter()
        .map(|(name, file)| {
            let v = wait(|| {
                serde_json::from_str::<Value>(&fs::read_to_string(file).ok()?)
                    .ok()
                    .filter(|v| v["sandbox"]["filesystem"]["denyRead"].is_array())
            })
            .unwrap_or_else(|| panic!("{name}: no settings at {} within {WAIT:?}", file.display()));
            (name.to_string(), v)
        })
        .collect();
    stop.store(true, Ordering::SeqCst);
    daemon.join().unwrap();
    read
}

fn the_settings_of_every_role_deny_the_resolved_place() {
    let root = temp_root();
    for f in ["share", "tmp", "workers", "docs", "repos", "a", "home"] {
        fs::create_dir_all(root.join(f)).unwrap();
    }
    // SAFETY: this binary's one test runs its steps in turn; no other thread reads these now.
    unsafe {
        std::env::set_var("MAKIBA_DOOR", "none");
        std::env::set_var(
            "MAKIBA_FAKE_AGENT",
            json!(["true", root.join("record")]).to_string(),
        );
        std::env::set_var("MAKIBA_VERIFY_TIMEOUT_MS", "600000");
        std::env::set_var("MAKIBA_SHARE_DIR", root.join("share"));
        std::env::set_var("MAKIBA_PERSON_KEY", root.join("a/person.key"));
        std::env::set_var("MAKIBA_GATE_KEY", root.join("a/gate.key"));
        std::env::set_var("MAKIBA_WORKERS_DIR", root.join("workers"));
        std::env::set_var("MAKIBA_DOCS_DIR", root.join("docs"));
        std::env::set_var("MAKIBA_REPOS_DIR", root.join("repos"));
        std::env::set_var("TMPDIR", root.join("tmp"));
        std::env::remove_var("MAKIBA_APPROVAL_KEY");
    }
    // At the default place, beside the person key.
    let key = root.join("a/approval.key").display().to_string();
    let staging = root.join("a/approval.key.new").display().to_string();
    for (name, v) in prepared_settings(&root, &root.join("state1")) {
        assert_denies(&format!("{name} (default place)"), &v, &key, &staging);
    }
    // At a custom place.
    let custom = root.join("b/custom/approval.key");
    // SAFETY: as above; the first daemon has stopped.
    unsafe { std::env::set_var("MAKIBA_APPROVAL_KEY", &custom) };
    let key = custom.display().to_string();
    let staging = format!("{key}.new");
    for (name, v) in prepared_settings(&root, &root.join("state2")) {
        assert_denies(&format!("{name} (custom place)"), &v, &key, &staging);
    }
}

// ---- 10 and 11: the reader -------------------------------------------------------------------

/// A stand-in program at `path`: it appends its arguments (one per line, then `--`) to
/// `<path>.args`, answers `login status` with `login`, and else runs `body`.
fn stand_in(path: &Path, login: i32, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$@\" -- >> \"$0.args\"\nif [ \"$1\" = login ]; then exit {login}; fi\n{body}\n"
    );
    common::exe::write(path, script);
}

/// A stand-in boundary at `path`: each call writes its arguments, NUL-separated, to
/// `<path>.argv.<pid>` and then runs what follows the first `--`.
fn recording_boundary(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    common::exe::write(
        path,
        "#!/bin/sh\nprintf '%s\\0' \"$@\" > \"$0.argv.$$\"\nwhile [ $# -gt 0 ]; do a=$1; shift; [ \"$a\" = -- ] && break; done\nexec \"$@\"\n",
    );
}

fn calls(path: &Path) -> Vec<Vec<String>> {
    let dir = path.parent().unwrap();
    let prefix = format!("{}.argv.", path.file_name().unwrap().to_string_lossy());
    let mut out = vec![];
    for e in fs::read_dir(dir).unwrap() {
        let e = e.unwrap();
        if e.file_name().to_string_lossy().starts_with(&prefix) {
            let bytes = fs::read(e.path()).unwrap();
            out.push(
                bytes
                    .split(|b| *b == 0)
                    .filter(|a| !a.is_empty())
                    .map(|a| String::from_utf8_lossy(a).into_owned())
                    .collect(),
            );
        }
    }
    out
}

fn command(argv: &[String]) -> &[String] {
    let i = argv
        .iter()
        .position(|a| a == "--")
        .unwrap_or_else(|| panic!("no -- in {argv:?}"));
    &argv[i + 1..]
}

/// The calls that started a reader: every call but the boundary's own check (`-- true`).
fn reader_calls(path: &Path) -> Vec<Vec<String>> {
    calls(path)
        .into_iter()
        .filter(|a| command(a).first().map(String::as_str) != Some("true"))
        .collect()
}

enum Mount {
    Bind { src: PathBuf, rw: bool },
    Cover,
}

/// The mounts of a bubblewrap argument list, in order. An option this reading does not know fails
/// the test, so that no option can hide a mount.
fn mounts(argv: &[String]) -> Vec<(PathBuf, Mount)> {
    let mut out = vec![];
    let mut i = 0;
    while i < argv.len() {
        let a = argv[i].as_str();
        let arg = |n: usize| -> &str {
            argv.get(i + n)
                .unwrap_or_else(|| panic!("{a} without its value in {argv:?}"))
        };
        match a {
            "--" => break,
            "--unshare-pid" | "--unshare-ipc" | "--unshare-uts" | "--unshare-user"
            | "--unshare-cgroup" | "--die-with-parent" | "--new-session" => i += 1,
            "--chdir" => i += 2,
            "--setenv" => i += 3,
            "--proc" | "--dev" | "--tmpfs" | "--dir" => {
                out.push((PathBuf::from(arg(1)), Mount::Cover));
                i += 2;
            }
            "--symlink" => {
                out.push((PathBuf::from(arg(2)), Mount::Cover));
                i += 3;
            }
            "--ro-bind" | "--ro-bind-try" | "--bind" | "--bind-try" => {
                let (src, dst) = (PathBuf::from(arg(1)), PathBuf::from(arg(2)));
                let m = if src == Path::new("/dev/null") {
                    Mount::Cover
                } else {
                    Mount::Bind {
                        src,
                        rw: a.starts_with("--bind"),
                    }
                };
                out.push((dst, m));
                i += 3;
            }
            other => panic!("an option this test does not read: {other} in {argv:?}"),
        }
    }
    out
}

/// What the reader sees at `p`: the source and whether it may write, or `None`.
fn visible(argv: &[String], p: &Path) -> Option<(PathBuf, bool)> {
    for (dst, m) in mounts(argv).into_iter().rev() {
        if let Ok(rel) = p.strip_prefix(&dst) {
            return match m {
                Mount::Bind { src, rw } => Some((
                    if rel.as_os_str().is_empty() {
                        src
                    } else {
                        src.join(rel)
                    },
                    rw,
                )),
                Mount::Cover => None,
            };
        }
    }
    None
}

fn codex_stream(text: &str) -> String {
    let c = json!({"type": "item.completed", "item": {"type": "command_execution", "command": "cat MAKIBA_REVIEW.diff", "exit_code": 0, "status": "completed"}});
    let m = json!({"type": "item.completed", "item": {"type": "agent_message", "text": text}});
    format!("{c}\n{m}\n")
}

fn claude_stream(text: &str) -> String {
    let a = json!({"type": "assistant", "message": {"content": [{"type": "tool_use", "id": "u1", "name": "Read", "input": {"file_path": "MAKIBA_REVIEW.diff"}}]}});
    let u = json!({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "u1", "is_error": false, "content": "x"}]}});
    let r = json!({"type": "result", "subtype": "success", "is_error": false, "result": text});
    format!("{a}\n{u}\n{r}\n")
}

/// A door over an `in_use` code run of `task` with its own repository, and the worker's key.
fn review_door(
    root: &Path,
    task: &str,
    other: &Path,
    same: &Path,
    boundary: &Path,
) -> (Door, String) {
    let state = root.join(format!("state-{task}"));
    fs::create_dir_all(&state).unwrap();
    let branch = format!("makiba/{task}");
    let worktree = root.join("workers/p").join(task);
    makiba::daemon::run_repo::create(
        &root.join("repos/p"),
        &makiba::daemon::run_repo::dir(&state.join("settings"), task),
        &worktree,
        &branch,
        "main",
        &state.join("git-hooks"),
    )
    .unwrap();
    let w = StateWriter::open(&state.join("state.db")).unwrap();
    let run = w
        .upsert_run(&RunUpsert {
            task_id: task,
            project_slug: "p",
            status: Some("in_use"),
            branch: Some(&branch),
            ..Default::default()
        })
        .unwrap();
    let spawn = format!("{task}-s1");
    w.conn()
        .execute(
            "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree, stage)
             VALUES (?1, ?2, 'code', 'fake', 'the brief', ?3, 'verified')",
            rusqlite::params![run, spawn, worktree.to_string_lossy()],
        )
        .unwrap();
    let worker = issue_key(
        &w,
        Role::Worker,
        &format!("w-{task}"),
        Some(run),
        Some(&spawn),
    )
    .unwrap();
    fs::write(worktree.join("a.txt"), "a\n").unwrap();
    git(&worktree, &["add", "a.txt"]);
    git(&worktree, &["commit", "-q", "-m", "a"]);
    drop(w);
    let door = Door::new(
        StateWriter::open(&state.join("state.db")).unwrap(),
        Arc::new(Stub::new()),
        "fake".into(),
    )
    .with_review_programs(other.to_path_buf(), same.to_path_buf())
    .with_review_boundary(boundary.to_path_buf());
    (door, worker)
}

fn the_reader_sees_no_approval_key() {
    let root = temp_root();
    let repo = root.join("repos/p");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    fs::write(repo.join("file.txt"), "first\n").unwrap();
    git(&repo, &["add", "file.txt"]);
    git(&repo, &["commit", "-q", "-m", "first"]);
    let origin = root.join("origin.git");
    git(&root, &["init", "-q", "--bare", &origin.to_string_lossy()]);
    git(
        &repo,
        &["remote", "add", "origin", &origin.to_string_lossy()],
    );
    git(&repo, &["push", "-q", "origin", "main"]);
    git(&repo, &["fetch", "-q", "origin"]);
    let person = root.join("config/makiba/person.key");
    fs::create_dir_all(person.parent().unwrap()).unwrap();
    fs::write(&person, format!("{}\n", "6e".repeat(32))).unwrap();
    fs::create_dir_all(root.join("home")).unwrap();
    // The reader's program lies in a folder that also holds the approval key.
    let d = root.join("reader-package");
    let key = d.join("approval.key");
    make_key(&key);
    fs::create_dir_all(d.join("approval.key.new")).unwrap();
    fs::write(d.join("approval.key.new/key-9"), "a staged copy\n").unwrap();
    fs::write(d.join("decoy.txt"), "beside the key\n").unwrap();
    // SAFETY: this binary's one test runs its steps in turn; no other thread reads these now.
    unsafe {
        std::env::set_var("MAKIBA_WORKERS_DIR", root.join("workers"));
        std::env::set_var("MAKIBA_DOCS_DIR", root.join("docs"));
        std::env::set_var("MAKIBA_REPOS_DIR", root.join("repos"));
        std::env::set_var("MAKIBA_PERSON_KEY", &person);
        std::env::set_var("MAKIBA_GATE_KEY", root.join("config/makiba/gate.key"));
        std::env::set_var("MAKIBA_APPROVAL_KEY", &key);
        std::env::set_var("HOME", root.join("home"));
        for v in [
            "CODEX_HOME",
            "CLAUDE_CONFIG_DIR",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_STATE_HOME",
            "XDG_CACHE_HOME",
        ] {
            std::env::remove_var(v);
        }
    }

    // 10. The other reader, its program in the key's folder.
    let other = d.join("bin/codex");
    let boundary = root.join("bin/boundary/bwrap");
    let same = root.join("bin/same/claude");
    stand_in(&other, 0, "cat \"$0.out\"");
    stand_in(&same, 0, "cat > /dev/null; exit 1");
    fs::write(format!("{}.out", other.display()), codex_stream(FINDINGS)).unwrap();
    recording_boundary(&boundary);
    let (door, worker) = review_door(&root, "ak1", &other, &same, &boundary);
    let v = door
        .handle(Some(&worker), "review", json!({"kind": "diff"}), None, true)
        .unwrap();
    assert_eq!(v["verdict"], "passed", "{v}");
    assert_eq!(v["reader"], "codex", "{v}");
    let started = reader_calls(&boundary);
    assert_eq!(started.len(), 1, "{started:?}");
    let argv = &started[0];
    for p in [
        key.clone(),
        d.join("approval.key.new/key-9"),
        d.join("decoy.txt"),
    ] {
        assert_eq!(
            visible(argv, &p),
            None,
            "{} is visible to the reader: {argv:?}",
            p.display()
        );
    }
    assert!(
        !mounts(argv)
            .iter()
            .any(|(dst, m)| *dst == d && matches!(m, Mount::Bind { .. })),
        "the folder holding the key is bound: {argv:?}"
    );
    assert_eq!(
        visible(argv, &other),
        Some((other.clone(), false)),
        "{argv:?}"
    );

    // The fallback reader's settings deny the key and its staging folder.
    let boundary2 = root.join("bin/boundary2/bwrap");
    let same2 = root.join("bin/same2/claude");
    stand_in(
        &same2,
        0,
        "while [ $# -gt 0 ]; do if [ \"$1\" = --settings ]; then cp \"$2\" \"$0.settings\"; fi; shift; done\ncat > /dev/null; cat \"$0.out\"",
    );
    fs::write(format!("{}.out", same2.display()), claude_stream(FINDINGS)).unwrap();
    recording_boundary(&boundary2);
    let (door2, worker2) = review_door(
        &root,
        "ak2",
        &root.join("bin/nowhere/codex"),
        &same2,
        &boundary2,
    );
    let v = door2
        .handle(
            Some(&worker2),
            "review",
            json!({"kind": "diff"}),
            None,
            true,
        )
        .unwrap();
    assert_eq!(v["verdict"], "passed", "{v}");
    assert_eq!(v["reader"], "fresh-session", "{v}");
    let settings: Value = serde_json::from_str(
        &fs::read_to_string(format!("{}.settings", same2.display()))
            .expect("the fallback reader was given no settings file"),
    )
    .unwrap();
    let deny_read = strings(&settings["sandbox"]["filesystem"]["denyRead"]);
    for p in [key.clone(), d.join("approval.key.new")] {
        let p = p.display().to_string();
        assert!(
            deny_read.contains(&p),
            "the fallback's settings do not deny {p}: {settings}"
        );
    }

    // 11. A reader's answer carrying the key, whole or one line of it, is withheld.
    let content = fs::read_to_string(&key).unwrap();
    let second = content.lines().nth(1).unwrap().to_string();
    assert!(makiba::daemon::review::secret_in(&format!("x {content} y")).is_some());
    assert!(
        makiba::daemon::review::secret_in(&format!("the line {second} quoted")).is_some(),
        "one line of the key is not found"
    );
    assert_eq!(
        makiba::daemon::review::secret_in("nothing of the key here"),
        None
    );
}

// ---- 12: the read surface --------------------------------------------------------------------

fn the_read_surface_shows_no_approval_key() {
    let root = temp_root();
    fs::create_dir_all(root.join("state")).unwrap();
    fs::create_dir_all(root.join("config")).unwrap();
    let default_key = root.join("config/approval.key");
    make_key(&default_key);
    let content = fs::read_to_string(&default_key).unwrap();
    let second = content.lines().nth(1).unwrap().to_string();
    {
        let w = StateWriter::open(&root.join("state/state.db")).unwrap();
        w.conn()
            .execute(
                "INSERT INTO dispatcher (id, pid, started_at, heartbeat_at, subscribed)
                 VALUES (1, 4242, strftime('%Y-%m-%dT%H:%M:%fZ','now','-1 hour'),
                         strftime('%Y-%m-%dT%H:%M:%fZ','now','-1 hour'), 1)",
                [],
            )
            .unwrap();
        w.ensure_project("acme", None, None).unwrap();
        let title = format!("title-start {second} title-end");
        let run = w
            .upsert_run(&RunUpsert {
                task_id: "t1",
                project_slug: "acme",
                title: Some(&title),
                status: Some("in_use"),
                ..Default::default()
            })
            .unwrap();
        w.conn()
            .execute(
                "INSERT INTO pending (run_id, kind, question, context_json)
                 VALUES (?1, 'question', ?2, ?3)",
                rusqlite::params![
                    run,
                    format!("question-start {second} question-end"),
                    json!({"background": content}).to_string()
                ],
            )
            .unwrap();
    }
    let cli = |args: &[&str], approval: Option<&Path>| -> String {
        let mut c = Command::new(MAKIBA);
        c.args(args)
            .env("MAKIBA_STATE_DIR", root.join("state"))
            .env("MAKIBA_PERSON_KEY", root.join("config/person.key"))
            .env("MAKIBA_GATE_KEY", root.join("config/gate.key"))
            .env("MAKIBA_DOOR", "none")
            .env("MAKIBA_PORT", "0")
            .env("MAKIBA_BASE", "stub")
            .stdin(Stdio::null());
        match approval {
            Some(p) => c.env("MAKIBA_APPROVAL_KEY", p),
            None => c.env_remove("MAKIBA_APPROVAL_KEY"),
        };
        text(&c.output().unwrap())
    };
    let custom = root.join("custom/approval.key");
    let places: [(&str, Option<PathBuf>); 2] =
        [("default", None), ("custom", Some(custom.clone()))];
    for (place, approval) in places {
        if approval.is_some() {
            // Only the custom place holds the key now.
            fs::create_dir_all(custom.parent().unwrap()).unwrap();
            fs::write(&custom, &content).unwrap();
            fs::remove_file(&default_key).unwrap();
        }
        let mut outputs = vec![];
        for args in [
            &["status", "--json"][..],
            &["status"],
            &["events", "--after", "0", "--json"],
            &["events", "--after", "0"],
        ] {
            outputs.push((args, cli(args, approval.as_deref())));
        }
        for (args, out) in &outputs {
            assert!(
                !out.contains(content.trim()),
                "{place}: makiba {args:?} shows the key: {out}"
            );
            for line in body_lines(&content) {
                assert!(
                    !out.contains(&line),
                    "{place}: makiba {args:?} shows the key's line {line}: {out}"
                );
            }
        }
        let (_, json_status) = &outputs[0];
        let (_, human_status) = &outputs[1];
        for (what, out) in [("status --json", json_status), ("status", human_status)] {
            assert!(
                out.contains("question-start <key> question-end"),
                "{place}: makiba {what} does not show the question masked: {out}"
            );
        }
        assert!(
            json_status.contains("title-start <key> title-end"),
            "{place}: makiba status --json does not show the title masked: {json_status}"
        );
    }
}

#[test]
fn the_approval_key_is_made_private_denied_to_every_agent_and_never_shown() {
    up_makes_the_key_and_approval_key_prints_it();
    up_does_not_make_the_key_beside_an_agent_that_may_read_it();
    the_role_files_deny_the_default_place();
    the_settings_of_every_role_deny_the_resolved_place();
    the_reader_sees_no_approval_key();
    the_read_surface_shows_no_approval_key();
}
