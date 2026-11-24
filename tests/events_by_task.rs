//! Issue 413: `makiba events --task <task_id>` returns only that task's events, with the shape of
//! `makiba events` (Issue 168), so that a screen's "History" of one task need not filter the whole
//! ledger itself.
//!
//! No `makibad` runs here: the state is written with `StateWriter` and the person's `makiba
//! events` reads `state.db` read-only by itself.

use makiba::db::writer::{NewEvent, RunUpsert, StateWriter};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

const MAKIBA: &str = env!("CARGO_BIN_EXE_makiba");

struct Org {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl Org {
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

    /// The JSON `makiba <args>` prints, and that it exited 0.
    fn json(&self, args: &[&str]) -> Value {
        let out = self.cli(args);
        assert!(
            out.status.success(),
            "makiba {args:?} failed: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "makiba {args:?} printed no JSON ({e}): {}",
                String::from_utf8_lossy(&out.stdout)
            )
        })
    }
}

/// The written event ids: t1's, and every one in order.
struct Written {
    t1: Vec<i64>,
    all: Vec<i64>,
}

/// Runs `t1`, `t2` and `t10` (a name that `t1` is a prefix of), and their events interleaved
/// with one event of no run.
fn org() -> (Org, Written) {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    for d in ["state", "config"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    std::fs::write(root.join("config/person.key"), "person-key-413\n").unwrap();
    std::fs::write(root.join("config/gate.key"), "gate-key-413\n").unwrap();
    let w = StateWriter::open(&root.join("state/state.db")).unwrap();
    w.ensure_project("acme", None, Some("https://github.com/acme/widget"))
        .unwrap();
    for task in ["t1", "t2", "t10"] {
        w.upsert_run(&RunUpsert {
            task_id: task,
            project_slug: "acme",
            title: Some("Events by task"),
            status: Some("in_use"),
            ..Default::default()
        })
        .unwrap();
    }
    let script: [(&str, Option<&str>, Value); 8] = [
        ("run_taken", Some("t1"), json!({"task_id": "t1"})),
        ("run_taken", Some("t2"), json!({"task_id": "t2"})),
        ("dispatcher_started", None, json!({"pid": 1})),
        (
            "worker_spawned",
            Some("t1"),
            json!({"spawn_id": "spawn-t1"}),
        ),
        ("run_taken", Some("t10"), json!({"task_id": "t10"})),
        ("run_closed", Some("t2"), json!({"task_id": "t2"})),
        (
            "worker_spawn_failed",
            Some("t1"),
            json!({"spawn_id": "spawn-t1", "stage": "launched", "reason": "no status",
                   "screen": "a captured screen"}),
        ),
        ("run_closed", Some("t1"), json!({"task_id": "t1"})),
    ];
    let mut written = Written {
        t1: Vec::new(),
        all: Vec::new(),
    };
    for (kind, task, payload) in &script {
        let id = w
            .append_event(&NewEvent {
                kind,
                actor: Some("makibad"),
                payload: Some(payload),
                run_task_id: *task,
                ..Default::default()
            })
            .unwrap();
        written.all.push(id);
        if *task == Some("t1") {
            written.t1.push(id);
        }
    }
    drop(w);
    (Org { _tmp: tmp, root }, written)
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
fn events_of_one_task_are_its_own_in_order_with_the_same_shape() {
    let (org, written) = org();
    let all = org.json(&["events", "--after", "0", "--json"]);
    assert_eq!(ids(&all), written.all, "{all}");

    let v = org.json(&["events", "--after", "0", "--task", "t1", "--json"]);
    assert_eq!(ids(&v), written.t1, "{v}");
    for e in v["events"].as_array().unwrap() {
        assert_eq!(e["task_id"], json!("t1"), "{v}");
        let same = all["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["id"] == e["id"])
            .unwrap_or_else(|| panic!("{e} is not in the unfiltered read: {all}"));
        assert_eq!(
            e, same,
            "the filtered entry differs from the unfiltered one"
        );
    }
    let failed = v["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == json!("worker_spawn_failed"))
        .unwrap_or_else(|| panic!("no worker_spawn_failed: {v}"));
    assert_eq!(failed["payload"]["screen"], json!("<screen>"), "{v}");
}

#[test]
fn after_and_limit_apply_to_the_task_s_events() {
    let (org, written) = org();
    let after = written.t1[1].to_string();
    let v = org.json(&["events", "--after", &after, "--task", "t1", "--json"]);
    assert_eq!(ids(&v), written.t1[2..], "{v}");

    // Two events of other runs lie between t1's first two: a limit taken before the filter
    // would return one.
    let v = org.json(&[
        "events", "--after", "0", "--task", "t1", "--limit", "2", "--json",
    ]);
    assert_eq!(ids(&v), written.t1[..2], "{v}");
}

#[test]
fn a_task_with_no_events_gives_an_empty_list() {
    let (org, _) = org();
    let v = org.json(&["events", "--after", "0", "--task", "nope", "--json"]);
    assert_eq!(v, json!({"events": []}));
}

#[test]
fn the_text_form_lists_the_task_s_events_only() {
    let (org, written) = org();
    let out = org.cli(&["events", "--after", "0", "--task", "t1"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{text}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), written.t1.len(), "{text}");
    for line in lines {
        assert_eq!(line.split(' ').nth(4), Some("t1"), "{line}");
    }
}
