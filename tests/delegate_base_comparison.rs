//! Issue 635 (G2): a base comparison builds only the failing targets and removes its worktree.
//!
//! A brief that asks a worker to compare its failing set with the base led workers to run the
//! whole suite at the base, which built a second full target folder (21 to 24 GB a task, measured
//! on 2026-11-28) and left it behind. `skills/delegate/SKILL.md`, in its guidance on the brief,
//! says how the comparison is run and how it ends. This test reads that text only.

use std::path::Path;

/// What the guidance on the brief says a base comparison runs, and where.
const RUNS: [&str; 7] = [
    "base comparison",
    "only the targets that fail at the worker's head",
    "`cargo test --locked --no-fail-fast --test <t>`",
    "`cargo nextest run --locked --no-fail-fast --test <t>`",
    "`--lib` in place of `--test <t>`",
    "a detached worktree under `$TMPDIR`",
    "its own `CARGO_TARGET_DIR` inside it",
];

/// How it ends.
const ENDS: [&str; 2] = [
    "ends by removing that worktree (`git worktree remove --force",
    "and its target folder",
];

/// The section "The brief" of the delegate skill, with every run of whitespace collapsed to one
/// space, so that line wrapping does not matter.
fn the_brief_section() -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("skills/delegate/SKILL.md");
    let text =
        std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()));
    let start = text
        .find("\n## The brief\n")
        .expect("skills/delegate/SKILL.md has no section \"The brief\"");
    let end = start
        + 1
        + text[start + 1..]
            .find("\n## ")
            .expect("no section after \"The brief\"");
    text[start..end]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn assert_says(phrases: &[&str]) {
    let section = the_brief_section();
    for phrase in phrases {
        assert!(
            section.contains(phrase),
            "the delegate skill's guidance on the brief does not say {phrase:?}"
        );
    }
}

#[test]
fn a_base_comparison_runs_only_the_failing_targets_in_its_own_worktree() {
    assert_says(&RUNS);
}

#[test]
fn a_base_comparison_ends_by_removing_its_worktree_and_target_folder() {
    assert_says(&ENDS);
}
