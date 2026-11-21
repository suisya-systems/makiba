//! Issue 202: a renewed secretary learns what the records say, beside the handover.
//!
//! No daemon and no socket: the requests go through the door (`Door::handle`, as an agent's do),
//! and `makibad`'s secretary pass runs on the stand-in base, which lists no terminal. The new
//! secretary's task text is its launch's `brief` in `run_launch`.
//!
//! Rows are named, never counted in total: the report by its message id, the entries by their
//! question, the run by its task.

use makiba::base::stub::Stub;
use makiba::daemon::door::{Door, issue_key};
use makiba::daemon::secretary::Secretary;
use makiba::db::writer::{RunUpsert, StateWriter};
use makiba::spec::Role;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;

/// Today's fixed resume line (spec section 5.3).
const RESUME: &str = "You are the secretary of this makiba organization, launched to take over from the secretary before you. Your rules are appended to your system prompt. First call the inbox tool: messages that waited for the secretary are there. Workers keep running; do not dispatch their tasks again. Then resume from what follows and tell the person you are back.";

const MARK: &str = "--- Read from the records (state.db) at this launch, not from the handover ---";

/// A database with run `t1` ("Write the docs", queued) and two pending entries of it: one open,
/// one answered.
fn records(dir: &Path) -> StateWriter {
    let w = StateWriter::open(&dir.join("state.db")).unwrap();
    w.ensure_project("p", None, None).unwrap();
    w.upsert_run(&RunUpsert {
        task_id: "t1",
        project_slug: "p",
        title: Some("Write the docs"),
        status: Some("queued"),
        ..Default::default()
    })
    .unwrap();
    for (question, status) in [("Merge it?", "open"), ("Keep it?", "answered")] {
        w.conn()
            .execute(
                "INSERT INTO pending (run_id, kind, question, status)
                 VALUES ((SELECT id FROM runs WHERE task_id = 't1'), 'question', ?1, ?2)",
                [question, status],
            )
            .unwrap();
    }
    w
}

fn call(door: &Door, key: &str, verb: &str, args: Value) -> Value {
    door.handle(Some(key), verb, args, None, true)
        .unwrap_or_else(|e| panic!("{verb}: {e:?}"))
}

/// The newest secretary launch's brief, after the secretary passes that add it.
fn launched_brief(w: &StateWriter, dir: &Path, passes: usize) -> String {
    let mut s = Secretary::from_env(dir);
    let base = Stub::new();
    for _ in 0..passes {
        s.pass(w, &base).unwrap();
    }
    w.conn()
        .query_row(
            "SELECT brief FROM run_launch WHERE role = 'secretary' ORDER BY id DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .expect("no secretary launch was added")
}

/// A renewal with a handover that names no task, while `t1` is at `review` and one pending entry
/// is open, and a report delivered to the old secretary just before it: the new secretary's task
/// text holds the handover, then the records part with the report, the run and the entry.
#[test]
fn a_renewal_carries_the_records_after_the_handover() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let w = records(dir);
    let run: i64 = w
        .conn()
        .query_row("SELECT id FROM runs WHERE task_id = 't1'", [], |r| r.get(0))
        .unwrap();
    let person = issue_key(&w, Role::Person, "person", None, None).unwrap();
    let secretary = issue_key(&w, Role::Secretary, "secretary", None, Some("old")).unwrap();
    let worker = issue_key(&w, Role::Worker, "w-t1", Some(run), Some("spawn-t1")).unwrap();
    let door = Door::new(
        StateWriter::open(&dir.join("state.db")).unwrap(),
        Arc::new(Stub::new()),
        "fake".into(),
    );

    // A message delivered long before the renewal, a report just before it, both read by the old
    // secretary; then one that waits (inbox gives it to the new secretary).
    let long_ago = call(
        &door,
        &person,
        "send",
        json!({"to": "secretary", "text": "An old note."}),
    )["id"]
        .as_i64()
        .unwrap();
    let report = call(
        &door,
        &worker,
        "report",
        json!({"state": "done", "task": "t1", "text": "Done."}),
    )["id"]
        .as_i64()
        .unwrap();
    let read = call(&door, &secretary, "inbox", json!({}));
    let read: Vec<i64> = read["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_i64().unwrap())
        .collect();
    assert!(
        read.contains(&long_ago) && read.contains(&report),
        "{read:?}"
    );
    w.conn()
        .execute(
            "UPDATE event_deliveries SET delivered_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-20 minutes')
             WHERE recipient = 'secretary'
               AND source_event_id = (SELECT event_id FROM messages WHERE id = ?1)",
            [long_ago],
        )
        .unwrap();
    let waiting = call(
        &door,
        &person,
        "send",
        json!({"to": "secretary", "text": "Still waiting."}),
    )["id"]
        .as_i64()
        .unwrap();

    let handover = "Nothing is waiting. Talk to the person.";
    call(&door, &secretary, "renew", json!({"handover": handover}));
    let brief = launched_brief(&w, dir, 3);

    assert!(
        brief.starts_with(&format!("{RESUME}\n\n{handover}")),
        "{brief}"
    );
    let at = brief
        .find(MARK)
        .unwrap_or_else(|| panic!("no records part: {brief}"));
    assert!(at > RESUME.len() + handover.len(), "{brief}");
    let records = &brief[at..];
    assert!(
        records.contains(&format!("- #{report} report_done from w-t1 (task t1)")),
        "{brief}"
    );
    assert!(
        records.contains("Runs waiting for review:\n- t1: Write the docs\n"),
        "{brief}"
    );
    assert!(records.contains("(open, task t1): Merge it?"), "{brief}");
    assert!(!records.contains("Keep it?"), "{brief}");
    assert!(
        !records.contains(&format!("- #{long_ago} message")),
        "{brief}"
    );
    assert!(
        !records.contains(&format!("- #{waiting} message")),
        "{brief}"
    );
}

/// A relaunch with no handover (the secretary's launch failed, no renewal ever left one) gets
/// the text it got before Issue 202: the resume line and what state.db holds open.
#[test]
fn a_relaunch_with_no_handover_gets_todays_text() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let w = records(dir);
    w.conn()
        .execute(
            "INSERT INTO run_launch (run_id, role, spawn_id, agent, brief, worktree, stage, failed_reason)
             VALUES (NULL, 'secretary', 'gone', 'fake', 'b', ?1, 'failed', 'x')",
            [dir.join("secretary").to_string_lossy()],
        )
        .unwrap();
    let pending: i64 = w
        .conn()
        .query_row(
            "SELECT id FROM pending WHERE question = 'Merge it?'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    w.conn()
        .execute("UPDATE runs SET status = 'in_use' WHERE task_id = 't1'", [])
        .unwrap();

    let brief = launched_brief(&w, dir, 3);
    assert_eq!(
        brief,
        format!(
            "{RESUME}\n\nNo handover was left. What state.db holds open:\n\nOpen pending entries:\n\
             - #{pending} (open, task t1): Merge it?\n\nRunning tasks:\n- t1: Write the docs\n"
        )
    );
}
