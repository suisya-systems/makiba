//! Issue 271: a goal ledger is written and revised through the verb `goal`, never by hand, and a
//! ledger that fails the validator is never placed. The triage's lines about a project with no
//! ledger, a ledger in error, and Issues that match no clause point at the skill
//! `makiba:goal-setup`, which the secretary's rules list.
//!
//! The verb runs on the real binary, `makiba daemon` on the stand-in base with `MAKIBA_DOOR=none`
//! (no Unix socket is needed), called over the MCP port with the agents' tokens. The triage lines
//! run in this process on the stand-in base, with a stand-in `gh`, as in `triage_forward.rs`.

use makiba::base::Base;
use makiba::base::stub::Stub;
use makiba::daemon;
use makiba::db::writer::{NewEvent, RunUpsert, StateWriter};
use makiba::work_discovery::goals::{GoalStatus, load_goals};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Once, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const MAKIBA: &str = env!("CARGO_BIN_EXE_makiba");

const SECRETARY_KEY: &str = "5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5ec5";
const WORKER_KEY: &str = "0d1a0d1a0d1a0d1a0d1a0d1a0d1a0d1a0d1a0d1a0d1a0d1a0d1a0d1a0d1a0d1a";
const CURATOR_KEY: &str = "c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0";

const FIRST: &str = "# Goals of o/repo\n\n\
## G1 It runs its own development\n\
- unmet if: no Issue is started by the organization for a week\n\n\
## G2 It is quiet\n\
Text of the clause.\n\
- unmet if: the person is asked something a rule answers\n\
- unmet if: a notice is sent twice\n";

const SECOND: &str = "# Goals of o/repo\n\n\
## G1 It is quiet\n\
- unmet if: a notice is sent twice\n";

fn sha256(bytes: &[u8]) -> String {
    let mut child = Command::new("sha256sum")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(bytes).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap()[..64].to_string()
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A `makibad` that is killed when the test ends, so a failed test leaves nothing running.
struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// One tool call on the port with a bearer token: whether it is a tool error, and the JSON of its
/// text.
fn call(addr: &str, token: &str, tool: &str, args: Value) -> (bool, Value) {
    let body = json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call",
        "params": {"name": tool, "arguments": args}})
    .to_string();
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(
        s,
        "POST /mcp HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut all = String::new();
    s.read_to_string(&mut all).unwrap();
    let body = all.split_once("\r\n\r\n").map_or("", |(_, b)| b);
    let r: Value = serde_json::from_str(body).unwrap_or_else(|e| panic!("not JSON: {all:?}: {e}"));
    let result = &r["result"];
    let text = result["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("no tool result: {r}"));
    (
        result["isError"] == json!(true),
        serde_json::from_str(text).unwrap(),
    )
}

struct Org {
    _tmp: tempfile::TempDir,
    goals: PathBuf,
    db: PathBuf,
    addr: String,
    _daemon: Daemon,
}

impl Org {
    fn start() -> Org {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        for d in [
            "home",
            "state",
            "config",
            "data",
            "run",
            "xdg-state",
            "goals",
        ] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        let db = root.join("state/state.db");
        let w = StateWriter::open(&db).unwrap();
        let run = w
            .upsert_run(&RunUpsert {
                task_id: "t1",
                project_slug: "p",
                status: Some("queued"),
                ..Default::default()
            })
            .unwrap();
        for (agent, role, key, run) in [
            ("secretary", "secretary", SECRETARY_KEY, None),
            ("w-t1", "worker", WORKER_KEY, Some(run)),
            ("curator", "curator", CURATOR_KEY, None),
        ] {
            w.conn()
                .execute(
                    "INSERT INTO agents (agent_id, role, run_id, key_sha256) VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![agent, role, run, sha256(key.as_bytes())],
                )
                .unwrap();
        }
        drop(w);
        let port = free_port();
        let daemon = Daemon(
            Command::new(MAKIBA)
                .env("HOME", root.join("home"))
                .env("MAKIBA_STATE_DIR", root.join("state"))
                .env("XDG_STATE_HOME", root.join("xdg-state"))
                .env("XDG_CONFIG_HOME", root.join("config"))
                .env("XDG_DATA_HOME", root.join("data"))
                .env("XDG_RUNTIME_DIR", root.join("run"))
                .env("MAKIBA_GOALS_DIR", root.join("goals"))
                .env("MAKIBA_DOOR", "none")
                .env("MAKIBA_PORT", port.to_string())
                .env("MAKIBA_BASE", "stub")
                .env("MAKIBA_STUB_SUBSCRIBE_DELAY_MS", "0")
                .env_remove("MAKIBA_PERSON_KEY")
                .arg("daemon")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let addr = format!("127.0.0.1:{port}");
        let until = Instant::now() + Duration::from_secs(15);
        while Instant::now() < until && TcpStream::connect(&addr).is_err() {
            std::thread::sleep(Duration::from_millis(50));
        }
        Org {
            goals: root.join("goals"),
            _tmp: tmp,
            db,
            addr,
            _daemon: daemon,
        }
    }

    fn goal(&self, key: &str, action: &str, repo: &str, text: Option<&str>) -> (bool, Value) {
        let mut args = json!({"action": action, "repo": repo});
        if let Some(t) = text {
            args["text"] = json!(t);
        }
        call(&self.addr, key, "goal", args)
    }

    /// The `goal_ledger_placed` events of a repository: (actor, payload), oldest first.
    fn placed(&self, repo: &str) -> Vec<(String, Value)> {
        let c = Connection::open_with_flags(&self.db, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let mut q = c
            .prepare(
                "SELECT actor, payload_json FROM events WHERE kind = 'goal_ledger_placed'
                  AND json_extract(payload_json, '$.repo') = ?1 ORDER BY id",
            )
            .unwrap();
        q.query_map([repo], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .unwrap()
        .map(|r| {
            let (a, p) = r.unwrap();
            (a, serde_json::from_str(&p).unwrap())
        })
        .collect()
    }

    fn file(&self) -> PathBuf {
        self.goals.join("o/repo.md")
    }
}

#[test]
fn a_ledger_is_written_and_revised_through_the_verb_and_a_bad_draft_is_never_placed() {
    let org = Org::start();
    let s = SECRETARY_KEY;

    // 1. No ledger yet: `show` says unset, writes nothing.
    let (e, shown) = org.goal(s, "show", "O/Repo", None);
    assert!(!e, "{shown}");
    assert_eq!(shown["status"], json!("unset"), "{shown}");
    assert_eq!(shown["repo"], json!("o/repo"), "{shown}");
    assert!(!org.file().exists());

    // 2. Drafts the validator refuses are `usage` with the reason, through `check` and `place`
    //    alike, and no file or folder is made.
    let bad = [
        ("", "no clauses"),
        ("## G1 Only\n", "G1: no 'unmet if:' line"),
        ("## G2 Late\n- unmet if: x\n", "G1..G1"),
        ("## G1 A\n- unmet if: x\n## G1 B\n- unmet if: y\n", "G1..G2"),
        ("## G1\n- unmet if: x\n", "line 1: malformed clause heading"),
    ];
    for (draft, reason) in bad {
        for action in ["check", "place"] {
            let (e, err) = org.goal(s, action, "o/repo", Some(draft));
            assert!(e, "{action} {draft:?}: {err}");
            assert_eq!(err["kind"], json!("usage"), "{err}");
            assert!(
                err["message"].as_str().unwrap_or_default().contains(reason),
                "{action} {draft:?}: {err}"
            );
        }
    }
    assert!(!org.goals.join("o").exists());
    assert_eq!(org.placed("o/repo"), vec![]);

    // 3. `check` of a good draft names its clauses and writes nothing.
    let (e, checked) = org.goal(s, "check", "o/repo", Some(FIRST));
    assert!(!e, "{checked}");
    assert_eq!(checked["status"], json!("valid"), "{checked}");
    assert_eq!(checked["clauses"][1]["heading"], json!("It is quiet"));
    assert!(!org.file().exists());

    // 4. `place` writes the draft byte for byte, in the lower case name, and the scan's own
    //    loader reads it.
    let (e, placed) = org.goal(s, "place", "O/Repo", Some(FIRST));
    assert!(!e, "{placed}");
    assert_eq!(placed["status"], json!("placed"), "{placed}");
    assert_eq!(placed["previous"], Value::Null, "{placed}");
    assert_eq!(std::fs::read_to_string(org.file()).unwrap(), FIRST);
    let loaded = load_goals(&org.goals, "o/repo");
    assert_eq!(loaded.status, GoalStatus::Ok, "{:?}", loaded.error);
    assert_eq!(loaded.clauses.len(), 2);
    assert!(!org.goals.join("o/repo.md.prev").exists());
    let events = org.placed("o/repo");
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0].0, "secretary");
    assert_eq!(events[0].1["clauses"], json!(2));
    assert_eq!(events[0].1["bytes"], json!(FIRST.len()));
    assert_eq!(events[0].1["previous_sha256"], Value::Null);

    // 5. `show` returns the text as it is, its clauses, and who placed it.
    let (e, shown) = org.goal(s, "show", "o/repo", None);
    assert!(!e, "{shown}");
    assert_eq!(shown["status"], json!("ok"), "{shown}");
    assert_eq!(shown["text"], json!(FIRST), "{shown}");
    assert_eq!(shown["clauses"][0]["id"], json!("G1"), "{shown}");
    assert_eq!(shown["placed_by"], json!("secretary"), "{shown}");
    assert!(shown["placed_at"].is_string(), "{shown}");

    // 6. A bad revision leaves the placed file as it was, and no event.
    let (e, err) = org.goal(s, "place", "o/repo", Some("## G1 Broken\n"));
    assert!(e, "{err}");
    assert_eq!(err["kind"], json!("usage"), "{err}");
    assert_eq!(std::fs::read_to_string(org.file()).unwrap(), FIRST);
    assert_eq!(org.placed("o/repo").len(), 1);

    // 7. A revision replaces the file, keeps the old one beside it (one generation), and the
    //    event names the old file's hash and clause count.
    let (e, revised) = org.goal(s, "place", "o/repo", Some(SECOND));
    assert!(!e, "{revised}");
    assert_eq!(revised["previous"], json!(2), "{revised}");
    assert_eq!(std::fs::read_to_string(org.file()).unwrap(), SECOND);
    assert_eq!(
        std::fs::read_to_string(org.goals.join("o/repo.md.prev")).unwrap(),
        FIRST
    );
    let events = org.placed("o/repo");
    assert_eq!(events.len(), 2, "{events:?}");
    assert_eq!(
        events[1].1["previous_sha256"],
        json!(sha256(FIRST.as_bytes()))
    );
    assert_eq!(events[1].1["previous_clauses"], json!(2));
    let names: Vec<String> = std::fs::read_dir(org.goals.join("o"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        names.iter().all(|n| n == "repo.md" || n == "repo.md.prev"),
        "no staging file is left: {names:?}"
    );

    // 8. A ledger that cannot be read (broken by hand) is shown with its text and reason, and a
    //    placement repairs it.
    std::fs::write(org.file(), "## G1 Broken\n").unwrap();
    let (e, shown) = org.goal(s, "show", "o/repo", None);
    assert!(!e, "{shown}");
    assert_eq!(shown["status"], json!("error"), "{shown}");
    assert_eq!(shown["text"], json!("## G1 Broken\n"), "{shown}");
    assert!(
        shown["error"]
            .as_str()
            .unwrap_or_default()
            .contains("unmet"),
        "{shown}"
    );
    let (e, fixed) = org.goal(s, "place", "o/repo", Some(FIRST));
    assert!(!e, "{fixed}");
    assert_eq!(fixed["previous"], Value::Null, "{fixed}");
    assert_eq!(std::fs::read_to_string(org.file()).unwrap(), FIRST);
}

#[test]
fn a_worker_or_the_curator_cannot_use_goal_and_a_name_of_another_case_is_not_replaced() {
    let org = Org::start();
    for key in [WORKER_KEY, CURATOR_KEY] {
        for action in ["show", "check", "place"] {
            let (e, denied) = org.goal(key, action, "o/repo", Some(FIRST));
            assert!(e, "{action}: {denied}");
            assert_eq!(denied["kind"], json!("denied"), "{denied}");
        }
    }
    assert!(!org.goals.join("o").exists());
    assert_eq!(org.placed("o/repo"), vec![]);

    // A file `Repo.md` is the loader's case mismatch: placing would leave two files, so it is
    // `conflict` and nothing is written.
    std::fs::create_dir_all(org.goals.join("o")).unwrap();
    std::fs::write(org.goals.join("o/Repo.md"), "kept\n").unwrap();
    let (e, conflict) = org.goal(SECRETARY_KEY, "place", "o/repo", Some(FIRST));
    assert!(e, "{conflict}");
    assert_eq!(conflict["kind"], json!("conflict"), "{conflict}");
    assert_eq!(
        std::fs::read_to_string(org.goals.join("o/Repo.md")).unwrap(),
        "kept\n"
    );
    assert!(!org.file().exists() || std::fs::read_to_string(org.file()).unwrap() == "kept\n");
    assert_eq!(org.placed("o/repo"), vec![]);

    // A repository name that is not OWNER/NAME is `usage`.
    for repo in ["repo", "o/../x", "a/b/c", ""] {
        let (e, err) = org.goal(SECRETARY_KEY, "place", repo, Some(FIRST));
        assert!(e, "{repo:?}: {err}");
        assert_eq!(err["kind"], json!("usage"), "{repo:?}: {err}");
    }
}

#[test]
fn the_secretarys_rules_and_the_skills_name_the_goal_dialogue() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let skill = std::fs::read_to_string(root.join("skills/goal-setup/SKILL.md"))
        .expect("skills/goal-setup/SKILL.md exists");
    assert!(skill.starts_with("---\nname: goal-setup\n"), "{skill}");
    for field in ["\ndescription: ", "\nwhen_to_use: "] {
        assert!(skill.contains(field), "no {field:?} in the frontmatter");
    }
    // The dialogue holds the ledger only through the verb, and says what the provider is sent.
    assert!(skill.contains("goal"), "{skill}");
    let rules = std::fs::read_to_string(root.join("roles/secretary/rules.md")).unwrap();
    assert!(rules.contains("`makiba:goal-setup`"), "rules.md: {rules}");
    assert!(rules.contains("`goal`"), "rules.md names the verb");
}

// ---- the triage's lines -------------------------------------------------------------------

const STAND_IN: &str = r#"#!/bin/sh
what="$1 $2"; shift 2
case "$what" in "issue list"|"pr list") ;; *) echo "unexpected call: $what" >&2; exit 2 ;; esac
repo=; state=
while [ $# -gt 0 ]; do
  case "$1" in
    --repo) repo=$2; shift 2 ;;
    --state) state=$2; shift 2 ;;
    --limit|--json|--search|--base) shift 2 ;;
    *) echo "unexpected argument: $1" >&2; exit 2 ;;
  esac
done
r=${repo#github.com/}
f="$FAKE_GH_DIR/${r%%/*}_${r#*/}/${what%% *}-$state.json"
if [ -e "$f" ]; then cat "$f"; else echo '[]'; fi
"#;

static HOME: OnceLock<tempfile::TempDir> = OnceLock::new();
static INIT: Once = Once::new();

fn init() -> PathBuf {
    INIT.call_once(|| {
        let home = HOME.get_or_init(|| tempfile::tempdir().unwrap());
        let script = home.path().join("gh");
        std::fs::write(&script, STAND_IN).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        for d in ["answers", "goals", "repos"] {
            std::fs::create_dir_all(home.path().join(d)).unwrap();
        }
        // SAFETY: before this call returns no test of this file has started a thread that reads
        // the environment through `std::env`: the daemons of the other tests are child processes.
        unsafe {
            std::env::set_var("MAKIBA_GH", &script);
            std::env::set_var("FAKE_GH_DIR", home.path().join("answers"));
            std::env::set_var("MAKIBA_GOALS_DIR", home.path().join("goals"));
            std::env::set_var("MAKIBA_REPOS_DIR", home.path().join("repos"));
            std::env::set_var("MAKIBA_TRIAGE_INTERVAL_MS", "1500");
        }
    });
    HOME.get().unwrap().path().to_path_buf()
}

fn issue(number: i64, title: &str, body: &str) -> Value {
    json!({"number": number, "title": title, "body": body, "labels": [],
           "updatedAt": format!("2026-11-{:02}T10:00:00Z", 10 + number), "createdAt": "2026-11-01T10:00:00Z",
           "milestone": null, "comments": []})
}

/// The notice the triage queues for a project `o/<name>` whose ledger is `ledger` (none: unset)
/// and whose open Issues are `issues`.
fn notice_for(name: &str, ledger: Option<&str>, issues: &[Value]) -> String {
    let home = init();
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("state");
    std::fs::create_dir_all(&dir).unwrap();
    let w = StateWriter::open(&dir.join("state.db")).unwrap();
    w.ensure_project(name, None, Some(&format!("https://github.com/o/{name}")))
        .unwrap();
    w.upsert_run(&RunUpsert {
        task_id: "t1",
        project_slug: name,
        title: Some("T"),
        status: Some("in_use"),
        ..Default::default()
    })
    .unwrap();
    if let Some(text) = ledger {
        std::fs::create_dir_all(home.join("goals/o")).unwrap();
        std::fs::write(home.join(format!("goals/o/{name}.md")), text).unwrap();
    }
    let answers = home.join("answers").join(format!("o_{name}"));
    std::fs::create_dir_all(&answers).unwrap();
    std::fs::write(
        answers.join("issue-open.json"),
        serde_json::to_string(issues).unwrap(),
    )
    .unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let thread: JoinHandle<Result<(), String>> = {
        let (dir, stop) = (dir.clone(), Arc::clone(&stop));
        std::thread::spawn(move || {
            daemon::run_until(&dir, &|| Box::new(Stub::new()) as Box<dyn Base>, stop)
        })
    };
    let db = Connection::open(dir.join("state.db")).unwrap();
    let count = |sql: &str| db.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
    let wait = |sql: &str| {
        let until = Instant::now() + Duration::from_secs(20);
        while count(sql) == 0 {
            assert!(Instant::now() < until, "timed out waiting for {sql}");
            std::thread::sleep(Duration::from_millis(25));
        }
    };
    wait("SELECT COUNT(*) FROM events WHERE kind = 'dispatcher_started'");
    w.append_event(&NewEvent {
        kind: "run_closed",
        actor: Some("secretary"),
        payload: Some(&json!({"task_id": "t1"})),
        run_task_id: Some("t1"),
        ..Default::default()
    })
    .unwrap();
    wait("SELECT COUNT(*) FROM events WHERE kind = 'work_discovery_scanned'");
    wait(
        "SELECT COUNT(*) FROM messages WHERE kind = 'notice' AND sender = 'makibad'
          AND recipient = 'secretary' AND body LIKE 'Triage after a close%'",
    );
    let body: String = db
        .query_row(
            "SELECT body FROM messages WHERE kind = 'notice' AND sender = 'makibad'
              AND recipient = 'secretary' AND body LIKE 'Triage after a close%'
              ORDER BY id LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    stop.store(true, Ordering::SeqCst);
    let _ = thread.join();
    body
}

#[test]
fn the_notice_for_a_project_with_no_ledger_points_at_the_skill() {
    let n = notice_for("pointnone", None, &[issue(7, "Something", "Body.")]);
    assert!(n.contains("No goal ledger: o/pointnone"), "{n}");
    assert!(n.contains("makiba:goal-setup"), "{n}");
}

#[test]
fn the_notice_for_a_ledger_in_error_points_at_the_skill() {
    let n = notice_for(
        "pointerr",
        Some("## G1 No unmet line\n"),
        &[issue(7, "Something", "Body.")],
    );
    assert!(n.contains("Goal ledger error: o/pointerr"), "{n}");
    assert!(n.contains("makiba:goal-setup"), "{n}");
}

#[test]
fn the_notice_names_the_skill_for_issues_that_match_no_clause_and_only_then() {
    let ledger = "# Goals\n\n## G1 It runs its own development\n- unmet if: nothing is proposed\n";
    let n = notice_for(
        "pointclause",
        Some(ledger),
        &[
            issue(
                41,
                "Names a clause",
                "Do.\n\n## Goal clause\n\n- G1 It runs its own development\n",
            ),
            issue(42, "Names none", "Do."),
        ],
    );
    assert!(n.contains("match no clause"), "{n}");
    assert!(n.contains("makiba:goal-setup"), "{n}");

    let all = notice_for(
        "pointclauseok",
        Some(ledger),
        &[issue(
            43,
            "Names a clause",
            "Do.\n\n## Goal clause\n\n- G1 It runs its own development\n",
        )],
    );
    assert!(!all.contains("match no clause"), "{all}");
}
