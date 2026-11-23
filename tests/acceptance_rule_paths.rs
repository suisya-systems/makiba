//! Issue 361, the diff review's findings: the one rule of what a test commit may change reads a
//! declared path as the merge gate does ("./tests/a.rs" declares tests/a.rs), and reads file names
//! unquoted (a helper with a non-ASCII name is a helper), in `scripts/acceptance-check.sh` and so
//! in the gate that runs it. A declaration of only "." fails both; the gate's pin covers the
//! helpers the test commit changed, and keeps the old digest of a test commit without any.

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

/// The gate's verdict with a pin, and the digest it saw.
fn gate_pinned(dir: &Path, base: &str, pin: Option<&gate::Pin>) -> (Result<(), Reject>, Verdict) {
    let head = git(dir, &["rev-parse", "HEAD"]);
    let mut ci = Ci::new("/nonexistent/gh");
    let failed = |_: &str| -> Result<bool, Reject> { Ok(true) };
    let mut v = Verdict::default();
    let r = gate::evaluate(dir, base, &head, "o/scratch", pin, &mut ci, &failed, &mut v);
    (r, v)
}

#[test]
fn a_declaration_of_only_dot_fails_both_and_is_not_none() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("repo");
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    let base = commit(&dir, "base", &[("src/lib.rs", "pub fn answer() {}\n")]);
    git(&dir, &["checkout", "-q", "-b", "feature"]);
    commit(
        &dir,
        "test\n\nAcceptance-Test: .\nCo-Authored-By: t <t@example.com>",
        &[("tests/answer.rs", "#[test]\nfn a() {}\n")],
    );
    let head = commit(
        &dir,
        "fix",
        &[("src/lib.rs", "pub fn answer() -> u8 { 2 }\n")],
    );

    let (code, full) = script(&dir, &[&base, &head]);
    assert_eq!(code, 1, "{full}");
    assert_eq!(full.lines().last(), Some("ACCEPTANCE: fail"), "{full}");
    let why = gate_at(&dir, &base).expect_err("the gate refuses").0;
    assert!(
        !why.starts_with("cannot read"),
        "the gate let it pass: {why}"
    );
}

#[test]
fn a_rewrite_of_only_a_helper_of_the_test_commit_breaks_the_pin() {
    let (_tmp, dir, base, m) = repo();
    commit(
        &dir,
        "fix",
        &[("src/lib.rs", "pub fn answer() -> u8 { 2 }\n")],
    );
    let (_, first) = gate_pinned(&dir, &base, None);
    let digest = first.manifest_digest.expect("a digest");

    git(&dir, &["checkout", "-q", "-B", "feature", &base]);
    let m2 = commit(
        &dir,
        "test\n\nAcceptance-Test: ./tests/answer.rs\nCo-Authored-By: t <t@example.com>",
        &[
            ("tests/answer.rs", "#[test]\nfn a() {}\n"),
            (HELPER, "{\"y\": 2}\n"),
        ],
    );
    commit(
        &dir,
        "fix",
        &[("src/lib.rs", "pub fn answer() -> u8 { 2 }\n")],
    );
    let pin = gate::Pin {
        id: 1,
        commit: m,
        paths: serde_json::json!(["tests/answer.rs"]),
        digest,
    };
    let (r, _) = gate_pinned(&dir, &base, Some(&pin));
    let why = r.expect_err("the gate refuses").0;
    assert!(
        why.contains("rewritten since the gate first saw it"),
        "{why} (test commit {m2})"
    );
}

#[test]
fn a_test_commit_of_declared_paths_only_keeps_its_old_digest() {
    use sha2::{Digest, Sha256};
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("repo");
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    let base = commit(&dir, "base", &[("src/lib.rs", "pub fn answer() {}\n")]);
    git(&dir, &["checkout", "-q", "-b", "feature"]);
    let m = commit(
        &dir,
        "test\n\nAcceptance-Test: tests/answer.rs\nCo-Authored-By: t <t@example.com>",
        &[("tests/answer.rs", "#[test]\nfn a() {}\n")],
    );
    commit(
        &dir,
        "fix",
        &[("src/lib.rs", "pub fn answer() -> u8 { 2 }\n")],
    );

    let (_, v) = gate_pinned(&dir, &base, None);
    let ls = git(&dir, &["ls-tree", &m, "tests/answer.rs"]);
    let f: Vec<&str> = ls.split_whitespace().collect();
    // The digest before Issue 361: the sorted declared paths and their (path, mode, object).
    let old =
        serde_json::json!([["tests/answer.rs"], [["tests/answer.rs", f[0], f[2]]]]).to_string();
    let want: String = Sha256::digest(old.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(v.manifest_digest, Some(want));
}

/// A base, then a test commit declaring `tests/answer.rs` that also adds `extra` (undeclared).
fn repo_with(extra: &str) -> (tempfile::TempDir, PathBuf, String, String) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("repo");
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    let base = commit(&dir, "base", &[("src/lib.rs", "pub fn answer() {}\n")]);
    git(&dir, &["checkout", "-q", "-b", "feature"]);
    let m = commit(
        &dir,
        "test\n\nAcceptance-Test: tests/answer.rs\nCo-Authored-By: t <t@example.com>",
        &[
            ("tests/answer.rs", "#[test]\nfn a() {}\n"),
            (extra, "code\n"),
        ],
    );
    (tmp, dir, base, m)
}

#[test]
fn an_undeclared_name_of_only_newlines_is_refused_by_both() {
    let (_tmp, dir, base, m) = repo_with("\n");
    commit(
        &dir,
        "fix",
        &[("src/lib.rs", "pub fn answer() -> u8 { 2 }\n")],
    );

    let (code, out) = script(&dir, &["--test-commit", &m]);
    assert_eq!(code, 1, "{out:?}");
    assert_eq!(
        gate::test_commit_refusals(&dir, &m),
        Ok(vec![String::new()])
    );
    let why = gate_at(&dir, &base).expect_err("the gate refuses").0;
    assert!(why.contains("undeclared paths"), "{why}");
}

#[test]
fn a_test_commit_whose_changes_cannot_be_listed_is_refused_by_both() {
    let (_tmp, dir, base, m) = repo_with("src/extra.rs");
    let head = commit(
        &dir,
        "fix",
        &[("src/lib.rs", "pub fn answer() -> u8 { 2 }\n")],
    );
    // The test commit's tree is gone: git diff fails at it.
    let tree = git(&dir, &["rev-parse", &format!("{m}^{{tree}}")]);
    std::fs::remove_file(dir.join(".git/objects").join(&tree[..2]).join(&tree[2..])).unwrap();

    let (code, out) = script(&dir, &["--test-commit", &m]);
    assert_eq!(code, 2, "{out}");
    let (code, full) = script(&dir, &[&base, &head]);
    assert_eq!(code, 1, "{full}");
    assert!(
        full.contains("cannot list the changes of the test commit"),
        "{full}"
    );
    let why = gate::test_commit_refusals(&dir, &m)
        .expect_err("the gate cannot judge")
        .0;
    assert!(why.contains("could not judge the test commit"), "{why}");
}

#[test]
fn a_commit_after_the_test_commit_whose_changes_cannot_be_listed_fails_the_script() {
    let (_tmp, dir, base, _m) = repo_with("tests/common/extra.rs");
    let head = commit(&dir, "fix", &[("tests/common/extra.rs", "changed\n")]);
    let tree = git(&dir, &["rev-parse", &format!("{head}^{{tree}}")]);
    std::fs::remove_file(dir.join(".git/objects").join(&tree[..2]).join(&tree[2..])).unwrap();

    let (code, full) = script(&dir, &[&base, &head]);
    assert_eq!(code, 1, "{full}");
    assert!(
        full.contains("cannot list the changes of the commits after the test commit"),
        "{full}"
    );
}
