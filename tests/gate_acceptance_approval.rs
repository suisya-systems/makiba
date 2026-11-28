//! Issue 591 (docs/design/acceptance-approval.md 4.4 (1), build step 2): the gate judges a head
//! whose acceptance check failed by construction.
//!
//! - A log holding only approvable records gives an allowance (`passed`) whose failure set the
//!   acceptance warning names; condition (a)'s refusals are held back only for the set's paths.
//! - `not approvable`, no record, a malformed record, an unreadable log or one longer than the
//!   gate reads is refused; an indented line, a line without the runner's timestamp and a
//!   coloured echo line are not read.
//! - A record not valid by the note's section 5, a change under `.github/`, a checker change the
//!   log does not name, or another check that failed or has not finished is refused.
//! - Condition (b) is not judged: a head the fallback of (b) passes is not allowed by it.
//!
//! The gate's functions run on a real git repository; `gh` is a stand-in script (made through
//! tests/common/exe.rs) answering from files of the test's folder. The logs have the form of a
//! downloaded Actions job log: a byte order mark, then every line after a runner timestamp.

mod common;

use makiba::daemon::gate::{self, Ci, Reject, Verdict};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Once;

const C: &str = "cccccccccccccccccccccccccccccccccccccccc";
const T1: &str = "2026-11-28T03:00:00Z";
const ACTIONS: i64 = 15368;
const STAMP: &str = "2026-11-28T03:04:23.8170690Z";
const HUMAN: &str = "acceptance-check: tests/answer.rs changed after the test commit (declared tests, paths the test commit changed, test infrastructure by file name anywhere, test files outside tests/common/ and tests/fixtures/, .github/ and this script are protected)";

/// The stand-in for `gh`, answering from the folder `@DIR@`: a job's log from `log.<id>` (exit 1
/// when `log.<id>.fail` exists), every other call from `<sha>.<kind>`, the lines `--jq` prints.
const STAND_IN: &str = r#"#!/bin/sh
d='@DIR@'
if [ "$2" = "--paginate" ]; then ep=$3; else ep=$2; fi
echo "$ep" >> "$d/calls"
case "$ep" in
  */actions/jobs/*/logs)
    id=${ep#*/actions/jobs/}; id=${id%%/*}
    if [ -e "$d/log.$id.fail" ]; then echo "boom" >&2; exit 1; fi
    cat "$d/log.$id"; exit 0 ;;
  *check-runs?filter=all*) kind=checks ;;
  *check-runs*) kind=surface ;;
  */status*) kind=status ;;
  *event=pull_request*) kind=runs ;;
  */actions/runs*) kind=going ;;
  *) echo "unexpected call: $*" >&2; exit 2 ;;
esac
case "$ep" in
  */commits/*) sha=${ep#*/commits/}; sha=${sha%%/*} ;;
  *) sha=${ep#*head_sha=}; sha=${sha%%&*} ;;
esac
if [ -e "$d/$sha.$kind" ]; then cat "$d/$sha.$kind"; fi
exit 0
"#;

static INIT: Once = Once::new();

fn init() {
    INIT.call_once(|| {
        // SAFETY: set once, before this file's tests read the environment.
        unsafe { std::env::set_var(gate::READ_WAIT_ENV, "0") };
    });
}

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

/// One pull request: the repository, the stand-in's folder and program, the base and the test
/// commit (`m`).
struct World {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    gh: PathBuf,
    program: String,
    base: String,
    m: String,
}

impl World {
    /// A base; a test commit declaring `tests/answer.rs` and `tests/second.rs` that also adds the
    /// helper `tests/common/h.rs`; the fix.
    fn new() -> World {
        init();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("repo");
        let gh = tmp.path().join("gh");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&gh).unwrap();
        let program = tmp.path().join("gh-stand-in");
        common::exe::write(&program, STAND_IN.replace("@DIR@", gh.to_str().unwrap()));
        git(&dir, &["init", "-q", "-b", "main"]);
        let mut w = World {
            _tmp: tmp,
            dir,
            gh,
            program: program.to_str().unwrap().to_string(),
            base: String::new(),
            m: String::new(),
        };
        w.base = w.commit(
            "base",
            &[
                ("src/lib.rs", "pub fn answer() -> u32 { 41 }\n"),
                (".github/acceptance-check", "acceptance\n"),
                (".github/workflows/ci.yml", "on: pull_request\n"),
                ("scripts/acceptance-check.sh", "#!/bin/sh\necho base\n"),
                ("tests/conftest.py", "\n"),
            ],
        );
        git(&w.dir, &["checkout", "-q", "-b", "feature"]);
        w.m = w.commit(
            "test\n\nAcceptance-Test: tests/answer.rs\nAcceptance-Test: tests/second.rs\nCo-Authored-By: t <t@example.com>",
            &[
                ("tests/answer.rs", "#[test]\nfn a() {}\n"),
                ("tests/second.rs", "#[test]\nfn b() {}\n"),
                ("tests/common/h.rs", "pub fn h() {}\n"),
            ],
        );
        w.commit("fix", &[("src/lib.rs", "pub fn answer() -> u32 { 42 }\n")]);
        w
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

    fn head(&self) -> String {
        git(&self.dir, &["rev-parse", "HEAD"])
    }

    /// The stand-in's answer of one call at `sha`: the lines `--jq` would print.
    fn answer(&self, sha: &str, kind: &str, lines: &[Value]) {
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(self.gh.join(format!("{sha}.{kind}")), text).unwrap();
    }

    /// CI at `sha`: each `(name, conclusion, id)` as a completed check run of the pull request's
    /// workflow run (workflow 1, suite 1, base C), in both reads of the check runs.
    fn ci(&self, sha: &str, runs: &[(&str, &str, i64)]) {
        let checks: Vec<Value> = runs
            .iter()
            .map(|(name, c, id)| {
                json!({"app": ACTIONS, "name": name, "conclusion": c, "completed_at": T1,
                       "suite": 1, "id": id})
            })
            .collect();
        self.answer(sha, "checks", &checks);
        let surface: Vec<Value> = runs
            .iter()
            .map(|(name, c, id)| {
                json!({"name": name, "status": "completed", "conclusion": c, "id": id,
                       "app": "github-actions"})
            })
            .collect();
        self.answer(sha, "surface", &surface);
        self.answer(
            sha,
            "runs",
            &[json!({"wf": 1, "suite": 1, "created": T1, "bases": [C]})],
        );
        self.answer(sha, "going", &[json!({"status": "completed"})]);
        self.answer(sha, "status", &[]);
    }

    /// The default CI at `sha`: `acceptance` (job 501) failed, `tests` succeeded.
    fn failed_acceptance(&self, sha: &str) {
        self.ci(
            sha,
            &[("acceptance", "failure", 501), ("tests", "success", 502)],
        );
    }

    /// Job 501's log: the given lines, after a byte order mark, each after a runner timestamp;
    /// a coloured echo line naming a record that would be refused comes first.
    fn log(&self, lines: &[&str]) {
        let mut text = String::from("\u{feff}");
        text.push_str(&format!(
            "{STAMP} \x1b[36;1mecho acceptance-check: approvable changed src/lib.rs\x1b[0m\n"
        ));
        for l in lines {
            text.push_str(&format!("{STAMP} {l}\n"));
        }
        std::fs::write(self.gh.join("log.501"), text).unwrap();
    }

    /// `evaluate_failed` at `head`.
    fn judge(&self, head: &str) -> (Result<(), Reject>, Verdict, Ci) {
        let mut ci = Ci::new(&self.program);
        let mut v = Verdict {
            base: self.base.clone(),
            head: head.to_string(),
            ..Default::default()
        };
        let r = gate::evaluate_failed(&self.dir, &self.base, head, "o/r", None, &mut ci, &mut v);
        (r, v, ci)
    }

    /// Today's `evaluate` at `head`, a failed CI run recorded at the test commit.
    fn evaluate(&self, head: &str) -> (Result<(), Reject>, Verdict) {
        let mut ci = Ci::new(&self.program);
        let mut v = Verdict {
            base: self.base.clone(),
            head: head.to_string(),
            ..Default::default()
        };
        let failed = |_: &str| Ok(true);
        let r = gate::evaluate(
            &self.dir, &self.base, head, "o/r", None, &mut ci, &failed, &mut v,
        );
        (r, v)
    }

    /// Case 1's head: `tests/answer.rs` edited after the test commit, CI as by default.
    fn edited_answer(&self) -> String {
        let head = self.commit(
            "edit the declared test",
            &[("tests/answer.rs", "#[test]\nfn a() { assert!(true) }\n")],
        );
        self.failed_acceptance(&head);
        head
    }
}

fn refused(r: &Result<(), Reject>, needle: &str) -> String {
    match r {
        Ok(()) => panic!("allowed, expected a refusal naming {needle:?}"),
        Err(Reject(why)) => {
            assert!(why.contains(needle), "{needle:?} not in {why:?}");
            why.clone()
        }
    }
}

const APPROVABLE: &str = "acceptance-check: approvable changed tests/answer.rs";

#[test]
fn approvable_only_log_gives_an_allowance_with_the_warning() {
    let w = World::new();
    let head = w.edited_answer();
    w.log(&[HUMAN, APPROVABLE, "ACCEPTANCE: fail"]);
    let (r, v, _) = w.judge(&head);
    assert_eq!(r, Ok(()), "{}", v.reason);
    assert_eq!(v.result, "passed");
    assert_eq!(
        v.acceptance_failures,
        vec!["changed tests/answer.rs".to_string()]
    );
    assert_eq!(
        gate::acceptance_warning(&v.acceptance_failures),
        "acceptance fails by construction: changed tests/answer.rs"
    );
    assert!(v.reason.contains("by construction"), "{}", v.reason);
    // The allowance is the hold-back and nothing else: today's path refuses the same head.
    let (today, _) = w.evaluate(&head);
    refused(&today, "protected paths changed after the test commit");
}

#[test]
fn the_set_is_sorted_and_names_every_kind() {
    let w = World::new();
    let head = w.commit(
        "edit",
        &[
            ("tests/answer.rs", "#[test]\nfn a() { assert!(true) }\n"),
            ("tests/common/h.rs", "pub fn h() { }\n"),
            ("scripts/acceptance-check.sh", "#!/bin/sh\necho head\n"),
        ],
    );
    w.failed_acceptance(&head);
    w.log(&[
        "acceptance-check: approvable ignored tests/second.rs 2",
        "acceptance-check: approvable changed tests/common/h.rs",
        "acceptance-check: approvable changed tests/answer.rs",
        "acceptance-check: approvable changed scripts/acceptance-check.sh",
        "ACCEPTANCE: fail",
    ]);
    let (r, v, _) = w.judge(&head);
    assert_eq!(r, Ok(()), "{}", v.reason);
    assert_eq!(
        v.acceptance_failures,
        vec![
            "changed scripts/acceptance-check.sh".to_string(),
            "changed tests/answer.rs".to_string(),
            "changed tests/common/h.rs".to_string(),
            "ignored tests/second.rs 2".to_string(),
        ]
    );
}

#[test]
fn not_approvable_is_refused() {
    let w = World::new();
    let head = w.edited_answer();
    w.log(&[
        HUMAN,
        APPROVABLE,
        "acceptance-check: not approvable",
        "ACCEPTANCE: fail",
    ]);
    let (r, v, _) = w.judge(&head);
    refused(&r, "not approvable");
    assert!(v.acceptance_failures.is_empty());
}

#[test]
fn no_record_is_refused() {
    let w = World::new();
    let head = w.edited_answer();
    w.log(&["ACCEPTANCE: fail"]);
    refused(&w.judge(&head).0, "no approvable record");
}

#[test]
fn an_unreadable_log_is_refused() {
    let w = World::new();
    let head = w.edited_answer();
    std::fs::write(w.gh.join("log.501.fail"), "").unwrap();
    let (r, _, ci) = w.judge(&head);
    assert!(r.is_err(), "an unreadable log was allowed");
    assert!(ci.unavailable().is_some(), "{r:?}");
    // A log longer than the gate reads is refused, though its records are at its end.
    std::fs::remove_file(w.gh.join("log.501.fail")).unwrap();
    let filler = format!("{STAMP} {}\n", "x".repeat(200));
    let mut text = String::from("\u{feff}");
    while text.len() <= 4 << 20 {
        text.push_str(&filler);
    }
    text.push_str(&format!("{STAMP} {APPROVABLE}\n{STAMP} ACCEPTANCE: fail\n"));
    std::fs::write(w.gh.join("log.501"), text).unwrap();
    refused(&w.judge(&head).0, "longer than 4 MiB");
}

#[test]
fn a_fabricated_indented_line_is_not_read() {
    let w = World::new();
    let head = w.edited_answer();
    w.log(&[
        "  acceptance-check: approvable changed tests/answer.rs",
        "ACCEPTANCE: fail",
    ]);
    refused(&w.judge(&head).0, "no approvable record");
    // A line at column 0 without the runner's timestamp is not read either.
    let text = format!("\u{feff}{APPROVABLE}\n{STAMP} ACCEPTANCE: fail\n");
    std::fs::write(w.gh.join("log.501"), text).unwrap();
    refused(&w.judge(&head).0, "no approvable record");
}

#[test]
fn a_declared_path_outside_the_set_is_still_refused_by_a() {
    let w = World::new();
    let head = w.commit(
        "edit both",
        &[
            ("tests/answer.rs", "#[test]\nfn a() { assert!(true) }\n"),
            ("tests/second.rs", "#[test]\nfn b() { assert!(true) }\n"),
        ],
    );
    w.failed_acceptance(&head);
    w.log(&[HUMAN, APPROVABLE, "ACCEPTANCE: fail"]);
    let why = refused(
        &w.judge(&head).0,
        "protected paths changed after the test commit",
    );
    assert!(why.contains("tests/second.rs"), "{why}");
    assert!(!why.contains("\"tests/answer.rs\""), "{why}");
}

#[test]
fn the_fallback_of_b_does_not_allow_a_failed_acceptance() {
    let w = World::new();
    let head = w.head();
    w.ci(
        &w.m,
        &[("acceptance", "failure", 401), ("tests", "failure", 402)],
    );
    w.failed_acceptance(&head);
    // Today's path passes this head by the fallback of (b), its acceptance check failed.
    let (today, v) = w.evaluate(&head);
    assert_eq!(today, Ok(()), "{}", v.reason);
    assert!(
        v.reason.contains("failed at the test commit"),
        "{}",
        v.reason
    );
    // The path of a failed acceptance does not read the fallback.
    w.log(&["acceptance-check: not approvable", "ACCEPTANCE: fail"]);
    refused(&w.judge(&head).0, "not approvable");
    w.log(&[
        "acceptance-check: approvable ignored tests/second.rs 1",
        "ACCEPTANCE: fail",
    ]);
    let (r, v, _) = w.judge(&head);
    assert_eq!(r, Ok(()), "{}", v.reason);
    assert!(
        v.reason.contains("condition (b) is not judged"),
        "{}",
        v.reason
    );
    assert!(
        !v.reason.contains("failed at the test commit"),
        "{}",
        v.reason
    );
}

#[test]
fn a_github_change_is_refused() {
    let w = World::new();
    w.edited_answer();
    let head = w.commit(
        "workflow",
        &[(".github/workflows/ci.yml", "on: pull_request\njobs: {}\n")],
    );
    w.failed_acceptance(&head);
    w.log(&[HUMAN, APPROVABLE, "ACCEPTANCE: fail"]);
    refused(&w.judge(&head).0, ".github/");
    let other = World::new();
    other.edited_answer();
    let head = other.commit("other", &[(".github/other.txt", "x\n")]);
    other.failed_acceptance(&head);
    other.log(&[HUMAN, APPROVABLE, "ACCEPTANCE: fail"]);
    refused(&other.judge(&head).0, ".github/");
}

#[test]
fn an_invalid_record_is_refused() {
    let w = World::new();
    let head = w.edited_answer();
    for record in [
        "changed src/lib.rs",
        "ignored src/lib.rs 1",
        "ignored tests/common/h.rs 1",
        "changed tests/conftest.py",
    ] {
        let line = format!("acceptance-check: approvable {record}");
        w.log(&[HUMAN, APPROVABLE, &line, "ACCEPTANCE: fail"]);
        let why = refused(&w.judge(&head).0, "not approvable by section 5");
        let path = record.split(' ').nth(1).unwrap();
        assert!(why.contains(path), "{path} not in {why}");
    }
}

#[test]
fn a_malformed_record_is_refused() {
    let w = World::new();
    let head = w.edited_answer();
    for line in [
        "acceptance-check: approvable changed a b",
        "acceptance-check: approvable ignored tests/second.rs x",
        "acceptance-check: approvable renamed x",
    ] {
        w.log(&[HUMAN, APPROVABLE, line, "ACCEPTANCE: fail"]);
        refused(&w.judge(&head).0, "malformed");
    }
}

#[test]
fn acceptance_must_be_the_only_failed_check() {
    let w = World::new();
    let head = w.edited_answer();
    w.log(&[HUMAN, APPROVABLE, "ACCEPTANCE: fail"]);
    // Acceptance succeeded.
    w.ci(
        &head,
        &[("acceptance", "success", 501), ("tests", "success", 502)],
    );
    refused(&w.judge(&head).0, "did not fail");
    // Another check failed beside it.
    w.ci(
        &head,
        &[("acceptance", "failure", 501), ("tests", "failure", 502)],
    );
    refused(&w.judge(&head).0, "another check");
    // A commit status failed.
    w.failed_acceptance(&head);
    w.answer(
        &head,
        "status",
        &[json!({"context": "ci/external", "state": "failure"})],
    );
    refused(&w.judge(&head).0, "another check");
    // A check run has not finished.
    w.failed_acceptance(&head);
    w.answer(
        &head,
        "surface",
        &[
            json!({"name": "acceptance", "status": "completed", "conclusion": "failure", "id": 501}),
            json!({"name": "tests", "status": "in_progress", "conclusion": null, "id": 502}),
        ],
    );
    refused(&w.judge(&head).0, "not finished");
    // A workflow run has not finished.
    w.failed_acceptance(&head);
    w.answer(&head, "going", &[json!({"status": "in_progress"})]);
    refused(&w.judge(&head).0, "not finished");
    // No run of the acceptance check.
    w.ci(&head, &[("tests", "success", 502)]);
    assert!(w.judge(&head).0.is_err(), "allowed with no acceptance run");
    // A skipped check beside the failed acceptance is no failure.
    w.ci(
        &head,
        &[
            ("acceptance", "failure", 501),
            ("tests", "success", 502),
            ("lint", "skipped", 503),
        ],
    );
    let (r, v, _) = w.judge(&head);
    assert_eq!(r, Ok(()), "{}", v.reason);
}

#[test]
fn a_checker_change_the_log_does_not_name_is_refused() {
    let w = World::new();
    let head = w.commit(
        "edit",
        &[
            ("tests/answer.rs", "#[test]\nfn a() { assert!(true) }\n"),
            ("scripts/acceptance-check.sh", "#!/bin/sh\necho head\n"),
        ],
    );
    w.failed_acceptance(&head);
    w.log(&[HUMAN, APPROVABLE, "ACCEPTANCE: fail"]);
    refused(&w.judge(&head).0, "scripts/acceptance-check.sh");
}
