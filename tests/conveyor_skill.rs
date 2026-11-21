//! Issue 188: the conveyor skill for the secretary.
//!
//! - `skills/conveyor/SKILL.md` exists, names no terminal tool, and every tool or command it names
//!   is a verb of the table in spec section 12 (the forms `tests/no_terminal_tools.rs` reads).
//! - The skill names the stop lines of the pull-request skill, and says that the approval mode is
//!   read with `status` before every merge.
//! - `roles/secretary/rules.md` maps the situation to the skill.
//! - The secretary's `status` tool returns the merge approval mode, read as the gate reads it, at
//!   each call: a change reaches a secretary that is already working.

use makiba::base::stub::Stub;
use makiba::daemon::door::{Door, issue_key};
use makiba::daemon::gate;
use makiba::db::writer::StateWriter;
use makiba::spec::Role;
use regex::Regex;
use serde_json::json;
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

fn read(rel: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

fn skill() -> String {
    read("skills/conveyor/SKILL.md")
}

/// The verbs and the agents' tools of spec section 12's table.
fn verb_table() -> (BTreeSet<String>, BTreeSet<String>) {
    let spec = read("docs/design/spec.md");
    let start = spec.find("\n## 12.").expect("spec.md has no section 12");
    let end = start + spec[start..].find("\n## 13.").expect("no section 13");
    let tick = Regex::new(r"^`([a-z]+)`").unwrap();
    let (mut verbs, mut tools) = (BTreeSet::new(), BTreeSet::new());
    for line in spec[start..end].lines().filter(|l| l.starts_with("| ")) {
        let cells: Vec<&str> = line.trim_matches('|').split(" | ").map(str::trim).collect();
        if let Some(c) = tick.captures(cells[0]) {
            verbs.insert(c[1].to_string());
        }
        if let Some(c) = tick.captures(cells[cells.len() - 1]) {
            tools.insert(c[1].to_string());
        }
    }
    assert!(tools.contains("status"), "section 12's table not read");
    (verbs, tools)
}

#[test]
fn the_skill_names_only_verbs_of_section_12() {
    let text = skill();
    let (verbs, tools) = verb_table();
    let mcp = Regex::new(r"mcp__([A-Za-z0-9_-]+?)__([A-Za-z0-9_]+)").unwrap();
    let command = Regex::new(r"`makiba ([a-z_-]+)").unwrap();
    let tool = Regex::new(r"`([A-Za-z0-9_-]+)` tool").unwrap();
    let forbidden = [
        "herdr",
        "send_keys",
        "inspect_pane",
        "list_panes",
        "spawn_pane",
        "close_pane",
        "tmux",
        "wezterm",
        "send_message",
        "check_messages",
    ];
    let mut bad = Vec::new();
    for (n, line) in text.lines().enumerate() {
        for c in mcp.captures_iter(line) {
            if &c[1] != "makiba" || !tools.contains(&c[2]) {
                bad.push(format!("{}: {}", n + 1, &c[0]));
            }
        }
        for c in command.captures_iter(line) {
            if !verbs.contains(&c[1]) {
                bad.push(format!("{}: {}", n + 1, &c[0]));
            }
        }
        for c in tool.captures_iter(line) {
            if !tools.contains(&c[1]) {
                bad.push(format!("{}: {}", n + 1, &c[0]));
            }
        }
        let lower = line.to_lowercase();
        for w in forbidden {
            if lower.contains(w) {
                bad.push(format!("{}: {w}", n + 1));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "skills/conveyor/SKILL.md names what section 12 has not:\n{}",
        bad.join("\n")
    );
    assert!(
        text.contains("mcp__makiba__status"),
        "the skill does not name the status tool it reads the mode with"
    );
}

#[test]
fn the_skill_names_the_stop_lines_and_reads_the_mode_before_every_merge() {
    // The lines it points at exist.
    assert!(
        read("skills/pull-request/SKILL.md").contains("Stop lines:"),
        "skills/pull-request/SKILL.md has no stop lines"
    );
    let text = skill();
    let lines: Vec<String> = text.lines().map(str::to_lowercase).collect();
    assert!(
        lines
            .iter()
            .any(|l| l.contains("stop line") && l.contains("makiba:pull-request")),
        "the skill does not name the stop lines of makiba:pull-request"
    );
    assert!(
        lines.iter().any(|l| l.contains("approval mode")
            && l.contains("before every")
            && l.contains("merge")
            && l.contains("status")),
        "the skill does not say the approval mode is read with status before every merge"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("acceptance") && l.contains("both") && l.contains("human")),
        "the skill does not say acceptance holds only when the scope and status both say it, else human"
    );
}

#[test]
fn the_secretary_rules_map_the_situation_to_the_skill() {
    let rules = read("roles/secretary/rules.md");
    let row = rules
        .lines()
        .find(|l| l.starts_with("| ") && l.contains("`makiba:conveyor`"))
        .expect("roles/secretary/rules.md has no table row for makiba:conveyor");
    assert!(
        row.to_lowercase().contains("scope"),
        "the row does not name the situation (a scope the person approved): {row}"
    );
}

#[test]
fn status_returns_the_merge_approval_mode_read_at_each_call() {
    let tmp = tempfile::tempdir().unwrap();
    let w = StateWriter::open(&tmp.path().join("state.db")).unwrap();
    let key = issue_key(&w, Role::Secretary, "secretary", None, None).unwrap();
    let door = Door::new(w, Arc::new(Stub::new()), "fake".into());
    let mode = |value: Option<&str>| {
        // SAFETY: the only test of this binary that touches the environment.
        unsafe {
            match value {
                Some(v) => std::env::set_var(gate::MODE_ENV, v),
                None => std::env::remove_var(gate::MODE_ENV),
            }
        }
        let r = door
            .handle(Some(&key), "status", json!({}), None, true)
            .unwrap_or_else(|e| panic!("status refused: {e:?}"));
        r["merge_approval_mode"].clone()
    };
    assert_eq!(mode(Some("acceptance")), json!("acceptance"));
    // The same door, a changed setting: the next call says so.
    assert_eq!(mode(Some("human")), json!("human"));
    assert_eq!(mode(Some(" Acceptance ")), json!("acceptance"));
    assert_eq!(mode(Some("yes")), json!("human"));
    assert_eq!(mode(None), json!("human"));
}
