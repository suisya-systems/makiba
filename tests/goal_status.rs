//! Issue 271: `makiba status` shows each project's goal ledger (none, ok with its clause count, or
//! error) and, when a placement through the verb `goal` was recorded, when and by whom.
//!
//! The person's `makiba status` reads `state.db` by itself, so no `makibad` runs: the database is
//! made here, the ledgers are files under a scratch goal folder, and a placement is the event the
//! verb writes.

use makiba::db::writer::{NewEvent, StateWriter};
use serde_json::{Value, json};
use std::process::{Command, Stdio};

const MAKIBA: &str = env!("CARGO_BIN_EXE_makiba");

const LEDGER: &str = "## G1 One\n- unmet if: a\n\n## G2 Two\n- unmet if: b\n";

fn status(state: &std::path::Path, goals: &std::path::Path, json: bool) -> String {
    let mut c = Command::new(MAKIBA);
    c.arg("status");
    if json {
        c.arg("--json");
    }
    let out = c
        .env("MAKIBA_STATE_DIR", state)
        .env("MAKIBA_GOALS_DIR", goals)
        .env("MAKIBA_DOOR", "none")
        .env("MAKIBA_PORT", "0")
        .env("MAKIBA_BASE", "stub")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn status_shows_the_state_of_each_projects_goal_ledger_and_its_last_placement() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let goals = tmp.path().join("goals");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(goals.join("o")).unwrap();
    let w = StateWriter::open(&state.join("state.db")).unwrap();
    for name in ["good", "bad", "none"] {
        w.ensure_project(name, None, Some(&format!("https://github.com/o/{name}")))
            .unwrap();
    }
    std::fs::write(goals.join("o/good.md"), LEDGER).unwrap();
    std::fs::write(goals.join("o/bad.md"), "## G1 No unmet line\n").unwrap();
    w.append_event(&NewEvent {
        kind: "goal_ledger_placed",
        actor: Some("secretary"),
        payload: Some(
            &json!({"repo": "o/good", "clauses": 2, "bytes": LEDGER.len(), "by": "secretary"}),
        ),
        ..Default::default()
    })
    .unwrap();
    drop(w);

    let v: Value = serde_json::from_str(&status(&state, &goals, true)).unwrap();
    let of = |slug: &str| {
        v["projects"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["slug"] == json!(slug))
            .unwrap_or_else(|| panic!("no project {slug}: {v}"))["goal_ledger"]
            .clone()
    };
    let good = of("good");
    assert_eq!(good["state"], json!("ok"), "{good}");
    assert_eq!(good["clauses"], json!(2), "{good}");
    assert_eq!(good["placed_by"], json!("secretary"), "{good}");
    assert!(good["placed_at"].is_string(), "{good}");
    let bad = of("bad");
    assert_eq!(bad["state"], json!("error"), "{bad}");
    assert_eq!(bad["placed_by"], Value::Null, "{bad}");
    assert_eq!(of("none")["state"], json!("none"));

    let text = status(&state, &goals, false);
    let line = |slug: &str| {
        text.lines()
            .find(|l| l.trim_start().starts_with(&format!("{slug}:")))
            .unwrap_or_else(|| panic!("no line for {slug}: {text}"))
            .to_string()
    };
    assert!(
        line("good").contains("goal ledger: ok, 2 clauses, placed "),
        "{text}"
    );
    assert!(line("good").contains("by secretary"), "{text}");
    assert!(
        line("bad").contains("goal ledger: cannot be read"),
        "{text}"
    );
    assert!(line("none").contains("goal ledger: none"), "{text}");
}
