//! Issue 269 (G3, incident I-11, claude-org-ja #1192): the secretary checks the record before it
//! agrees or disagrees with a premise the person states.
//!
//! Two things hold it, and neither is a stop: a rule in the secretary's rules, and a
//! `UserPromptSubmit` hook (`makiba hook premise-check`) that prints one reminder line when the
//! person's message carries a claim. The hook reminds; it cannot see whether a record was
//! opened. It never refuses: exit 0 on every input.

use makiba::agents::{Role, rules_path, write_rules};
use serde_json::{Value, json};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

const RULE: &str = "open the record or the code once and answer with its source";
const LINE: &str = "VERIFY_BEFORE_AGREE";

fn read(rel: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)).unwrap()
}

/// `makiba hook premise-check` with `stdin`: its exit code and standard output.
fn hook(stdin: &str) -> (i32, String) {
    let mut c = Command::new(env!("CARGO_BIN_EXE_makiba"))
        .args(["hook", "premise-check"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    c.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    let out = c.wait_with_output().unwrap();
    (
        out.status.code().unwrap(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

fn prompt(text: &str) -> String {
    json!({"hook_event_name": "UserPromptSubmit", "prompt": text}).to_string()
}

#[test]
fn the_installed_secretary_rules_carry_the_premise_check() {
    let share = tempfile::tempdir().unwrap();
    // SAFETY: the other tests of this file read no environment variable of the process.
    unsafe { std::env::set_var("MAKIBA_SHARE_DIR", share.path()) };
    write_rules(Role::Secretary).unwrap();
    let installed = std::fs::read_to_string(rules_path(Role::Secretary)).unwrap();
    assert!(installed.contains(RULE), "the rule's sentence is missing");
    assert!(
        installed.contains("premise the person states"),
        "the rule does not name the person's premise"
    );
}

#[test]
fn a_message_with_a_claim_gets_the_reminder_line() {
    for text in [
        "I think the threshold should assume a 256K window",
        "The timeout is 900 seconds, right?",
        "the retry limit is set to 5 and MAKIBA_DOOR=none turns the door off",
        "it should be 80% by default",
    ] {
        let (code, out) = hook(&prompt(text));
        assert_eq!(code, 0, "{text}");
        assert_eq!(out.lines().count(), 1, "{text}: {out:?}");
        assert!(out.starts_with(LINE), "{text}: {out:?}");
    }
}

#[test]
fn a_message_without_a_claim_gets_nothing() {
    for text in [
        "please look at the open pull requests",
        "merge #269 for makiba-269-rule-task, opened 2026-11-22 at 28f0504",
        "thanks, go on",
        // Messages from agents and the dispatcher are not the person's premises.
        "<makiba from=\"w-t1\" kind=\"report_done\" id=\"7\">\nthe test takes 90 seconds\n</makiba>",
    ] {
        assert_eq!(hook(&prompt(text)), (0, String::new()), "{text}");
    }
}

#[test]
fn the_hook_never_refuses() {
    for raw in [
        "",
        "  \n",
        "not json{",
        "[]",
        "null",
        "{}",
        r#"{"prompt": 5}"#,
        r#"{"prompt": null}"#,
    ] {
        assert_eq!(hook(raw), (0, String::new()), "{raw:?}");
    }
}

#[test]
fn only_the_secretary_runs_it() {
    let command = |role: &str| -> Vec<String> {
        let v: Value = serde_json::from_str(&read(&format!("roles/{role}/settings.json"))).unwrap();
        v["hooks"]["UserPromptSubmit"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|g| g["hooks"].as_array().into_iter().flatten())
            .filter_map(|h| h["command"].as_str().map(String::from))
            .collect()
    };
    let secretary = command("secretary");
    assert_eq!(secretary.len(), 1, "{secretary:?}");
    assert!(
        secretary[0].ends_with("hook premise-check"),
        "{secretary:?}"
    );
    assert!(command("worker").is_empty());
    assert!(command("curator").is_empty());
}

#[test]
fn the_incident_table_names_this_test_and_says_it_is_not_a_stop() {
    let table = read("docs/design/incidents.md");
    let row = table
        .lines()
        .find(|l| l.contains("claude-org-ja #1192"))
        .expect("no incident row for claude-org-ja #1192");
    assert!(
        row.contains("`tests/premise_check_rule.rs`"),
        "the row does not name its test: {row}"
    );
    assert!(
        row.contains("not a stop"),
        "the row hides what it is: {row}"
    );
}
