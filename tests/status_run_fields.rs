//! Issue 407: a run on `makiba status --json` carries its title, start, Issue and range, so that a
//! one-off task (not a range's item) shows them as a range's item does.
//!
//! `runs[].title` is `runs.title`; `runs[].since` is `runs.dispatched_at`; `runs[].issue` is the
//! number of the first `issue_refs` entry `<the run's project>#<N>`, null when there is none or
//! the refs are not a JSON array; `runs[].range_id` is `runs.range_id`, null for a one-off task.
//! Every key is present, null when none.
//!
//! No `makibad` runs here: the state is written with `StateWriter` and read by the person's
//! `makiba status --json`, which needs no door.

use makiba::db::writer::{RunUpsert, StateWriter};
use rusqlite::params;
use serde_json::{Value, json};
use std::process::{Command, Stdio};

const MAKIBA: &str = env!("CARGO_BIN_EXE_makiba");

/// A run to write: task id, title, status, `issue_refs` as stored (raw text), in a range or not,
/// and its dispatch time.
struct Seed {
    task: &'static str,
    title: &'static str,
    status: &'static str,
    refs: Option<&'static str>,
    in_range: bool,
    at: &'static str,
}

const SEEDS: &[Seed] = &[
    Seed {
        task: "r1-5",
        title: "Range item",
        status: "in_use",
        refs: Some(r#"["acme#5"]"#),
        in_range: true,
        at: "2026-11-20T10:00:00.000Z",
    },
    Seed {
        task: "solo",
        title: "One-off fix",
        status: "in_use",
        refs: Some(r#"["acme#12"]"#),
        in_range: false,
        at: "2026-11-21T11:00:00.000Z",
    },
    Seed {
        task: "bare",
        title: "No issue",
        status: "failed",
        refs: None,
        in_range: false,
        at: "2026-11-22T12:00:00.000Z",
    },
    Seed {
        task: "foreign",
        title: "Other repo",
        status: "in_use",
        refs: Some(r#"["beta#9"]"#),
        in_range: false,
        at: "2026-11-23T13:00:00.000Z",
    },
    Seed {
        task: "mixed",
        title: "Several refs",
        status: "in_use",
        refs: Some(r#"["beta#9","acme#5","acme#12"]"#),
        in_range: false,
        at: "2026-11-23T14:00:00.000Z",
    },
    Seed {
        task: "garbled",
        title: "Garbled refs",
        status: "in_use",
        refs: Some("not json"),
        in_range: false,
        at: "2026-11-23T15:00:00.000Z",
    },
    Seed {
        task: "object",
        title: "Object refs",
        status: "in_use",
        refs: Some(r#"{"acme#3":1}"#),
        in_range: false,
        at: "2026-11-23T16:00:00.000Z",
    },
];

#[test]
fn a_one_off_and_a_range_run_carry_title_since_issue_and_range() {
    let tmp = tempfile::tempdir().unwrap();
    let state = std::fs::canonicalize(tmp.path()).unwrap().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let w = StateWriter::open(&state.join("state.db")).unwrap();
    let c = w.conn();
    c.execute(
        "INSERT INTO dispatcher (id, pid, started_at, heartbeat_at, subscribed)
         VALUES (1, 4242, strftime('%Y-%m-%dT%H:%M:%fZ','now','-1 hour'),
                 strftime('%Y-%m-%dT%H:%M:%fZ','now','-1 hour'), 1)",
        [],
    )
    .unwrap();
    w.ensure_project("acme", None, Some("https://github.com/acme/widget"))
        .unwrap();
    c.execute(
        "INSERT INTO ranges (project_id, at_once, budget_tasks, brief_head, proposed_by, status)
         VALUES ((SELECT id FROM projects WHERE slug = 'acme'), 1, 5, 'head', 'secretary', 'active')",
        [],
    )
    .unwrap();
    let range = c.last_insert_rowid();
    for s in SEEDS {
        w.upsert_run(&RunUpsert {
            task_id: s.task,
            project_slug: "acme",
            title: Some(s.title),
            status: Some(s.status),
            ..Default::default()
        })
        .unwrap();
        c.execute(
            "UPDATE runs SET dispatched_at = ?2, issue_refs = ?3, range_id = ?4 WHERE task_id = ?1",
            params![s.task, s.at, s.refs, s.in_range.then_some(range)],
        )
        .unwrap();
    }
    drop(w);

    let out = Command::new(MAKIBA)
        .args(["status", "--json"])
        .env("MAKIBA_STATE_DIR", &state)
        .env("MAKIBA_DOOR", "none")
        .env("MAKIBA_PORT", "0")
        .env("MAKIBA_BASE", "stub")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "makiba status --json printed no JSON ({e}): {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    });
    let runs = v["runs"].as_array().expect("runs[] is an array");
    let run = |task: &str| -> &Value {
        runs.iter()
            .find(|r| r["task_id"] == json!(task))
            .unwrap_or_else(|| panic!("{task} is not in runs[]: {runs:?}"))
    };

    // 1. A range's run.
    let r = run("r1-5");
    assert_eq!(r["title"], json!("Range item"), "{r}");
    assert_eq!(r["since"], json!("2026-11-20T10:00:00.000Z"), "{r}");
    assert_eq!(r["issue"], json!(5), "{r}");
    assert_eq!(r["range_id"], json!(range), "{r}");

    // 2. A one-off task with an Issue.
    let r = run("solo");
    assert_eq!(r["title"], json!("One-off fix"), "{r}");
    assert_eq!(r["since"], json!("2026-11-21T11:00:00.000Z"), "{r}");
    assert_eq!(r["issue"], json!(12), "{r}");
    assert_eq!(r["range_id"], Value::Null, "{r}");

    // 3. A one-off task with no Issue.
    let r = run("bare");
    assert_eq!(r["title"], json!("No issue"), "{r}");
    assert_eq!(r["since"], json!("2026-11-22T12:00:00.000Z"), "{r}");
    assert_eq!(r["issue"], Value::Null, "{r}");
    assert_eq!(r["range_id"], Value::Null, "{r}");

    // 4. A ref to another project's Issue is not this run's Issue.
    let r = run("foreign");
    assert_eq!(r["title"], json!("Other repo"), "{r}");
    assert_eq!(r["issue"], Value::Null, "{r}");
    assert_eq!(r["range_id"], Value::Null, "{r}");

    // 5. The first ref of the run's own project.
    assert_eq!(run("mixed")["issue"], json!(5));

    // 6 and 7. Refs that are not a JSON array give no Issue.
    assert_eq!(run("garbled")["issue"], Value::Null);
    assert_eq!(run("object")["issue"], Value::Null);

    // 8. Every key is present, also when null.
    for s in SEEDS {
        let r = run(s.task).as_object().unwrap();
        for k in ["title", "since", "issue", "range_id"] {
            assert!(r.contains_key(k), "{}: no {k} in {r:?}", s.task);
        }
    }
}
