//! Issue 255: a pending entry carries its context (background, if_yes, if_no, recommend, why),
//! and the read surface shows it.
//!
//! An entry added with its context and escalated shows that context in `makiba status --json`, in
//! the text of `makiba status` and in the secretary's `status` tool; an entry escalated without
//! the five parts is refused with a message that names the missing ones (`recommend` may say
//! plainly that there is none); an entry made from a screen still shows its first line only; the
//! person's `makiba pending add` sends the five parts. The door runs in this process as the
//! secretary's tool and `makiba status` reads `state.db` by itself, so no socket is needed.

use clap::Parser;
use makiba::base::stub::Stub;
use makiba::daemon::door::{Door, issue_key};
use makiba::db::writer::{RunUpsert, StateWriter};
use makiba::spec::{ErrorKind, Role};
use rusqlite::{Connection, OpenFlags, params};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::Arc;

const MAKIBA: &str = env!("CARGO_BIN_EXE_makiba");
const PERSON_KEY: &str = "person-key-in-a-context-3b9d";
const PARTS: [&str; 5] = ["background", "if_yes", "if_no", "recommend", "why"];

struct Org {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    door: Door,
    secretary: String,
    /// A worker's token, whose SHA-256 `agents` stores: no output may show it.
    worker_key: String,
}

impl Org {
    /// Run `t1` in use, the secretary's key and a worker's; a stale heartbeat when `heartbeat`.
    fn new(heartbeat: bool) -> Org {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        for d in ["state", "config"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        std::fs::write(root.join("config/person.key"), format!("{PERSON_KEY}\n")).unwrap();
        let w = StateWriter::open(&root.join("state/state.db")).unwrap();
        if heartbeat {
            w.conn()
                .execute(
                    "INSERT INTO dispatcher (id, pid, started_at, heartbeat_at, subscribed)
                     VALUES (1, 4242, strftime('%Y-%m-%dT%H:%M:%fZ','now','-1 hour'),
                             strftime('%Y-%m-%dT%H:%M:%fZ','now','-1 hour'), 1)",
                    [],
                )
                .unwrap();
        }
        w.upsert_run(&RunUpsert {
            task_id: "t1",
            project_slug: "acme",
            status: Some("in_use"),
            ..Default::default()
        })
        .unwrap();
        let secretary = issue_key(&w, Role::Secretary, "secretary", None, None).unwrap();
        let worker_key = issue_key(&w, Role::Worker, "w-t1", None, None).unwrap();
        let door = Door::new(w, Arc::new(Stub::new()), "fake".into());
        Org {
            _tmp: tmp,
            root,
            door,
            secretary,
            worker_key,
        }
    }

    fn pending(&self, args: Value) -> Result<Value, makiba::spec::DoorError> {
        self.door
            .handle(Some(&self.secretary), "pending", args, None, true)
    }

    fn add(&self, question: &str, context: Value) -> i64 {
        let mut args = json!({"action": "add", "task": "t1", "question": question});
        for (k, v) in context.as_object().unwrap() {
            args[k] = v.clone();
        }
        let r = self
            .pending(args)
            .unwrap_or_else(|e| panic!("add was refused: {e:?}"));
        r["id"].as_i64().unwrap()
    }

    fn escalate(&self, id: i64) -> Result<Value, makiba::spec::DoorError> {
        self.pending(json!({"action": "escalate", "id": id}))
    }

    fn db(&self) -> Connection {
        Connection::open_with_flags(
            self.root.join("state/state.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap()
    }

    fn status_of(&self, id: i64) -> String {
        self.db()
            .query_row("SELECT status FROM pending WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .unwrap()
    }

    /// The person's `makiba status`, with no door and no `makibad`.
    fn cli(&self, args: &[&str]) -> Output {
        Command::new(MAKIBA)
            .args(args)
            .env("MAKIBA_STATE_DIR", self.root.join("state"))
            .env("MAKIBA_PERSON_KEY", self.root.join("config/person.key"))
            .env("MAKIBA_GATE_KEY", self.root.join("config/gate.key"))
            .env("MAKIBA_DOOR", "none")
            .env("MAKIBA_PORT", "0")
            .env("MAKIBA_BASE", "stub")
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    fn status_json(&self) -> Value {
        let out = self.cli(&["status", "--json"]);
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "status --json printed no JSON ({e}): {}",
                String::from_utf8_lossy(&out.stderr)
            )
        })
    }

    fn status_text(&self) -> String {
        let out = self.cli(&["status"]);
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn tool_status(&self) -> Value {
        self.door
            .handle(Some(&self.secretary), "status", json!({}), None, true)
            .unwrap_or_else(|e| panic!("the status tool was refused: {e:?}"))
    }
}

/// The entry of `id` in a status object's `pending`.
fn entry(v: &Value, id: i64) -> &Value {
    v["pending"]
        .as_array()
        .unwrap_or_else(|| panic!("no pending array: {v}"))
        .iter()
        .find(|e| e["id"] == json!(id))
        .unwrap_or_else(|| panic!("no pending entry {id}: {v}"))
}

fn full_context(org: &Org) -> Value {
    json!({
        "background": format!("The branch is behind main; token {} was seen", org.worker_key),
        "if_yes": "It merges main now.\nSecond line of if_yes.",
        "if_no": format!("It waits for review; {PERSON_KEY} stays put"),
        "recommend": "yes",
        "why": "CI is green on the merged head",
    })
}

#[test]
fn an_escalated_entry_shows_its_context_in_status_json_text_and_the_tool() {
    for heartbeat in [true, false] {
        let org = Org::new(heartbeat);
        let id = org.add("Merge main into the branch?", full_context(&org));
        org.escalate(id)
            .unwrap_or_else(|e| panic!("escalate with every part was refused: {e:?}"));
        assert_eq!(org.status_of(id), "escalated");

        let want = json!({
            "background": "The branch is behind main; token <key> was seen",
            "if_yes": "It merges main now.\nSecond line of if_yes.",
            "if_no": "It waits for review; <key> stays put",
            "recommend": "yes",
            "why": "CI is green on the merged head",
        });
        let local = org.status_json();
        let e = entry(&local, id);
        assert_eq!(e["status"], json!("escalated"), "{local}");
        assert_eq!(
            e["question"],
            json!("Merge main into the branch?"),
            "{local}"
        );
        assert_eq!(e["context"], want, "{local}");

        // The secretary's tool: the same context. This process's key files are not the person's,
        // so only what `agents` stores is masked here.
        let tool = org.tool_status();
        let t = entry(&tool, id);
        assert_eq!(t["status"], json!("escalated"), "{tool}");
        for part in ["background", "if_yes", "recommend", "why"] {
            assert_eq!(t["context"][part], want[part], "{part}: {tool}");
        }
        assert!(
            t["context"]["if_no"]
                .as_str()
                .is_some_and(|s| s.starts_with("It waits for review")),
            "{tool}"
        );

        // The text: the entry and its parts, first line each, keys masked.
        let text = org.status_text();
        assert!(
            text.lines()
                .any(|l| l.contains(&format!("#{id}")) && l.contains("Merge main into the branch?")),
            "heartbeat {heartbeat}: {text}"
        );
        for line in [
            "background: The branch is behind main; token <key> was seen",
            "if yes: It merges main now.",
            "if no: It waits for review; <key> stays put",
            "recommend: yes - CI is green on the merged head",
        ] {
            assert!(
                text.contains(line),
                "heartbeat {heartbeat}, no {line:?}: {text}"
            );
        }
        assert!(!text.contains("Second line of if_yes"), "{text}");
        for secret in [org.worker_key.as_str(), PERSON_KEY] {
            assert!(!text.contains(secret), "{text}");
            assert!(!local.to_string().contains(secret), "{local}");
            assert!(!tool.to_string().contains(&org.worker_key), "{tool}");
        }
    }
}

#[test]
fn an_entry_escalated_without_the_parts_is_refused_naming_them() {
    let org = Org::new(true);
    let id = org.add("Use the old API?", json!({}));
    let e = org.escalate(id).expect_err("escalated with no context");
    assert_eq!(e.kind, ErrorKind::Usage, "{e:?}");
    for part in PARTS {
        assert!(e.message.contains(part), "{part} not named: {e:?}");
    }
    assert_eq!(org.status_of(id), "open");
    let local = org.status_json();
    let c = &entry(&local, id)["context"];
    for part in PARTS {
        assert_eq!(c[part], Value::Null, "{part}: {local}");
    }

    // Some parts given (a blank one does not count): the rest are named, and only they.
    let again = org.add(
        "Use the old API?",
        json!({"background": "The new API is in beta.", "if_yes": "It ships today.",
               "why": "   "}),
    );
    assert_eq!(again, id, "the same open question is the same entry");
    let e = org
        .escalate(id)
        .expect_err("escalated with three parts missing");
    assert_eq!(e.kind, ErrorKind::Usage, "{e:?}");
    for part in ["if_no", "recommend", "why"] {
        assert!(e.message.contains(part), "{part} not named: {e:?}");
    }
    for part in ["background", "if_yes"] {
        assert!(!e.message.contains(part), "{part} is set, yet named: {e:?}");
    }
    assert_eq!(org.status_of(id), "open");

    // The rest given: it is escalated, with every part.
    org.add(
        "Use the old API?",
        json!({"if_no": "It waits a week.", "recommend": "no", "why": "The beta is stable."}),
    );
    org.escalate(id)
        .unwrap_or_else(|e| panic!("escalate with every part was refused: {e:?}"));
    let local = org.status_json();
    assert_eq!(
        entry(&local, id)["context"],
        json!({"background": "The new API is in beta.", "if_yes": "It ships today.",
               "if_no": "It waits a week.", "recommend": "no", "why": "The beta is stable."}),
        "{local}"
    );
}

#[test]
fn a_recommendation_may_say_there_is_none() {
    let org = Org::new(true);
    let id = org.add(
        "Which of the two layouts?",
        json!({"background": "Both pass review.", "if_yes": "Layout A.", "if_no": "Layout B.",
               "recommend": "None: I have no ground to prefer one.",
               "why": "Both meet the Issue; it is a matter of taste."}),
    );
    org.escalate(id)
        .unwrap_or_else(|e| panic!("a plain no-recommendation was refused: {e:?}"));
    let local = org.status_json();
    assert_eq!(
        entry(&local, id)["context"]["recommend"],
        json!("None: I have no ground to prefer one."),
        "{local}"
    );
}

#[test]
fn an_entry_made_from_a_screen_shows_its_first_line_only() {
    let org = Org::new(true);
    let screen = "w-t1 asks for permission:\nprivate-screen-line-4c1e\nDo you want to proceed?";
    let w = StateWriter::open(&org.root.join("state/state.db")).unwrap();
    w.conn()
        .execute(
            "INSERT INTO pending (run_id, kind, question, prompt_text, context_json)
             VALUES ((SELECT id FROM runs WHERE task_id = 't1'), 'question', ?1, ?1, ?2)",
            params![
                screen,
                json!({"background": "private-context-9a7b", "recommend": "yes"}).to_string()
            ],
        )
        .unwrap();
    let id = w.conn().last_insert_rowid();
    drop(w);

    // `add` of the same text is a new entry; the screen's entry is left as it was.
    let added = org.add(screen, json!({"background": "Asked by hand."}));
    assert_ne!(added, id, "add took the entry made from a screen");
    let stored: String = org
        .db()
        .query_row(
            "SELECT context_json FROM pending WHERE id = ?1",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!stored.contains("Asked by hand."), "{stored}");
    // The entry added by hand holds the whole text: it goes, so only the screen's is read below.
    org.pending(json!({"action": "withdraw", "id": added}))
        .unwrap();

    let local = org.status_json();
    let e = entry(&local, id);
    assert_eq!(e["question"], json!("w-t1 asks for permission:"), "{local}");
    for part in PARTS {
        assert_eq!(e["context"][part], Value::Null, "{part}: {local}");
    }
    let tool = org.tool_status();
    assert_eq!(
        entry(&tool, id)["question"],
        json!("w-t1 asks for permission:")
    );
    assert_eq!(
        entry(&tool, id)["context"]["background"],
        Value::Null,
        "{tool}"
    );
    let text = org.status_text();
    assert!(text.contains("w-t1 asks for permission:"), "{text}");
    for secret in ["private-screen-line-4c1e", "private-context-9a7b"] {
        assert!(!text.contains(secret), "{text}");
        assert!(!local.to_string().contains(secret), "{local}");
        assert!(!tool.to_string().contains(secret), "{tool}");
    }
}

#[test]
fn the_persons_pending_add_sends_the_five_parts() {
    let m = makiba::cli::Makiba::try_parse_from([
        "makiba",
        "pending",
        "add",
        "--task",
        "t1",
        "--question",
        "Merge it?",
        "--background",
        "b",
        "--if-yes",
        "y",
        "--if-no",
        "n",
        "--recommend",
        "r",
        "--why",
        "w",
    ])
    .unwrap_or_else(|e| panic!("makiba pending add does not take the context: {e}"));
    let (verb, args) = makiba::cli::request(m.verb).unwrap();
    assert_eq!(verb, "pending");
    assert_eq!(
        args,
        json!({"action": "add", "task": "t1", "question": "Merge it?", "background": "b",
               "if_yes": "y", "if_no": "n", "recommend": "r", "why": "w"})
    );
}
