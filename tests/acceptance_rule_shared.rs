//! Issue 361: `scripts/acceptance-check.sh` and the merge gate agree on what a test commit may
//! change. PR 353 extended `tests/common/makibad.rs` in its test commit: the script let it through
//! (rule 2: declared paths and test helpers), the gate refused it ("also changes undeclared
//! paths"). The rule is held once, in the script, and the gate runs it.
//!
//! Each case builds a scratch repository and asks both: the script (its `--test-commit` mode and
//! its full run) and the gate (`gate::test_commit_refusals`, `gate::evaluate`, `gate::edited_tests`).
//! The scratch repository has no `Cargo.toml`, so the full run of the script stops at rule 3
//! without building anything; rules 1, 2 and 4 are printed before it.

use makiba::daemon::gate::{self, Ci, Reject, Verdict};
use std::path::{Path, PathBuf};
use std::process::Command;

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
            "-c",
            "init.defaultBranch=main",
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

/// A scratch repository: `main` holds the code, a test helper and a pyproject; `feature` follows.
struct Repo {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    base: String,
}

impl Repo {
    fn new() -> Repo {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("repo");
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main"]);
        let r = Repo {
            _tmp: tmp,
            dir,
            base: String::new(),
        };
        let base = r.commit(
            "base",
            &[
                ("src/lib.rs", "pub fn answer() -> u32 { 1 }\n"),
                ("tests/common/mod.rs", "pub fn setup() {}\n"),
                ("pyproject.toml", "[project]\nname = 'x'\n"),
            ],
        );
        git(&r.dir, &["checkout", "-q", "-b", "feature"]);
        Repo { base, ..r }
    }

    fn commit(&self, msg: &str, files: &[(&str, &str)]) -> String {
        for (p, text) in files {
            let path = self.dir.join(p);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, text).unwrap();
            git(&self.dir, &["add", p]);
        }
        git(&self.dir, &["commit", "-q", "-m", msg]);
        git(&self.dir, &["rev-parse", "HEAD"])
    }

    /// The test commit: declares and adds `tests/answer.rs`, and extends the helper.
    fn test_commit(&self, extra: &[(&str, &str)]) -> String {
        let mut files = vec![
            (
                "tests/answer.rs",
                "mod common;\n#[test]\nfn answer_is_two() { assert_eq!(2, 2); }\n",
            ),
            (
                "tests/common/mod.rs",
                "pub fn setup() {}\npub fn helper() {}\n",
            ),
        ];
        files.extend_from_slice(extra);
        self.commit(
            "test: the answer\n\nAcceptance-Test: tests/answer.rs\nCo-Authored-By: t <t@example.com>",
            &files,
        )
    }

    fn head(&self) -> String {
        git(&self.dir, &["rev-parse", "HEAD"])
    }

    /// The script with `args`, run in the scratch repository: (exit code, stdout, stderr).
    fn script(&self, args: &[&str]) -> (i32, String, String) {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/acceptance-check.sh");
        let out = Command::new("bash")
            .arg(script)
            .args(args)
            .current_dir(&self.dir)
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "core.hooksPath")
            .env("GIT_CONFIG_VALUE_0", "/dev/null")
            .output()
            .unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    /// The full run of the script on `base..head`: stdout and stderr together.
    fn full(&self) -> String {
        let (_, out, err) = self.script(&[&self.base, &self.head()]);
        out + &err
    }

    /// The gate's git and CI checks; a failed CI run is recorded at every commit, and GitHub
    /// cannot be read, so a case that passes every git check stops at "cannot read".
    fn gate(&self) -> Result<(), Reject> {
        let mut ci = Ci::new("/nonexistent/gh");
        let failed = |_: &str| -> Result<bool, Reject> { Ok(true) };
        let mut v = Verdict::default();
        gate::evaluate(
            &self.dir,
            &self.base,
            &self.head(),
            "o/scratch",
            None,
            &mut ci,
            &failed,
            &mut v,
        )
    }
}

#[test]
fn test_commit_adding_to_tests_common_passes_both() {
    let r = Repo::new();
    let m = r.test_commit(&[]);
    r.commit("fix", &[("src/lib.rs", "pub fn answer() -> u32 { 2 }\n")]);

    let (code, out, err) = r.script(&["--test-commit", &m]);
    assert_eq!(
        code, 0,
        "script --test-commit: stdout {out:?} stderr {err:?}"
    );
    assert_eq!(out, "", "script --test-commit refuses nothing");

    let full = r.full();
    assert!(
        !full.contains("neither declared nor a test helper"),
        "the full script refuses a path of the test commit:\n{full}"
    );
    assert!(
        !full.contains("changed after the test commit"),
        "the full script finds a protected change:\n{full}"
    );

    assert_eq!(gate::test_commit_refusals(&r.dir, &m), Ok(vec![]));
    match r.gate() {
        Err(Reject(why)) => assert!(
            why.starts_with("cannot read"),
            "the gate refused before the CI read: {why}"
        ),
        Ok(()) => panic!("the gate passed without reading CI"),
    }
    assert_eq!(
        gate::edited_tests(
            &r.dir,
            &r.base,
            &r.head(),
            Some(&m),
            &["tests/answer.rs".to_string()]
        ),
        Ok(vec![]),
        "the helper extended in the test commit is not an edited test"
    );
}

#[test]
fn test_commit_changing_code_is_refused_by_both() {
    let r = Repo::new();
    let m = r.test_commit(&[
        ("src/lib.rs", "pub fn answer() -> u32 { 2 }\n"),
        ("pyproject.toml", "[project]\nname = 'y'\n"),
    ]);
    r.commit("more", &[("README.md", "x\n")]);

    let (code, out, err) = r.script(&["--test-commit", &m]);
    assert_eq!(
        code, 1,
        "script --test-commit: stdout {out:?} stderr {err:?}"
    );
    assert_eq!(
        out.lines().collect::<Vec<_>>(),
        vec!["pyproject.toml", "src/lib.rs"]
    );

    let full = r.full();
    for p in ["src/lib.rs", "pyproject.toml"] {
        assert!(
            full.contains(&format!(
                "test commit changes {p}, which is neither declared nor a test helper"
            )),
            "the full script does not refuse {p}:\n{full}"
        );
    }

    assert_eq!(
        gate::test_commit_refusals(&r.dir, &m),
        Ok(vec!["pyproject.toml".to_string(), "src/lib.rs".to_string()])
    );
    let why = r.gate().expect_err("the gate refuses").0;
    assert!(
        why.contains("undeclared paths [\"pyproject.toml\", \"src/lib.rs\"]"),
        "{why}"
    );
}

#[test]
fn helper_from_test_commit_changed_by_fix_is_refused_by_both() {
    let r = Repo::new();
    r.test_commit(&[]);
    r.commit(
        "fix",
        &[
            ("src/lib.rs", "pub fn answer() -> u32 { 2 }\n"),
            ("tests/common/mod.rs", "pub fn setup() {}\n"),
        ],
    );

    let full = r.full();
    assert!(
        full.contains("tests/common/mod.rs changed after the test commit"),
        "the full script lets the helper change:\n{full}"
    );

    let why = r.gate().expect_err("the gate refuses").0;
    assert!(
        why.contains("protected paths changed after the test commit")
            && why.contains("tests/common/mod.rs"),
        "{why}"
    );
}
