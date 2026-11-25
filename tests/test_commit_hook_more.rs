//! Issue 322, diff review rounds 1 and 2: two more cases of the commit-msg hook's check of a test
//! commit (line L-K9; the declared cases are in `tests/test_commit_hook.rs`).
//!
//! - Git tells the hook nothing of `--amend`, so the declared paths are judged by the index
//!   against HEAD, the commit amended: a message-only amend of a test commit is refused (the
//!   approved design's known limit), and the remedy the refusal names, `git reset --soft HEAD~1`
//!   and a new commit, passes.
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
fn a_message_only_amend_is_refused_and_the_named_remedy_passes() {
    let repo = Repo::new();
    std::fs::write(repo.wt.join("tests/t.rs"), "#[test]\nfn t() {}\n").unwrap();
    repo.git(&["add", "tests/t.rs"]);
    let out = repo.commit(MESSAGE, &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    let made = repo.head();
    let reworded = MESSAGE.replace("test: declare", "test: declare it");
    // The amend changes no file against HEAD, the commit it amends: refused, naming the remedy.
    let out = repo.commit(&reworded, &["--amend"]);
    let err = stderr(&out);
    assert!(!out.status.success(), "the amend passed: {err}");
    assert!(err.contains(REFUSED), "{err}");
    assert!(err.contains("Acceptance-Test: tests/t.rs"), "{err}");
    assert!(err.contains("git reset --soft HEAD~1"), "{err}");
    assert_eq!(repo.head(), made, "the commit was amended");
    // The remedy: take the commit back into the index and commit again.
    repo.git(&["reset", "-q", "--soft", "HEAD~1"]);
    let out = repo.commit(&reworded, &[]);
    assert!(
        out.status.success(),
        "the commit made again was refused: {}",
        stderr(&out)
    );
    assert_ne!(repo.head(), made);
    assert_eq!(repo.git(&["log", "-1", "--format=%s"]), "test: declare it");
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

/// Diff review round 3: the makibad-start rule is makiba's own. A declared test of another
/// project, which names no makiba, is not scanned, even when it says "daemon".
#[test]
fn another_projects_test_that_says_daemon_passes() {
    let repo = Repo::new();
    std::fs::write(
        repo.wt.join("tests/t.rs"),
        format!(
            "#[test]\nfn t() {{ assert_eq!(mode(), \"{}{}\"); }}\n",
            "dae", "mon"
        ),
    )
    .unwrap();
    repo.git(&["add", "tests/t.rs"]);
    let before = repo.head();
    let out = repo.commit(MESSAGE, &[]);
    assert!(out.status.success(), "refused: {}", stderr(&out));
    assert_ne!(repo.head(), before);
}

/// Diff review round 3: a declaration the merge gate rejects is refused as written, not
/// normalized into a path the commit changes.
#[test]
fn a_declared_path_the_gate_rejects_is_refused() {
    let repo = Repo::new();
    std::fs::write(repo.wt.join("tests/t.rs"), "#[test]\nfn t() {}\n").unwrap();
    repo.git(&["add", "tests/t.rs"]);
    let before = repo.head();
    for bad in ["/tests/t.rs", "tests/../tests/t.rs"] {
        let out = repo.commit(&MESSAGE.replace("tests/t.rs", bad), &[]);
        let err = stderr(&out);
        assert!(!out.status.success(), "{bad} passed: {err}");
        assert!(err.contains(REFUSED), "{err}");
        assert!(err.contains(&format!("Acceptance-Test: {bad}")), "{err}");
        assert_eq!(repo.head(), before, "a commit was made");
    }
}
