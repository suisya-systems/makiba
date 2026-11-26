//! Issue 272: calling the person back (docs/design/person-call.md).
//!
//! - An entry that waits for the person (escalated, or a merge waiting for the person's key)
//!   makes makibad run the person's `notify_command` once, as `<path> <title>` with the body on
//!   stdin, and write `notify_sent` with `to` `person`; an open entry is never called.
//! - Entries waiting together are one call; a later entry is a call of its own; a restart calls
//!   nothing again.
//! - An empty `notify_command` silences it; set again, what still waits is called.
//! - A failing command is tried three times per setting; setting it again tries again.
//! - Keys are masked before anything is cut; control characters are replaced.
//! - Only the person sets it, and only to a path no agent can write.
//! - A command never holds the daemon's tick: a grandchild holding stderr does not delay the
//!   record, and a command that hangs is killed with its process group at the timeout.
//!
//! The daemon runs in this process on the stand-in base, stopped by a flag (as
//! tests/belt_stops.rs); the secretary and the person act through a door of their own.

mod common;

use common::{exe, makibad};
use makiba::base::stub::Stub;
use makiba::daemon::door::{Door, Response, issue_key, line};
use makiba::db::writer::{RunUpsert, StateWriter};
use makiba::spec::Role;
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Once, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const WAIT: u64 = 20;
const STAYS: Duration = Duration::from_secs(2);
const LAST_LINE: &str = "Answer in the secretary's conversation, or see makiba status.";

static INIT: Once = Once::new();
static HOME: OnceLock<tempfile::TempDir> = OnceLock::new();

/// The process's environment, once, before any test starts a thread: the call's test seams, and
/// the places agents write moved into a folder of the test's own.
fn init() {
    INIT.call_once(|| {
        makibad::process_knowledge();
        let home = HOME.get_or_init(|| tempfile::tempdir().unwrap());
        let root = std::fs::canonicalize(home.path()).unwrap();
        for d in ["repos", "workers", "docs", "share", "config", "goals"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        // SAFETY: before this call returns no test of this file has started a thread of its own.
        unsafe {
            std::env::set_var("MAKIBA_CALL_GAP_MS", "300");
            std::env::set_var("MAKIBA_CALL_RETRY_MS", "200");
            std::env::set_var("MAKIBA_CALL_TIMEOUT_MS", "3000");
            std::env::set_var("MAKIBA_REPOS_DIR", root.join("repos"));
            std::env::set_var("MAKIBA_WORKERS_DIR", root.join("workers"));
            std::env::set_var("MAKIBA_DOCS_DIR", root.join("docs"));
            std::env::set_var("MAKIBA_SHARE_DIR", root.join("share"));
            std::env::set_var("MAKIBA_GOALS_DIR", root.join("goals"));
            std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
            std::env::set_var("MAKIBA_LAUNCH_AGENT", "fake");
            std::env::remove_var("MAKIBA_SECRETARY");
        }
    });
}

/// Poll until `f` holds, for up to `secs` seconds.
fn wait_for(secs: u64, mut f: impl FnMut() -> bool) -> bool {
    let until = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < until {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// A folder for scripts, under the target's temporary folder (not under `/tmp`, which the rule
/// refuses), only the user can write.
fn scripts_folder() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("person-call-")
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap()
}

/// Whether process `pid` is alive.
fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Kill process `pid`, if it still runs.
fn kill(pid: i32) {
    // SAFETY: a process this test's script started.
    unsafe { libc::kill(pid, libc::SIGKILL) };
}

/// One organization: its state folder, the run `t1`, the daemon on the stand-in, a door for
/// the secretary and the person, and a folder of scripts with the recording one.
struct Org {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    door: Door,
    person: String,
    secretary: String,
    scripts: tempfile::TempDir,
    calls: PathBuf,
    recorder: PathBuf,
    stop: Arc<AtomicBool>,
    daemon: Option<JoinHandle<Result<(), String>>>,
}

impl Org {
    fn new() -> Org {
        init();
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let dir = root.join("state");
        std::fs::create_dir_all(&dir).unwrap();
        let calls = root.join("calls");
        std::fs::create_dir_all(&calls).unwrap();
        let w = StateWriter::open(&dir.join("state.db")).unwrap();
        w.upsert_run(&RunUpsert {
            task_id: "t1",
            project_slug: "p",
            status: Some("in_use"),
            ..Default::default()
        })
        .unwrap();
        let person = issue_key(&w, Role::Person, "person", None, None).unwrap();
        let secretary = issue_key(&w, Role::Secretary, "secretary", None, None).unwrap();
        let door = Door::new(w, Arc::new(Stub::new()), "fake".into());
        let scripts = scripts_folder();
        let folder = std::fs::canonicalize(scripts.path()).unwrap();
        let recorder = folder.join("record");
        exe::write(
            &recorder,
            format!(
                "#!/bin/sh\nd='{}'\nn=$(ls \"$d\" | grep -c '\\.title$')\ncat > \"$d/call-$n.body\"\nprintf '%s' \"$1\" > \"$d/call-$n.title\"\n",
                calls.display()
            ),
        );
        let mut org = Org {
            _tmp: tmp,
            dir,
            door,
            person,
            secretary,
            scripts,
            calls,
            recorder,
            stop: Arc::new(AtomicBool::new(false)),
            daemon: None,
        };
        org.start();
        org
    }

    fn start(&mut self) {
        self.stop = Arc::new(AtomicBool::new(false));
        let (dir, flag) = (self.dir.clone(), Arc::clone(&self.stop));
        self.daemon = Some(std::thread::spawn(move || {
            makibad::run_until(&dir, &|| Box::new(Stub::new()), flag)
        }));
    }

    fn halt(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.daemon.take() {
            t.join().unwrap().unwrap();
        }
    }

    /// The scripts' folder, resolved.
    fn folder(&self) -> PathBuf {
        std::fs::canonicalize(self.scripts.path()).unwrap()
    }

    /// A script named `name` in the scripts' folder; its path.
    fn script(&self, name: &str, body: &str) -> PathBuf {
        let path = self.folder().join(name);
        exe::write(&path, format!("#!/bin/sh\n{body}\n"));
        path
    }

    fn handle(&self, key: &str, verb: &str, args: Value) -> Response {
        self.door.handle(Some(key), verb, args, None, true)
    }

    fn call(&self, key: &str, verb: &str, args: Value) -> Value {
        self.handle(key, verb, args.clone())
            .unwrap_or_else(|e| panic!("{verb} {args} refused: {e:?}"))
    }

    /// The person's `config` (no tool: the person's own `makiba`).
    fn config(&self, args: Value) -> Response {
        self.door
            .handle(Some(&self.person), "config", args, None, false)
    }

    /// The person sets `notify_command` to `value`; the answer's value.
    fn set(&self, value: &str) -> Value {
        let r = self.config(json!({"action": "set", "key": "notify_command", "value": value}));
        r.unwrap_or_else(|e| panic!("config set notify_command {value:?} refused: {e:?}"))["value"]
            .clone()
    }

    /// The person's `config get notify_command`, its value.
    fn get(&self) -> Value {
        let r = self.config(json!({"action": "get", "key": "notify_command"}));
        r.unwrap_or_else(|e| panic!("config get notify_command refused: {e:?}"))["value"].clone()
    }

    /// The secretary adds a question with its five parts for `t1`; its id.
    fn add(&self, question: &str) -> i64 {
        let added = self.call(
            &self.secretary,
            "pending",
            json!({"action": "add", "task": "t1", "question": question,
                   "background": "Why it is asked.", "if_yes": "What yes does.",
                   "if_no": "What no does.", "recommend": "yes", "why": "The reason."}),
        );
        added["id"].as_i64().unwrap()
    }

    fn escalate(&self, id: i64) {
        self.call(
            &self.secretary,
            "pending",
            json!({"action": "escalate", "id": id}),
        );
    }

    /// A question added and escalated; its id.
    fn waiting(&self, question: &str) -> i64 {
        let id = self.add(question);
        self.escalate(id);
        id
    }

    fn db(&self) -> Connection {
        Connection::open_with_flags(self.dir.join("state.db"), OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap()
    }

    /// The payloads of the events of `kind` with `to` `person`, oldest first.
    fn events(&self, kind: &str) -> Vec<Value> {
        let c = self.db();
        let mut q = c
            .prepare("SELECT payload_json FROM events WHERE kind = ?1 ORDER BY id")
            .unwrap();
        q.query_map([kind], |r| r.get::<_, Option<String>>(0))
            .unwrap()
            .map(|p| serde_json::from_str(&p.unwrap().unwrap_or_default()).unwrap_or(Value::Null))
            .filter(|v: &Value| v["to"] == json!("person"))
            .collect()
    }

    fn sent(&self) -> Vec<Value> {
        self.events("notify_sent")
    }

    fn failed(&self) -> Vec<Value> {
        self.events("notify_failed")
    }

    /// The calls the recording script received, oldest first: (title, body).
    fn calls(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        loop {
            let n = out.len();
            let title = self.calls.join(format!("call-{n}.title"));
            let Ok(t) = std::fs::read_to_string(&title) else {
                return out;
            };
            let b = std::fs::read_to_string(self.calls.join(format!("call-{n}.body"))).unwrap();
            out.push((t, b));
        }
    }

    /// Wait until `n` calls were received and `n` `notify_sent` written; the calls.
    fn wait_calls(&self, n: usize) -> Vec<(String, String)> {
        let ok = wait_for(WAIT, || self.calls().len() >= n && self.sent().len() >= n);
        assert!(
            ok,
            "not {n} calls: calls {:?}, sent {:?}, failed {:?}",
            self.calls(),
            self.sent(),
            self.failed()
        );
        self.calls()
    }

    /// Wait until a `notify_sent` names `id`.
    fn wait_sent(&self, id: i64) -> Value {
        let mut found = None;
        let ok = wait_for(WAIT, || {
            found = self.sent().into_iter().find(|s| names(s, id));
            found.is_some()
        });
        assert!(
            ok,
            "no notify_sent names {id}: sent {:?}, failed {:?}",
            self.sent(),
            self.failed()
        );
        found.unwrap()
    }

    /// Wait until a `notify_failed` names `id`.
    fn wait_failed(&self, id: i64) -> Value {
        let mut found = None;
        let ok = wait_for(WAIT, || {
            found = self.failed().into_iter().find(|s| names(s, id));
            found.is_some()
        });
        assert!(
            ok,
            "no notify_failed names {id}: sent {:?}, failed {:?}",
            self.sent(),
            self.failed()
        );
        found.unwrap()
    }
}

impl Drop for Org {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.daemon.take() {
            let _ = t.join();
        }
    }
}

/// Whether an event's `pending_ids` names `id`.
fn names(v: &Value, id: i64) -> bool {
    v["pending_ids"]
        .as_array()
        .is_some_and(|a| a.contains(&json!(id)))
}

fn first_line(body: &str) -> &str {
    body.lines().next().unwrap_or_default()
}

fn last_line(body: &str) -> &str {
    body.trim_end_matches('\n')
        .lines()
        .last()
        .unwrap_or_default()
}

/// An escalated entry is called once; an open one never; a later entry is a call of its own;
/// nothing is called again, also after a restart.
#[test]
fn an_escalated_entry_calls_the_person_once_and_a_later_one_alone() {
    let mut org = Org::new();
    let a = org.add("qa");
    let b = org.waiting("qb");
    assert_eq!(
        org.set(&org.recorder.to_string_lossy()),
        json!(org.recorder.to_string_lossy())
    );

    let calls = org.wait_calls(1);
    assert_eq!(calls.len(), 1, "{calls:?}");
    let (title, body) = &calls[0];
    assert_eq!(title, "makiba secretary: 1 waiting for you");
    assert_eq!(first_line(body), format!("#{b} qb"));
    assert!(!body.contains(&format!("#{a}")), "{body}");
    assert_eq!(last_line(body), LAST_LINE);
    let sent = org.sent();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0]["to"], json!("person"));
    assert_eq!(sent[0]["pending_ids"], json!([b]));
    assert_eq!(sent[0]["command"], json!(org.recorder.to_string_lossy()));
    assert_eq!(sent[0]["title"], json!(title));
    assert_eq!(sent[0]["body"], json!(body));

    let c = org.waiting("qc");
    let calls = org.wait_calls(2);
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert_eq!(first_line(&calls[1].1), format!("#{c} qc"));
    assert!(!calls[1].1.contains(&format!("#{b}")), "{:?}", calls[1]);
    let sent = org.sent();
    assert_eq!(sent.len(), 2, "{sent:?}");
    assert_eq!(sent[1]["pending_ids"], json!([c]));

    std::thread::sleep(STAYS);
    assert_eq!(org.calls().len(), 2);
    assert_eq!(org.sent().len(), 2);
    org.halt();
    org.start();
    std::thread::sleep(STAYS);
    assert_eq!(org.calls().len(), 2, "{:?}", org.calls());
    assert_eq!(org.sent().len(), 2, "{:?}", org.sent());
}

/// Entries that wait together are one call; more than five end with `and <k> more`; a long
/// question is cut; control characters become spaces.
#[test]
fn entries_waiting_together_are_one_call() {
    let org = Org::new();
    let recorder = org.recorder.to_string_lossy().into_owned();
    let b = org.waiting("qb");
    let c = org.waiting("qc");
    org.set(&recorder);
    let calls = org.wait_calls(1);
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(calls[0].0, "makiba secretary: 2 waiting for you");
    let lines: Vec<&str> = calls[0].1.lines().collect();
    assert_eq!(lines[0], format!("#{b} qb"));
    assert_eq!(lines[1], format!("#{c} qc"));
    assert_eq!(org.sent()[0]["pending_ids"], json!([b, c]));

    assert_eq!(org.set(""), json!(""));
    let seven: Vec<i64> = (0..7).map(|i| org.waiting(&format!("q{i}"))).collect();
    org.set(&recorder);
    let calls = org.wait_calls(2);
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert_eq!(calls[1].0, "makiba secretary: 7 waiting for you");
    let lines: Vec<&str> = calls[1].1.lines().collect();
    for (i, id) in seven.iter().take(5).enumerate() {
        assert_eq!(lines[i], format!("#{id} q{i}"));
    }
    assert_eq!(lines[5], "and 2 more");
    assert_eq!(org.sent()[1]["pending_ids"], json!(seven));

    let long: String = "abcdefghij".repeat(20);
    let l = org.waiting(&long);
    let calls = org.wait_calls(3);
    assert_eq!(first_line(&calls[2].1), format!("#{l} {}...", &long[..120]));

    let ctl = org.waiting("one\u{0}two\tthree");
    org.wait_sent(ctl);
    let calls = org.wait_calls(4);
    assert_eq!(first_line(&calls[3].1), format!("#{ctl} one two three"));
}

/// A merge that waits for the person's key is called with the command that lands it.
#[test]
fn a_merge_waiting_for_the_key_calls_with_its_command() {
    let org = Org::new();
    let m: i64 = {
        let c = Connection::open(org.dir.join("state.db")).unwrap();
        c.execute(
            "INSERT INTO pending (run_id, kind, question, status)
             VALUES ((SELECT id FROM runs WHERE task_id = 't1'), 'merge',
                     'Merge example/repo#7 at its head?', 'open')",
            [],
        )
        .unwrap();
        c.last_insert_rowid()
    };
    org.set(&org.recorder.to_string_lossy());
    let calls = org.wait_calls(1);
    assert_eq!(
        first_line(&calls[0].1),
        format!("#{m} a merge waits for your key: makiba merge approve {m}")
    );
    assert_eq!(org.sent()[0]["pending_ids"], json!([m]));
}

/// An empty command is off; set again, what still waits is called, and what stopped waiting
/// is not.
#[test]
fn unsetting_the_command_silences_and_setting_it_calls_what_still_waits() {
    let org = Org::new();
    let recorder = org.recorder.to_string_lossy().into_owned();
    org.set(&recorder);
    assert_eq!(org.set(""), json!(""));
    let b = org.waiting("qb");
    std::thread::sleep(STAYS);
    assert!(org.calls().is_empty(), "{:?}", org.calls());
    assert!(org.sent().is_empty(), "{:?}", org.sent());

    let d = org.waiting("qd");
    org.call(
        &org.secretary,
        "pending",
        json!({"action": "withdraw", "id": d}),
    );
    let e = org.waiting("qe");
    org.call(&org.person, "answer", json!({"id": e, "answer": "yes"}));

    org.set(&recorder);
    let calls = org.wait_calls(1);
    assert!(calls[0].1.contains(&format!("#{b} qb")), "{calls:?}");
    let sent = org.sent();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0]["pending_ids"], json!([b]));
    std::thread::sleep(STAYS);
    assert!(
        org.sent().iter().all(|s| !names(s, d) && !names(s, e)),
        "{:?}",
        org.sent()
    );
}

/// A command that fails is tried three times under its setting, then not again; setting the
/// command again tries again.
#[test]
fn a_failing_command_is_tried_three_times_until_it_is_set_again() {
    let org = Org::new();
    let failing = org.script("fail", "exit 3");
    let b = org.waiting("qb");
    org.set(&failing.to_string_lossy());
    let ok = wait_for(WAIT, || {
        org.failed().iter().filter(|f| names(f, b)).count() >= 3
    });
    assert!(ok, "not three failures: {:?}", org.failed());
    std::thread::sleep(STAYS);
    let failed: Vec<Value> = org.failed().into_iter().filter(|f| names(f, b)).collect();
    assert_eq!(failed.len(), 3, "{failed:?}");
    for f in &failed {
        assert_eq!(f["to"], json!("person"));
        assert_eq!(f["reason"], json!("exit 3"));
    }
    assert!(org.sent().is_empty(), "{:?}", org.sent());

    org.set(&org.recorder.to_string_lossy());
    let calls = org.wait_calls(1);
    assert!(calls[0].1.contains(&format!("#{b} qb")), "{calls:?}");
    let sent = org.sent();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0]["pending_ids"], json!([b]));
}

/// A key is masked whole before the question is cut, and before stderr is cut.
#[test]
fn keys_are_masked_before_anything_is_cut() {
    let org = Org::new();
    let key = org.secretary.clone();
    let b = org.waiting(&format!("{}{key}", "x".repeat(100)));
    org.set(&org.recorder.to_string_lossy());
    let calls = org.wait_calls(1);
    let body = &calls[0].1;
    assert!(!body.contains(&key[..20]), "{body}");
    let sent = org.wait_sent(b);
    assert!(!sent.to_string().contains(&key[..20]), "{sent}");

    let loud = org.script(
        "loud",
        &format!("printf '%0490d' 0 >&2\nprintf '%s' '{key}' >&2\nexit 1"),
    );
    org.set(&loud.to_string_lossy());
    let c = org.waiting("qc");
    let failed = org.wait_failed(c);
    let stderr = failed["stderr"].as_str().unwrap_or_default();
    assert!(!stderr.contains(&key[..10]), "{stderr}");
    assert!(!failed.to_string().contains(&key[..10]), "{failed}");
}

/// Only the person sets the command, and only to a path no agent can write; a path made
/// unsafe after the set is not run.
#[test]
fn only_the_person_sets_a_safe_command() {
    let org = Org::new();
    assert_eq!(org.get(), Value::Null);

    let r = org.door.handle(
        Some(&org.secretary),
        "config",
        json!({"action": "set", "key": "notify_command", "value": org.recorder.to_string_lossy()}),
        None,
        true,
    );
    assert_eq!(line(&r)["error"]["kind"], json!("denied"), "{}", line(&r));

    let in_state = org.dir.join("call");
    exe::write(&in_state, "#!/bin/sh\nexit 0\n");
    let open = scripts_folder();
    let open_script = std::fs::canonicalize(open.path()).unwrap().join("open");
    exe::write(&open_script, "#!/bin/sh\nexit 0\n");
    // not an executable: a directory's mode
    std::fs::set_permissions(open.path(), std::fs::Permissions::from_mode(0o775)).unwrap();
    let link = org.folder().join("link");
    std::os::unix::fs::symlink(&in_state, &link).unwrap();
    let missing = org.folder().join("missing");
    let newline = format!("{}\nx", org.recorder.to_string_lossy());
    for value in [
        "relative/path".to_string(),
        newline,
        missing.to_string_lossy().into_owned(),
        in_state.to_string_lossy().into_owned(),
        open_script.to_string_lossy().into_owned(),
        link.to_string_lossy().into_owned(),
    ] {
        let r = org.config(json!({"action": "set", "key": "notify_command", "value": value}));
        assert_eq!(
            line(&r)["error"]["kind"],
            json!("usage"),
            "{value:?}: {}",
            line(&r)
        );
        assert_eq!(org.get(), Value::Null, "{value:?} was stored");
    }

    let recorder = org.recorder.to_string_lossy().into_owned();
    org.set(&recorder);
    assert_eq!(org.get(), json!(recorder));

    // not an executable: a directory's mode
    std::fs::set_permissions(org.folder(), std::fs::Permissions::from_mode(0o770)).unwrap();
    let b = org.waiting("qb");
    let failed = org.wait_failed(b);
    assert!(
        failed["reason"]
            .as_str()
            .is_some_and(|r| r.starts_with("unsafe:")),
        "{failed}"
    );
    assert!(org.calls().is_empty(), "{:?}", org.calls());
    // not an executable: a directory's mode
    std::fs::set_permissions(org.folder(), std::fs::Permissions::from_mode(0o700)).unwrap();
}

/// A grandchild that holds stderr does not delay the record of a call that exited; a command
/// that hangs is killed with its process group at the timeout.
#[test]
fn a_command_never_holds_the_tick() {
    let org = Org::new();
    let left_pid = org.folder().join("left.pid");
    let leaves = org.script(
        "leaves",
        &format!("sleep 60 >&2 &\necho $! > '{}'\nexit 0", left_pid.display()),
    );
    org.set(&leaves.to_string_lossy());
    let b = org.waiting("qb");
    org.wait_sent(b);
    if let Some(pid) = std::fs::read_to_string(&left_pid)
        .ok()
        .and_then(|p| p.trim().parse().ok())
    {
        kill(pid);
    }

    let hang_pid = org.folder().join("hang.pid");
    let hangs = org.script(
        "hangs",
        &format!("sleep 60 >&2 &\necho $! > '{}'\nwait", hang_pid.display()),
    );
    org.set(&hangs.to_string_lossy());
    let c = org.waiting("qc");
    let failed = org.wait_failed(c);
    assert_eq!(failed["reason"], json!("timeout"), "{failed}");
    let pid: i32 = std::fs::read_to_string(&hang_pid)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let gone = wait_for(5, || !alive(pid));
    if !gone {
        kill(pid);
    }
    assert!(gone, "the grandchild {pid} still runs after the timeout");
}
