//! Issue 361, the diff review's findings: the one rule of what a test commit may change reads a
//! declared path as the merge gate does ("./tests/a.rs" declares tests/a.rs), and reads file names
//! unquoted (a helper with a non-ASCII name is a helper), in `scripts/acceptance-check.sh` and so
//! in the gate that runs it.

use makiba::daemon::gate::{self, Ci, Reject, Verdict};
use std::path::{Path, PathBuf};
use std::process::Command;

const HELPER: &str = "tests/common/\u{65e5}\u{672c}.json";

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn commit(dir: &Path, msg: &str, files: &[(&str, &str)]) -> String {
    for (p, text) in files {
        let path = dir.join(p);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        git(dir, &["add", p]);
    }
    git(dir, &["commit", "-q", "-m", msg]);
    git(dir, &["rev-parse", "HEAD"])
}

/// A base, then a test commit declaring `./tests/answer.rs` that also adds a non-ASCII helper.
fn repo() -> (tempfile::TempDir, PathBuf, String, String) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("repo");
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    let base = commit(&dir, "base", &[("src/lib.rs", "pub fn answer() {}\n")]);
    git(&dir, &["checkout", "-q", "-b", "feature"]);
    let m = commit(
        &dir,
        "test\n\nAcceptance-Test: ./tests/answer.rs\nCo-Authored-By: t <t@example.com>",
        &[
            ("tests/answer.rs", "#[test]\nfn a() {}\n"),
            (HELPER, "{}\n"),
        ],
    );
    (tmp, dir, base, m)
}

fn script(dir: &Path, args: &[&str]) -> (i32, String) {
    let out = Command::new("bash")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/acceptance-check.sh"))
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    (out.status.code().unwrap_or(-1), text)
}

fn gate_at(dir: &Path, base: &str) -> Result<(), Reject> {
    let head = git(dir, &["rev-parse", "HEAD"]);
    let mut ci = Ci::new("/nonexistent/gh");
    let failed = |_: &str| -> Result<bool, Reject> { Ok(true) };
    let mut v = Verdict::default();
    gate::evaluate(
        dir,
        base,
        &head,
        "o/scratch",
        None,
        &mut ci,
        &failed,
        &mut v,
    )
}

#[test]
fn a_dot_slash_declaration_and_a_non_ascii_helper_pass_both() {
    let (_tmp, dir, base, m) = repo();
    commit(
        &dir,
        "fix",
        &[("src/lib.rs", "pub fn answer() -> u8 { 2 }\n")],
    );

    assert_eq!(script(&dir, &["--test-commit", &m]), (0, String::new()));
    assert_eq!(gate::test_commit_refusals(&dir, &m), Ok(vec![]));
    let why = gate_at(&dir, &base).expect_err("GitHub cannot be read").0;
    assert!(why.starts_with("cannot read"), "{why}");
    let head = git(&dir, &["rev-parse", "HEAD"]);
    let (_, full) = script(&dir, &[&base, &head]);
    assert!(
        !full.contains("neither declared nor a test helper"),
        "{full}"
    );
}

#[test]
fn a_non_ascii_helper_changed_after_the_test_commit_is_refused_by_both() {
    let (_tmp, dir, base, _m) = repo();
    commit(&dir, "fix", &[(HELPER, "{\"x\": 1}\n")]);

    let head = git(&dir, &["rev-parse", "HEAD"]);
    let (_, full) = script(&dir, &[&base, &head]);
    assert!(
        full.contains(&format!("{HELPER} changed after the test commit")),
        "{full}"
    );
    let why = gate_at(&dir, &base).expect_err("the gate refuses").0;
    assert!(
        why.contains("protected paths changed after the test commit"),
        "{why}"
    );
}
