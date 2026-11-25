//! Issue 322, diff review round 1: two more cases of the commit-msg hook's check of a test
//! commit (line L-K9; the declared cases are in `tests/test_commit_hook.rs`).
//!
//! - A message-only `git commit --amend` of a valid test commit passes: the index equals HEAD's
//!   tree, so the declared paths are judged by what HEAD itself changes, the commit being amended.
//! - A project `commit-msg` hook runs after git wrote the commit's tree, between the scanner's two
//!   checks: it could add a declaration and stage its file, which the commit then does not hold.
//!   A declaring commit is refused in such a run, as with a project `prepare-commit-msg`.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const MAKIBA: &str = env!("CARGO_BIN_EXE_makiba");

const REFUSED: &str = "makiba: refused by hook commit-msg (line L-K9 in docs/design/lines.md)";

const MESSAGE: &str =
    "test: declare\n\nAcceptance-Test: tests/t.rs\nCo-Authored-By: t <t@example.com>\n";

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

    /// Commit the staged state with `message` through a file and `extra` options.
    fn commit(&self, message: &str, extra: &[&str]) -> Output {
        let msg = self.root.join("message");
        std::fs::write(&msg, message).unwrap();
        let mut args = vec!["commit", "-q"];
        args.extend_from_slice(extra);
        args.extend(["-F", msg.to_str().unwrap()]);
        self.git_out(&args)
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn a_message_only_amend_of_a_test_commit_passes() {
    let repo = Repo::new();
    std::fs::write(repo.wt.join("tests/t.rs"), "#[test]\nfn t() {}\n").unwrap();
    repo.git(&["add", "tests/t.rs"]);
    let out = repo.commit(MESSAGE, &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    let made = repo.head();
    let out = repo.commit(
        &MESSAGE.replace("test: declare", "test: declare it"),
        &["--amend"],
    );
    assert!(
        out.status.success(),
        "the amend was refused: {}",
        stderr(&out)
    );
    assert_ne!(repo.head(), made);
    // An amend that declares a path HEAD does not change is still refused.
    let out = repo.commit(
        &MESSAGE.replace("tests/t.rs", "tests/other.rs"),
        &["--amend"],
    );
    let err = stderr(&out);
    assert!(!out.status.success(), "the amend passed: {err}");
    assert!(err.contains(REFUSED), "{err}");
    assert!(err.contains("Acceptance-Test: tests/other.rs"), "{err}");
}

#[test]
fn a_project_commit_msg_hook_refuses_a_test_commit() {
    let repo = Repo::new();
    let project = repo.root.join("project-hooks");
    std::fs::create_dir_all(&project).unwrap();
    // A project hook that declares a file and stages it after git wrote the commit's tree.
    let hook = project.join("commit-msg");
    common::exe::write(
        &hook,
        "#!/bin/sh\nprintf '#[test]\\nfn t() {}\\n' > tests/t.rs\ngit add tests/t.rs\nprintf '\\nAcceptance-Test: tests/t.rs\\n' >> \"$1\"\n",
    );
    repo.git(&[
        "config",
        makiba::hooks::git_hook::PROJECT_KEY,
        project.to_str().unwrap(),
    ]);
    std::fs::write(repo.wt.join("notes.txt"), "a note\n").unwrap();
    repo.git(&["add", "notes.txt"]);
    let before = repo.head();
    let out = repo.commit("docs: a note\n", &[]);
    let err = stderr(&out);
    assert!(!out.status.success(), "the commit passed: {err}");
    assert!(err.contains(REFUSED), "{err}");
    assert!(err.contains(hook.to_str().unwrap()), "{err}");
    assert_eq!(repo.head(), before, "a commit was made");
    // Without a declaration the project's hook adds, an ordinary commit passes.
    common::exe::write(&hook, "#!/bin/sh\nexit 0\n");
    let out = repo.commit("docs: a note\n", &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_ne!(repo.head(), before);
}
