//! Issue 168: one read surface for programs outside `makibad`.
//!
//! `makiba status --json` lists the runs, terminals, open anomalies, pending entries and PR watches
//! with `makibad` stopped; `makiba events --after <id>` returns the events after an id, in order;
//! the door gives a holder of the person key the same read without recording it, and refuses an
//! agent's key; no output carries a key, a token or a message body between two agents; and the
//! shapes written in `docs/design/spec.md` section 12 are the shapes the code prints.
//!
//! No `makibad` runs here: the state is written with `StateWriter`, and the door is called in this
//! process, so no socket is needed.

use makiba::base::stub::Stub;
use makiba::daemon::door::{Door, issue_key};
use makiba::db::writer::{NewEvent, StateWriter};
use makiba::spec::Role;
use rusqlite::{Connection, OpenFlags, params};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::Arc;

const MAKIBA: &str = env!("CARGO_BIN_EXE_makiba");
const SPEC: &str = include_str!("../docs/design/spec.md");

/// A message from the secretary to the worker; it is also on a captured screen.
const BODY: &str = "private-body-between-two-agents-7f3a";
/// The person key: not hex, as an older key file may hold.
const PERSON_KEY: &str = "person-key-not-hex-91c2";
const GATE_KEY: &str = "gate-key-file-content-55e1";

struct Org {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    /// The worker's token, as `makibad` gives it at launch.
    worker_key: String,
}

impl Org {
    fn state(&self) -> PathBuf {
        self.root.join("state")
    }

    fn db(&self) -> Connection {
        Connection::open_with_flags(
            self.state().join("state.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap()
    }

    fn writer(&self) -> StateWriter {
        StateWriter::open(&self.state().join("state.db")).unwrap()
    }

    fn max_event(&self) -> i64 {
        self.db()
            .query_row("SELECT COALESCE(MAX(id), 0) FROM events", [], |r| r.get(0))
            .unwrap()
    }

    fn commands(&self) -> i64 {
        self.db()
            .query_row("SELECT COUNT(*) FROM commands", [], |r| r.get(0))
            .unwrap()
    }

    /// The person's `makiba`, with no door and no `makibad`.
    fn cli(&self, args: &[&str]) -> Output {
        Command::new(MAKIBA)
            .args(args)
            .env("MAKIBA_STATE_DIR", self.state())
            .env("MAKIBA_PERSON_KEY", self.root.join("config/person.key"))
            .env("MAKIBA_GATE_KEY", self.root.join("config/gate.key"))
            .env("MAKIBA_DOOR", "none")
            .env("MAKIBA_PORT", "0")
            .env("MAKIBA_BASE", "stub")
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    fn json(&self, args: &[&str]) -> Value {
        let out = self.cli(args);
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "makiba {args:?} printed no JSON ({e}): {}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        })
    }
}

/// An organization whose `makibad` is stopped (its heartbeat is an hour old), with one of each
/// thing `status` lists, a message between two agents, and the worker's token, the person key and
/// the gate key written where only a careless read would show them.
fn org() -> Org {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    for d in ["state", "config"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    std::fs::write(root.join("config/person.key"), format!("{PERSON_KEY}\n")).unwrap();
    std::fs::write(root.join("config/gate.key"), format!("{GATE_KEY}\n")).unwrap();
    let w = StateWriter::open(&root.join("state/state.db")).unwrap();
    let c = w.conn();
    c.execute(
        "INSERT INTO agents (agent_id, role, key_sha256) VALUES ('person', 'person', ?1)",
        [makiba::daemon::door::sha256_hex(PERSON_KEY)],
    )
    .unwrap();
    c.execute(
        "INSERT INTO dispatcher (id, pid, started_at, heartbeat_at, subscribed)
         VALUES (1, 4242, strftime('%Y-%m-%dT%H:%M:%fZ','now','-1 hour'),
                 strftime('%Y-%m-%dT%H:%M:%fZ','now','-1 hour'), 1)",
        [],
    )
    .unwrap();
    w.ensure_project("acme", None, Some("https://github.com/acme/widget"))
        .unwrap();
    w.upsert_run(&makiba::db::writer::RunUpsert {
        task_id: "t1",
        project_slug: "acme",
        title: Some("Read surface"),
        status: Some("in_use"),
        ..Default::default()
    })
    .unwrap();
    let run: i64 = c
        .query_row("SELECT id FROM runs WHERE task_id = 't1'", [], |r| r.get(0))
        .unwrap();
    c.execute(
        "INSERT INTO run_launch (run_id, role, spawn_id, kind, agent, brief, worktree, stage)
         VALUES (?1, 'worker', 'spawn-t1', 'code', 'fake', 'the brief', '/w/t1', 'verified')",
        [run],
    )
    .unwrap();
    let worker_key = issue_key(&w, Role::Worker, "w-t1", Some(run), Some("spawn-t1")).unwrap();
    let event = |kind: &str, actor: &str, task: Option<&str>, payload: Value| {
        w.append_event(&NewEvent {
            kind,
            actor: Some(actor),
            payload: Some(&payload),
            run_task_id: task,
            ..Default::default()
        })
        .unwrap()
    };
    event(
        "terminal_status",
        "makibad",
        Some("t1"),
        json!({"task_id": "t1", "role": "worker", "spawn_id": "spawn-t1", "state": "working",
               "kind": null, "msg": format!("token {worker_key} in a status message")}),
    );
    event(
        "anomaly_raised",
        "makibad",
        Some("t1"),
        json!({"task_id": "t1", "rule": "blocked_question"}),
    );
    // A message between the secretary and the worker, typed onto the worker's screen.
    let sent = event("message_sent", "secretary", Some("t1"), json!({}));
    c.execute(
        "INSERT INTO messages (event_id, sender, recipient, kind, run_id, body)
         VALUES (?1, 'secretary', 'w-t1', 'message', ?2, ?3)",
        params![sent, run, BODY],
    )
    .unwrap();
    let screen = format!("$ claude --mcp-config {worker_key}\n{BODY}\nerror");
    event(
        "worker_spawn_failed",
        "makibad",
        Some("t1"),
        json!({"spawn_id": "spawn-old", "stage": "launched", "reason": "no status",
               "screen": screen}),
    );
    // A permission prompt whose captured screen holds the body.
    c.execute(
        "INSERT INTO pending (run_id, kind, question, prompt_text, prompt_sum)
         VALUES (?1, 'prompt', ?2, ?3, 'sum')",
        params![run, format!("w-t1 asks for permission:\n{screen}"), screen],
    )
    .unwrap();
    c.execute(
        "INSERT INTO pending (run_id, kind, question) VALUES (?1, 'question', 'Merge it?')",
        [run],
    )
    .unwrap();
    c.execute(
        "INSERT INTO pr_watches (repo, pr, head_oid, run_id)
         VALUES ('acme/widget', 7, ?1, ?2)",
        params!["a".repeat(40), run],
    )
    .unwrap();
    event(
        "ci_watch_failed",
        "makibad",
        Some("t1"),
        json!({"repo": "acme/widget", "pr": 7, "head_oid": "a".repeat(40),
               "reason": format!("gh said {PERSON_KEY} and {GATE_KEY}")}),
    );
    drop(w);
    Org {
        _tmp: tmp,
        root,
        worker_key,
    }
}

fn entry<'a>(v: &'a Value, array: &str, field: &str, want: Value) -> &'a Value {
    v[array]
        .as_array()
        .unwrap_or_else(|| panic!("no array {array}: {v}"))
        .iter()
        .find(|e| e[field] == want)
        .unwrap_or_else(|| panic!("no {array} entry with {field} = {want}: {v}"))
}

#[test]
fn status_json_lists_what_was_created_with_makibad_stopped() {
    let org = org();
    let out = org.cli(&["status", "--json"]);
    assert!(!out.status.success(), "makibad is stopped, yet exit 0");
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["makibad"]["running"], json!(false), "{v}");

    let run = entry(&v, "runs", "task_id", json!("t1"));
    assert_eq!(run["kind"], json!("code"), "{v}");
    assert_eq!(run["project"], json!("acme"), "{v}");
    assert_eq!(run["status"], json!("in_use"), "{v}");
    assert_eq!(run["stage"], json!("verified"), "{v}");

    let term = entry(&v, "terminals", "name", json!("w-t1"));
    assert_eq!(term["role"], json!("worker"), "{v}");
    assert_eq!(term["state"], json!("working"), "{v}");
    assert!(term["since"].is_string(), "{v}");

    let anomaly = entry(&v, "anomalies", "rule", json!("blocked_question"));
    assert_eq!(anomaly["task_id"], json!("t1"), "{v}");

    let pending = entry(&v, "pending", "question", json!("Merge it?"));
    assert_eq!(pending["task_id"], json!("t1"), "{v}");
    assert_eq!(pending["status"], json!("open"), "{v}");

    let watch = entry(&v, "pr_watches", "pr", json!(7));
    assert_eq!(watch["repo"], json!("acme/widget"), "{v}");
    assert_eq!(watch["task_id"], json!("t1"), "{v}");
}

fn ids(v: &Value) -> Vec<i64> {
    v["events"]
        .as_array()
        .unwrap_or_else(|| panic!("no events array: {v}"))
        .iter()
        .map(|e| e["id"].as_i64().unwrap())
        .collect()
}

#[test]
fn events_after_an_id_are_exactly_the_later_ones_in_order() {
    let org = org();
    let start = org.max_event();
    let w = org.writer();
    let mut written = Vec::new();
    for kind in ["run_taken", "worker_spawned", "run_closed"] {
        written.push(
            w.append_event(&NewEvent {
                kind,
                actor: Some("makibad"),
                payload: Some(&json!({"task_id": "t1"})),
                run_task_id: Some("t1"),
                ..Default::default()
            })
            .unwrap(),
        );
    }
    drop(w);

    let after = start.to_string();
    let v = org.json(&["events", "--after", &after, "--json"]);
    assert_eq!(ids(&v), written, "{v}");
    let kinds: Vec<&str> = v["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["run_taken", "worker_spawned", "run_closed"]);
    let first = &v["events"][0];
    assert_eq!(first["actor"], json!("makibad"), "{v}");
    assert_eq!(first["task_id"], json!("t1"), "{v}");
    assert_eq!(first["payload"], json!({"task_id": "t1"}), "{v}");
    assert!(first["occurred_at"].is_string(), "{v}");

    let v = org.json(&["events", "--after", &after, "--limit", "1", "--json"]);
    assert_eq!(ids(&v), written[..1], "{v}");

    let last = written.last().unwrap().to_string();
    let v = org.json(&["events", "--after", &last, "--json"]);
    assert_eq!(ids(&v), Vec::<i64>::new(), "{v}");
    let past = (written.last().unwrap() + 100).to_string();
    let v = org.json(&["events", "--after", &past, "--json"]);
    assert_eq!(ids(&v), Vec::<i64>::new(), "{v}");
}

#[test]
fn the_person_key_reads_events_at_the_door_unrecorded_and_an_agent_key_is_refused() {
    let org = org();
    let w = org.writer();
    let person = issue_key(&w, Role::Person, "person", None, None).unwrap();
    let secretary = issue_key(&w, Role::Secretary, "secretary", None, None).unwrap();
    // The door runs in this process, whose key files are not the person's: it reads the events
    // written after the fixture's, which hold no key.
    let after = org.max_event();
    for kind in ["run_taken", "run_closed"] {
        w.append_event(&NewEvent {
            kind,
            actor: Some("makibad"),
            payload: Some(&json!({"task_id": "t1"})),
            run_task_id: Some("t1"),
            ..Default::default()
        })
        .unwrap();
    }
    let door = Door::new(w, Arc::new(Stub::new()), "fake".into());
    let local = org.json(&["events", "--after", &after.to_string(), "--json"]);
    assert_eq!(ids(&local).len(), 2, "{local}");

    let (events, commands) = (org.max_event(), org.commands());
    let args = json!({"after": after});
    let first = door
        .handle(Some(&person), "events", args.clone(), None, false)
        .unwrap_or_else(|e| panic!("the person key was refused: {e:?}"));
    let second = door
        .handle(Some(&person), "events", args.clone(), None, false)
        .unwrap();
    assert_eq!(first, local, "the door and the command differ");
    assert_eq!(second, first);
    assert_eq!(org.max_event(), events, "a read wrote an event");
    assert_eq!(org.commands(), commands, "a read wrote a command row");

    for tool in [false, true] {
        let e = door
            .handle(Some(&secretary), "events", args.clone(), None, tool)
            .expect_err("an agent key read the events");
        assert_eq!(e.kind, makiba::spec::ErrorKind::Denied, "{e:?}");
    }
}

#[test]
fn no_output_holds_a_key_a_token_or_a_message_body() {
    let org = org();
    let mut outputs = Vec::new();
    for args in [
        &["status", "--json"][..],
        &["status"],
        &["events", "--after", "0", "--json"],
        &["events", "--after", "0"],
    ] {
        let out = org.cli(args);
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        outputs.push((args, text));
    }
    for (args, text) in &outputs {
        for secret in [org.worker_key.as_str(), PERSON_KEY, GATE_KEY, BODY] {
            assert!(
                !text.contains(secret),
                "makiba {args:?} shows {secret}: {text}"
            );
        }
    }
    // The events were read, not left out: the status message is there with its token masked.
    let (_, events) = &outputs[2];
    assert!(events.contains("in a status message"), "{events}");
    assert!(events.contains("worker_spawn_failed"), "{events}");
}

// ---- the documented shapes -----------------------------------------------------------------

/// The text of `## <n>. ...` up to the next `## ` heading.
fn section(doc: &str, n: u32) -> &str {
    let start = doc
        .find(&format!("\n## {n}. "))
        .unwrap_or_else(|| panic!("spec.md has no section {n}"));
    let rest = &doc[start + 1..];
    match rest[3..].find("\n## ") {
        Some(end) => &rest[..end + 3],
        None => rest,
    }
}

/// The first backticked word of the first cell of each row of the table whose header starts with
/// `first`.
fn documented(doc: &str, first: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut inside = false;
    for line in section(doc, 12).lines() {
        let line = line.trim();
        if !line.starts_with('|') {
            inside = false;
            continue;
        }
        let cell = line.trim_matches('|').split('|').next().unwrap().trim();
        if cell == first {
            inside = true;
            continue;
        }
        if !inside || cell.chars().all(|c| c == '-' || c == ':') {
            continue;
        }
        let name = cell.split('`').nth(1).unwrap_or_else(|| panic!("{line}"));
        out.insert(name.to_string());
    }
    assert!(!out.is_empty(), "no table {first:?} in section 12");
    out
}

/// The field paths of a printed value: `a.b`, `a[].b`; `payload` is opaque.
fn paths(v: &Value, prefix: &str, out: &mut BTreeSet<String>) {
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                let p = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                if k == "payload" || !(x.is_object() || x.is_array()) {
                    out.insert(p);
                } else {
                    paths(x, &p, out);
                }
            }
        }
        Value::Array(a) => {
            for x in a {
                paths(x, &format!("{prefix}[]"), out);
            }
        }
        _ => {
            out.insert(prefix.to_string());
        }
    }
}

fn printed(v: &Value) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    paths(v, "", &mut out);
    out
}

#[test]
fn the_documented_shapes_are_the_printed_ones() {
    let org = org();
    let status = org.json(&["status", "--json"]);
    let events = org.json(&["events", "--after", "0", "--json"]);
    for (header, v) in [("Status field", &status), ("Event field", &events)] {
        assert_eq!(documented(SPEC, header), printed(v), "{header}");
    }

    // A field added to the document only, or printed only, makes them differ.
    let row = "| `runs[].task_id` |";
    assert!(SPEC.contains(row), "spec.md no longer has {row}");
    let more = SPEC.replacen(row, "| `runs[].owner` | x |\n| `runs[].task_id` |", 1);
    assert_ne!(documented(&more, "Status field"), printed(&status));
    let mut less = status.clone();
    for r in less["runs"].as_array_mut().unwrap() {
        r.as_object_mut().unwrap().remove("stage");
    }
    assert_ne!(documented(SPEC, "Status field"), printed(&less));
    let mut less = events.clone();
    for e in less["events"].as_array_mut().unwrap() {
        e.as_object_mut().unwrap().remove("actor");
    }
    assert_ne!(documented(SPEC, "Event field"), printed(&less));
}
