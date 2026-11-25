//! Issue 372: a test helper is a file under `tests/common/` or `tests/fixtures/` (by place), so an
//! existing test of a language the acceptance rule does not know (`tests/calc.test.js`,
//! `tests/calc_test.go`) changed or deleted in a test commit without being declared is refused,
//! by `scripts/acceptance-check.sh` (its `--test-commit` mode and its full run) and by the merge
//! gate, which runs the same script (`gate::test_commit_refusals`, `gate::evaluate`). Before, every
//! file under `tests/` that was neither a Cargo target nor `test_*.py` counted as a helper and
//! passed. PR 353's case (a helper extended in `tests/common/`) still passes both.
//!
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

const JS: &str = "tests/calc.test.js";
const GO: &str = "tests/calc_test.go";
const WEB_JS: &str = "tests/web/calc.test.js";

/// A scratch repository: `main` holds code, two helpers by place and tests of other languages;
/// `feature` follows.
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
                ("src/lib.rs", Some("pub fn answer() -> u32 { 1 }\n")),
                ("tests/common/mod.rs", Some("pub fn setup() {}\n")),
                ("tests/fixtures/data.json", Some("{}\n")),
                (JS, Some("test('add', () => { expect(1).toBe(1); });\n")),
                (GO, Some("package calc\n\nfunc TestAdd(t *testing.T) {}\n")),
                (WEB_JS, Some("test('web', () => {});\n")),
            ],
        );
        git(&r.dir, &["checkout", "-q", "-b", "feature"]);
        Repo { base, ..r }
    }

    /// A commit writing each `Some` file and deleting each `None` one.
    fn commit(&self, msg: &str, files: &[(&str, Option<&str>)]) -> String {
        for (p, text) in files {
            match text {
                Some(text) => {
                    let path = self.dir.join(p);
                    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                    std::fs::write(&path, text).unwrap();
                    git(&self.dir, &["add", p]);
                }
                None => {
                    git(&self.dir, &["rm", "-q", p]);
                }
            }
        }
        git(&self.dir, &["commit", "-q", "-m", msg]);
        git(&self.dir, &["rev-parse", "HEAD"])
    }

    /// The test commit: declares `declared` (tests/answer.rs first), adds `tests/answer.rs`, and
    /// makes the `extra` changes.
    fn test_commit(&self, declared: &[&str], extra: &[(&str, Option<&str>)]) -> String {
        let mut files = vec![(
            "tests/answer.rs",
            Some("#[test]\nfn answer_is_two() { assert_eq!(2, 2); }\n"),
        )];
        files.extend_from_slice(extra);
        let trailers: String = declared
            .iter()
            .map(|p| format!("Acceptance-Test: {p}\n"))
            .collect();
        self.commit(
            &format!("test: the answer\n\n{trailers}Co-Authored-By: t <t@example.com>"),
            &files,
        )
    }

    fn fix(&self) {
        self.commit(
            "fix",
            &[("src/lib.rs", Some("pub fn answer() -> u32 { 2 }\n"))],
        );
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
fn undeclared_tests_of_other_languages_are_refused_by_both() {
    let cases: [(&str, &str, Option<&str>); 5] = [
        ("modified", JS, Some("test('add', () => {});\n")),
        ("deleted", JS, None),
        ("modified", GO, Some("package calc\n")),
        ("deleted", GO, None),
        (
            "modified",
            WEB_JS,
            Some("test('web', () => { skip(); });\n"),
        ),
    ];
    for (how, path, text) in cases {
        let r = Repo::new();
        let m = r.test_commit(&["tests/answer.rs"], &[(path, text)]);
        r.fix();

        let (code, out, err) = r.script(&["--test-commit", &m]);
        assert_eq!(
            code, 1,
            "{path} {how}: script --test-commit: stdout {out:?} stderr {err:?}"
        );
        assert_eq!(
            out.lines().collect::<Vec<_>>(),
            vec![path],
            "{path} {how}: script --test-commit refuses exactly it"
        );

        let full = r.full();
        assert!(
            full.contains(&format!(
                "test commit changes {path}, which is neither declared nor a test helper"
            )),
            "{path} {how}: the full script does not refuse it:\n{full}"
        );
        assert_eq!(
            full.lines().last(),
            Some("ACCEPTANCE: fail"),
            "{path} {how}:\n{full}"
        );

        assert_eq!(
            gate::test_commit_refusals(&r.dir, &m),
            Ok(vec![path.to_string()]),
            "{path} {how}: the gate's rule 2"
        );
        let why = r
            .gate()
            .expect_err(&format!("{path} {how}: the gate refuses"))
            .0;
        assert!(
            why.contains(&format!("undeclared paths [\"{path}\"]")),
            "{path} {how}: {why}"
        );
    }
}

#[test]
fn helpers_in_common_and_fixtures_pass_both() {
    let r = Repo::new();
    let m = r.test_commit(
        &["tests/answer.rs"],
        &[
            (
                "tests/common/mod.rs",
                Some("pub fn setup() {}\npub fn helper() {}\n"),
            ),
            ("tests/fixtures/data.json", Some("{\"x\": 1}\n")),
        ],
    );
    r.fix();

    let (code, out, err) = r.script(&["--test-commit", &m]);
    assert_eq!(
        code, 0,
        "script --test-commit: stdout {out:?} stderr {err:?}"
    );
    assert_eq!(out, "", "script --test-commit refuses nothing");

    let full = r.full();
    assert!(
        !full.contains("neither declared nor a test helper"),
        "the full script refuses a helper of the test commit:\n{full}"
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
}

#[test]
fn a_declared_test_of_another_language_may_change() {
    let r = Repo::new();
    let m = r.test_commit(
        &["tests/answer.rs", JS],
        &[(JS, Some("test('add', () => { expect(2).toBe(2); });\n"))],
    );
    r.fix();

    let (code, out, err) = r.script(&["--test-commit", &m]);
    assert_eq!(
        code, 0,
        "script --test-commit: stdout {out:?} stderr {err:?}"
    );
    assert_eq!(out, "", "script --test-commit refuses nothing");
    assert_eq!(gate::test_commit_refusals(&r.dir, &m), Ok(vec![]));
}
