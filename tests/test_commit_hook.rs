//! Issue 322: the commit-msg hook makibad writes for a code run checks a test commit when it is
//! made (line L-K9 of docs/design/lines.md). A commit whose message declares `Acceptance-Test:` is
//! refused when git does not read that line as a trailer (pull request 308: a blank line before
//! `Co-Authored-By`), when a declared path is not a file the commit adds or changes, when a
//! declared Rust test starts makibad other than through `tests/common/makibad.rs` (pull requests
//! 315 and 316), and when the project has a prepare-commit-msg hook of its own (it runs after git
//! wrote the commit's tree, so the index the hook reads may not be the commit). An ordinary
//! commit is untouched.
//!
//! The hook scripts are the ones `makiba::hooks::git_hook::install` writes, copied into place
//! through `tests/common/exe.rs`. The text of a direct start is built at run time, so that this
//! file passes the same scan.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const MAKIBA: &str = env!("CARGO_BIN_EXE_makiba");

/// The start of every refusal of this check.
const REFUSED: &str = "makiba: refused by hook commit-msg (line L-K9 in docs/design/lines.md)";

const CO: &str = "Co-Authored-By: t <t@example.com>";

/// A direct start of makibad, as a test would write it.
fn direct_call() -> String {
    format!("makiba::{}{}::run_until(&dir, &base, stop);", "dae", "mon")
}

/// A test file whose line 3 is `call`.
fn test_file(head: &str, call: &str) -> String {
    format!("{head}\n#[test]\nfn t() {{ {call} }}\n")
}

fn plain_test() -> String {
    test_file("// a plain test", "assert_eq!(1, 1);")
}

/// A repository whose hooks folder holds the scripts makibad writes for a code run.
struct Repo {
    _dir: tempfile::TempDir,
    root: PathBuf,
    wt: PathBuf,
}

impl Repo {
    fn new() -> Repo {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let wt = root.join("wt");
        std::fs::create_dir_all(wt.join("tests")).unwrap();
        let repo = Repo {
            _dir: dir,
            root,
            wt,
        };
        repo.git(&["init", "-q"]);
        std::fs::write(repo.wt.join("README"), "a\n").unwrap();
        repo.git(&["add", "README"]);
        repo.git(&["commit", "-q", "--no-verify", "-m", "init"]);
        // The scripts as makibad writes them, staged outside the hooks folder and never run there.
        let staging = repo.root.join("staging");
        makiba::hooks::git_hook::install(&staging, Path::new(MAKIBA)).unwrap();
        let hooks = repo.root.join("hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        for e in std::fs::read_dir(&staging).unwrap().flatten() {
            common::exe::copy(&e.path(), &hooks.join(e.file_name()));
        }
        repo.git(&["config", "core.hooksPath", hooks.to_str().unwrap()]);
        repo
    }

    fn git_out(&self, args: &[&str]) -> Output {
        Command::new("git")
            .arg("-C")
            .arg(&self.wt)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    fn git(&self, args: &[&str]) -> String {
        let out = self.git_out(args);
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    fn head(&self) -> String {
        self.git(&["rev-parse", "HEAD"])
    }

    /// Stage `text` as `rel`, commit it with `message` (through a file, with `extra` options);
    /// the commit's output.
    fn commit(&self, rel: &str, text: &str, message: &str, extra: &[&str]) -> Output {
        std::fs::write(self.wt.join(rel), text).unwrap();
        self.git(&["add", "--", rel]);
        let msg = self.root.join("message");
        std::fs::write(&msg, message).unwrap();
        let mut args = vec!["commit", "-q"];
        args.extend_from_slice(extra);
        args.extend(["-F", msg.to_str().unwrap()]);
        self.git_out(&args)
    }

    fn declared(&self) -> String {
        self.git(&[
            "log",
            "-1",
            "--format=%(trailers:key=Acceptance-Test,valueonly)",
        ])
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Assert that `out` was refused by this check and its standard error holds each of `parts`.
fn refused(out: &Output, parts: &[&str]) {
    let err = stderr(out);
    assert!(!out.status.success(), "the commit passed: {err}");
    assert!(err.contains(REFUSED), "not refused by the check: {err}");
    for p in parts {
        assert!(err.contains(p), "the refusal does not name {p:?}: {err}");
    }
}

fn passed(repo: &Repo, before: &str, out: &Output) {
    let err = stderr(out);
    assert!(out.status.success(), "the commit was refused: {err}");
    assert!(!err.contains("refused"), "{err}");
    assert_ne!(repo.head(), before, "no commit was made");
}

/// Pull request 308: the blank line puts `Acceptance-Test` out of the trailer block.
#[test]
fn a_test_commit_with_a_blank_line_before_co_authored_by_is_refused() {
    let repo = Repo::new();
    let before = repo.head();
    let out = repo.commit(
        "tests/t.rs",
        &plain_test(),
        &format!("test: declare\n\nAcceptance-Test: tests/t.rs\n\n{CO}\n"),
        &[],
    );
    refused(
        &out,
        &["not read as a trailer", "Acceptance-Test: tests/t.rs"],
    );
    assert_eq!(repo.head(), before, "a commit was made");
}

#[test]
fn the_block_as_one_paragraph_passes() {
    let repo = Repo::new();
    let before = repo.head();
    let out = repo.commit(
        "tests/t.rs",
        &plain_test(),
        &format!("test: declare\n\nAcceptance-Test: tests/t.rs\n{CO}\n"),
        &[],
    );
    passed(&repo, &before, &out);
    assert_eq!(repo.declared(), "tests/t.rs");
}

/// Pull requests 315 and 316: a declared test that starts makibad itself.
#[test]
fn a_declared_test_calling_run_until_directly_is_refused() {
    let repo = Repo::new();
    let before = repo.head();
    let out = repo.commit(
        "tests/t.rs",
        &test_file("// starts makibad itself", &direct_call()),
        &format!("test: declare\n\nAcceptance-Test: tests/t.rs\n{CO}\n"),
        &[],
    );
    refused(
        &out,
        &["tests/t.rs:3:", "run_until(", "tests/common/makibad.rs"],
    );
    assert_eq!(repo.head(), before, "a commit was made");
}

#[test]
fn a_declared_test_through_the_helper_passes() {
    let repo = Repo::new();
    let before = repo.head();
    let out = repo.commit(
        "tests/t.rs",
        &test_file(
            "mod common;\nuse common::makibad;",
            "makibad::run_until(&dir, &base, stop);",
        ),
        &format!("test: declare\n\nAcceptance-Test: tests/t.rs\n{CO}\n"),
        &[],
    );
    passed(&repo, &before, &out);
}

#[test]
fn a_declared_path_the_commit_does_not_change_is_refused() {
    let repo = Repo::new();
    let before = repo.head();
    let out = repo.commit(
        "tests/t.rs",
        &plain_test(),
        &format!("test: declare\n\nAcceptance-Test: tests/other.rs\n{CO}\n"),
        &[],
    );
    refused(&out, &["Acceptance-Test: tests/other.rs"]);
    assert_eq!(repo.head(), before, "a commit was made");
}

#[test]
fn an_ordinary_commit_is_untouched() {
    let repo = Repo::new();
    let before = repo.head();
    let out = repo.commit(
        "tests/t.rs",
        &test_file("// starts makibad itself", &direct_call()),
        &format!("fix: a thing\n\nNo Acceptance-Test: line here.\n\n{CO}\n"),
        &[],
    );
    passed(&repo, &before, &out);
}

/// Git reads `Acceptance-Test : x` as the key `Acceptance-Test`.
#[test]
fn a_spaced_key_is_checked_too() {
    let repo = Repo::new();
    let before = repo.head();
    let out = repo.commit(
        "tests/t.rs",
        &test_file("// starts makibad itself", &direct_call()),
        &format!("test: declare\n\nAcceptance-Test : tests/t.rs\n{CO}\n"),
        &[],
    );
    refused(&out, &["tests/t.rs:3:"]);
    assert_eq!(repo.head(), before, "a commit was made");
}

/// What git cuts at the scissors line is not in the message, so it declares nothing.
#[test]
fn a_line_below_the_scissors_is_not_counted() {
    let repo = Repo::new();
    let before = repo.head();
    let out = repo.commit(
        "tests/t.rs",
        &plain_test(),
        &format!(
            "test: declare\n\nAcceptance-Test: tests/t.rs\n{CO}\n\
             # ------------------------ >8 ------------------------\n\
             Acceptance-Test: tests/other.rs\n"
        ),
        &["--cleanup=scissors"],
    );
    passed(&repo, &before, &out);
    assert_eq!(repo.declared(), "tests/t.rs");
}

/// A project's prepare-commit-msg runs after git wrote the commit's tree; what it stages is in
/// the index the hook reads and not in the commit, so a test commit is refused in such a run.
#[test]
fn a_project_prepare_commit_msg_hook_refuses_a_test_commit() {
    let repo = Repo::new();
    let project = repo.root.join("project-hooks");
    std::fs::create_dir_all(&project).unwrap();
    let hook = project.join("prepare-commit-msg");
    common::exe::write(&hook, "#!/bin/sh\nexit 0\n");
    repo.git(&[
        "config",
        makiba::hooks::git_hook::PROJECT_KEY,
        project.to_str().unwrap(),
    ]);
    let before = repo.head();
    let out = repo.commit(
        "tests/t.rs",
        &plain_test(),
        &format!("test: declare\n\nAcceptance-Test: tests/t.rs\n{CO}\n"),
        &[],
    );
    refused(&out, &["prepare-commit-msg", hook.to_str().unwrap()]);
    assert_eq!(repo.head(), before, "a commit was made");
    // An ordinary commit in the same run passes.
    let out = repo.commit("notes.txt", "a note\n", "docs: a note\n", &[]);
    passed(&repo, &before, &out);
}

#[test]
fn lines_md_names_the_hook_as_a_holder_of_l_k9() {
    let doc =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/design/lines.md"))
            .unwrap();
    let row = doc
        .lines()
        .find(|l| l.starts_with("| L-K9 "))
        .expect("lines.md has the row L-K9");
    assert!(row.contains("hook commit-msg"), "{row}");
    assert!(
        row.contains(
            "tests/test_commit_hook.rs::a_test_commit_with_a_blank_line_before_co_authored_by_is_refused"
        ),
        "{row}"
    );
}
