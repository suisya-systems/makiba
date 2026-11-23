//! Issue 341 (G3, incident I-17): a cause, a reading or a summary that comes from outside makiba
//! reaches a worker only as a hypothesis, with where it came from, and the worker establishes the
//! cause from the primary record before it uses it as a finding.
//!
//! The rule lives in text the agents read: the secretary's rules, the delegate and escalation
//! skills (the sending side) and the worker's rules (the receiving side). This test pins it there,
//! so that removing it from any of the four files fails.

use makiba::agents::{Role, rules_path, write_rules};
use std::path::Path;

/// What the sending side says: marked as a hypothesis, with its source, and the worker is asked to
/// establish the cause and say whether the hypothesis held.
const SENDER: [&str; 5] = [
    "outside makiba",
    "only marked as a hypothesis",
    "where it came from",
    "establish the cause from the primary record",
    "whether the hypothesis held",
];

/// What the receiving side says: a hypothesis is checked before it is used as a finding.
const RECEIVER: [&str; 5] = [
    "outside makiba",
    "a hypothesis, not a finding",
    "the primary record",
    "before you use it as a finding",
    "whether it held",
];

/// The file at `rel` in the repository, with every run of whitespace collapsed to one space, so
/// that line wrapping does not matter.
fn read(rel: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    let text =
        std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()));
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn assert_carries(rel: &str, phrases: &[&str]) {
    let text = read(rel);
    for phrase in phrases {
        assert!(
            text.contains(phrase),
            "{rel} does not carry the outside-reading rule: {phrase:?} is missing"
        );
    }
}

#[test]
fn the_secretary_rules_send_an_outside_reading_as_a_hypothesis() {
    assert_carries("roles/secretary/rules.md", &SENDER);
}

#[test]
fn the_delegate_skill_sends_an_outside_reading_as_a_hypothesis() {
    assert_carries("skills/delegate/SKILL.md", &SENDER);
}

#[test]
fn the_escalation_skill_sends_an_outside_reading_as_a_hypothesis() {
    assert_carries("skills/escalation/SKILL.md", &SENDER);
}

#[test]
fn the_worker_rules_check_an_outside_hypothesis() {
    assert_carries("roles/worker/rules.md", &RECEIVER);
}

#[test]
fn the_installed_rules_carry_the_rule() {
    let share = tempfile::tempdir().unwrap();
    // SAFETY: the other tests of this file read no environment variable of the process.
    unsafe { std::env::set_var("MAKIBA_SHARE_DIR", share.path()) };
    write_rules(Role::Secretary).unwrap();
    write_rules(Role::Worker).unwrap();
    let flat = |role| {
        let text = std::fs::read_to_string(rules_path(role)).unwrap();
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    };
    assert!(
        flat(Role::Secretary).contains("only marked as a hypothesis"),
        "the installed secretary rules do not carry the outside-reading rule"
    );
    assert!(
        flat(Role::Worker).contains("a hypothesis, not a finding"),
        "the installed worker rules do not carry the outside-reading rule"
    );
}
