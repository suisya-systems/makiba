//! Issue 256: the read surface shows the next candidates and why there are none.
//!
//! After a scan, `makiba status --json` holds `next_work`: every candidate found ready with its
//! goal clause, the recommended one and which were forwarded to the secretary, and what was left
//! out with the project and the reason; the text of `makiba status` says it in a line or two; a
//! read writes nothing; and the table `Next work field` of spec section 12 is what is printed.
//!
//! `makibad` runs in this process on the stand-in base, as in `tests/triage_forward.rs`: a close is
//! a `run_closed` appended here, and `gh` is a stand-in script (`MAKIBA_GH`) that answers
//! `issue list` / `pr list` from files and refuses any other call, so no test reaches the network.

mod common;

use common::makibad;
use makiba::base::Base;
use makiba::base::stub::Stub;
use makiba::db::writer::{NewEvent, RunUpsert, StateWriter};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Once, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const MAKIBA: &str = env!("CARGO_BIN_EXE_makiba");
const SPEC: &str = include_str!("../docs/design/spec.md");

const STAND_IN: &str = r#"#!/bin/sh
# gh <issue|pr> list --repo github.com/<owner>/<name> --state <s> [...]: the file of that read.
what="$1 $2"; shift 2
case "$what" in "issue list"|"pr list") ;; *) echo "unexpected call: $what" >&2; exit 2 ;; esac
repo=; state=
while [ $# -gt 0 ]; do
  case "$1" in
    --repo) repo=$2; shift 2 ;;
    --state) state=$2; shift 2 ;;
    --limit|--json|--search|--base) shift 2 ;;
    *) echo "unexpected argument: $1" >&2; exit 2 ;;
  esac
done
case "$repo" in github.com/*/*) ;; *) echo "unexpected repo: $repo" >&2; exit 2 ;; esac
r=${repo#github.com/}
f="$FAKE_GH_DIR/${r%%/*}_${r#*/}/${what%% *}-$state.json"
if [ -e "$f" ]; then cat "$f"; else echo '[]'; fi
"#;

const LEDGER: &str = "# Goals\n\n## G1 It runs its own development\n- unmet if: nothing is proposed\n\n\
                      ## G2 It is quiet\n- unmet if: it is loud\n";

static HOME: OnceLock<tempfile::TempDir> = OnceLock::new();
static INIT: Once = Once::new();

/// The shared folder: the `gh` stand-in and its answers, the goal ledgers, the repositories.
fn init() -> PathBuf {
    INIT.call_once(|| {
        let home = HOME.get_or_init(|| tempfile::tempdir().unwrap());
        let script = home.path().join("gh");
        std::fs::write(&script, STAND_IN).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        for d in ["answers", "goals/o", "repos"] {
            std::fs::create_dir_all(home.path().join(d)).unwrap();
        }
        // SAFETY: before this call returns no test of this file has started a thread of its own.
        unsafe {
            std::env::set_var("MAKIBA_GH", &script);
            std::env::set_var("FAKE_GH_DIR", home.path().join("answers"));
            std::env::set_var("MAKIBA_GOALS_DIR", home.path().join("goals"));
            std::env::set_var("MAKIBA_REPOS_DIR", home.path().join("repos"));
            std::env::set_var("MAKIBA_TRIAGE_INTERVAL_MS", "500");
            std::env::set_var("MAKIBA_DOOR", "none");
        }
    });
    HOME.get().unwrap().path().to_path_buf()
}

/// One organization with one project `<name>` (repository `o/<name>`) and a run `t1`, after one
/// scan, with `makibad` stopped again.
struct Org {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
}

impl Org {
    fn scanned(name: &str, ledger: bool, issues: &[Value]) -> Org {
        let home = init();
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("state");
        std::fs::create_dir_all(&dir).unwrap();
        let w = StateWriter::open(&dir.join("state.db")).unwrap();
        w.ensure_project(name, None, Some(&format!("https://github.com/o/{name}")))
            .unwrap();
        w.upsert_run(&RunUpsert {
            task_id: "t1",
            project_slug: name,
            title: Some("T"),
            status: Some("in_use"),
            ..Default::default()
        })
        .unwrap();
        if ledger {
            std::fs::write(home.join(format!("goals/o/{name}.md")), LEDGER).unwrap();
        }
        let answers = home.join("answers").join(format!("o_{name}"));
        std::fs::create_dir_all(&answers).unwrap();
        std::fs::write(
            answers.join("issue-open.json"),
            serde_json::to_string(issues).unwrap(),
        )
        .unwrap();
        let org = Org { _tmp: tmp, dir };
        {
            let _d = Daemon::start(&org.dir);
            let db = org.db();
            let count = |kind: &str| -> i64 {
                db.query_row("SELECT COUNT(*) FROM events WHERE kind = ?1", [kind], |r| {
                    r.get(0)
                })
                .unwrap()
            };
            wait("makibad to start", || count("dispatcher_started") > 0);
            w.append_event(&NewEvent {
                kind: "run_closed",
                actor: Some("secretary"),
                payload: Some(&json!({"task_id": "t1"})),
                run_task_id: Some("t1"),
                ..Default::default()
            })
            .unwrap();
            wait("the scan", || count("work_discovery_scanned") > 0);
        }
        org
    }

    fn db(&self) -> Connection {
        Connection::open(self.dir.join("state.db")).unwrap()
    }

    /// The person's `makiba`, with no door and no `makibad`.
    fn cli(&self, args: &[&str]) -> Output {
        Command::new(MAKIBA)
            .args(args)
            .env("MAKIBA_STATE_DIR", &self.dir)
            .env("MAKIBA_PERSON_KEY", self.dir.join("no-person.key"))
            .env("MAKIBA_GATE_KEY", self.dir.join("no-gate.key"))
            .env("MAKIBA_DOOR", "none")
            .env("MAKIBA_PORT", "0")
            .env("MAKIBA_BASE", "stub")
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    fn next_work(&self) -> Value {
        let out = self.cli(&["status", "--json"]);
        let v: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "status --json printed no JSON ({e}): {}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        });
        v["next_work"].clone()
    }

    fn text(&self) -> String {
        let out = self.cli(&["status"]);
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

/// `makibad` in this process, stopped when dropped.
struct Daemon {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<(), String>>>,
}

impl Daemon {
    fn start(dir: &Path) -> Daemon {
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (dir, stop) = (dir.to_path_buf(), Arc::clone(&stop));
            std::thread::spawn(move || {
                makibad::run_until(&dir, &|| Box::new(Stub::new()) as Box<dyn Base>, stop)
            })
        };
        Daemon {
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn wait(what: &str, mut ok: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(30);
    while Instant::now() < until {
        if ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("timed out waiting for {what}");
}

fn issue(number: i64, title: &str, body: &str) -> Value {
    json!({"number": number, "title": title, "body": body, "labels": [],
           "updatedAt": format!("2026-11-{:02}T10:00:00Z", number % 20 + 1),
           "createdAt": "2026-11-01T10:00:00Z", "milestone": null, "comments": []})
}

const NAMES_G1: &str = "Do it.\n\n## Goal clause\n\n- G1 It runs its own development\n";

/// The entry of `array` in `v` whose `field` is `want`.
fn entry<'a>(v: &'a Value, array: &str, field: &str, want: Value) -> &'a Value {
    v[array]
        .as_array()
        .unwrap_or_else(|| panic!("no array {array}: {v}"))
        .iter()
        .find(|e| e[field] == want)
        .unwrap_or_else(|| panic!("no {array} entry with {field} = {want}: {v}"))
}

/// Definition of done 1: after a scan with two candidates, both are listed with their clause and
/// the recommended one.
#[test]
fn a_scan_with_two_candidates_lists_both_with_their_clause_and_the_recommended_one() {
    let org = Org::scanned(
        "pair",
        true,
        &[
            issue(41, "Propose the next task", NAMES_G1),
            issue(42, "Tidy the logs", "Make them shorter."),
        ],
    );
    let v = org.next_work();
    assert_eq!(v["status"], json!("candidates_found"), "{v}");
    assert_eq!(v["trigger"], json!("close"), "{v}");
    assert_eq!(v["candidate_count"], json!(2), "{v}");
    assert!(v["scanned_at"].is_string(), "{v}");
    assert!(v["scan_id"].is_i64(), "{v}");

    let first = entry(&v, "candidates", "issue", json!(41));
    assert_eq!(first["repo"], json!("o/pair"), "{v}");
    assert_eq!(first["title"], json!("Propose the next task"), "{v}");
    assert_eq!(first["clause_id"], json!("G1"), "{v}");
    assert_eq!(
        first["clause_heading"],
        json!("It runs its own development"),
        "{v}"
    );
    assert_eq!(first["recommended"], json!(true), "{v}");
    assert_eq!(first["forwarded"], json!(true), "{v}");
    assert!(!first["why"].as_str().unwrap_or("").is_empty(), "{v}");

    let second = entry(&v, "candidates", "issue", json!(42));
    assert_eq!(second["title"], json!("Tidy the logs"), "{v}");
    assert_eq!(second["clause_id"], Value::Null, "{v}");
    assert_eq!(second["recommended"], json!(false), "{v}");

    let text = org.text();
    let line = text
        .lines()
        .find(|l| l.starts_with("next work:"))
        .unwrap_or_else(|| panic!("no next work line: {text}"));
    assert!(line.contains("2 candidates"), "{line}");
    assert!(line.contains("o/pair#41"), "{line}");
    assert!(line.contains("G1"), "{line}");
}

/// Definition of done 2: after a scan of a project with no goal ledger, no candidate, and the
/// project and the reason are named.
#[test]
fn a_project_with_no_goal_ledger_shows_no_candidate_and_names_the_project_and_the_reason() {
    let org = Org::scanned("bare", false, &[issue(51, "Anything", "Some work.")]);
    let v = org.next_work();
    assert_eq!(v["status"], json!("no_candidates"), "{v}");
    assert_eq!(v["candidate_count"], json!(0), "{v}");
    assert_eq!(v["candidates"], json!([]), "{v}");
    let left = entry(&v, "left_out", "project", json!("bare"));
    assert_eq!(left["kind"], json!("no_goal_ledger"), "{v}");
    assert_eq!(left["repo"], json!("o/bare"), "{v}");
    let reason = left["reason"].as_str().unwrap_or("");
    assert!(reason.contains("o/bare.md"), "{v}");

    let text = org.text();
    let at = text
        .find("next work:")
        .unwrap_or_else(|| panic!("no next work line: {text}"));
    let tail = &text[at..];
    assert!(tail.contains("no candidates"), "{tail}");
    assert!(tail.contains("project bare"), "{tail}");
    assert!(tail.contains("no goal ledger"), "{tail}");
}

/// The owner's remark on Issue 256: every candidate that could go in is listed, not only the
/// three the notice carries; those are flagged as forwarded.
#[test]
fn more_candidates_than_the_notice_carries_are_all_listed_and_the_forwarded_ones_flagged() {
    let org = Org::scanned(
        "many",
        true,
        &[
            issue(61, "First", NAMES_G1),
            issue(62, "Second", "b"),
            issue(63, "Third", "c"),
            issue(64, "Fourth", "d"),
        ],
    );
    let v = org.next_work();
    assert_eq!(v["candidate_count"], json!(4), "{v}");
    let mut forwarded = 0;
    for n in 61..=64 {
        let c = entry(&v, "candidates", "issue", json!(n));
        assert!(c["forwarded"].is_boolean(), "{v}");
        forwarded += usize::from(c["forwarded"] == json!(true));
    }
    assert_eq!(forwarded, 3, "{v}");
    assert_eq!(
        entry(&v, "candidates", "issue", json!(61))["forwarded"],
        json!(true),
        "{v}"
    );
    assert_eq!(v["candidates_not_recorded"], json!(0), "{v}");
}

/// Definition of done 3: a second read shows the same result and writes nothing.
#[test]
fn a_second_read_shows_the_same_and_writes_nothing() {
    let org = Org::scanned(
        "twice",
        true,
        &[
            issue(71, "Propose the next task", NAMES_G1),
            issue(72, "Tidy the logs", "Make them shorter."),
        ],
    );
    let facts = || {
        let db = org.db();
        let n = |sql: &str| -> i64 { db.query_row(sql, [], |r| r.get(0)).unwrap() };
        (
            n("SELECT COALESCE(MAX(id), 0) FROM events"),
            n("SELECT COUNT(*) FROM commands"),
            std::fs::read(org.dir.join("state.db")).unwrap(),
        )
    };
    let before = facts();
    let first = org.next_work();
    let _ = org.text();
    let second = org.next_work();
    assert!(first.is_object(), "{first}");
    assert_eq!(second, first);
    let after = facts();
    assert_eq!(after.0, before.0, "a read wrote an event");
    assert_eq!(after.1, before.1, "a read wrote a command row");
    assert!(after.2 == before.2, "a read changed state.db");
}

// ---- the documented shape ------------------------------------------------------------------

/// The first backticked word of the first cell of each row of the table headed `first`.
fn documented(doc: &str, first: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut inside = false;
    for line in doc.lines() {
        let line = line.trim();
        if !line.starts_with('|') {
            inside = false;
            continue;
        }
        let cell = line.trim_matches('|').split('|').next().unwrap().trim();
        if cell == first {
            inside = true;
            continue;
        }
        if !inside || cell.chars().all(|c| c == '-' || c == ':') {
            continue;
        }
        out.insert(cell.split('`').nth(1).unwrap().to_string());
    }
    out
}

/// The field paths of a printed value: `a.b`, `a[].b`.
fn paths(v: &Value, prefix: &str, out: &mut BTreeSet<String>) {
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                paths(x, &format!("{prefix}.{k}"), out);
            }
        }
        Value::Array(a) => a.iter().for_each(|x| paths(x, &format!("{prefix}[]"), out)),
        _ => {
            out.insert(prefix.to_string());
        }
    }
}

#[test]
fn the_next_work_table_of_the_spec_is_what_is_printed() {
    let found = Org::scanned("shape", true, &[issue(81, "One", NAMES_G1)]);
    let none = Org::scanned("shapeless", false, &[issue(82, "Two", "x")]);
    let mut printed = BTreeSet::new();
    paths(&found.next_work(), "next_work", &mut printed);
    paths(&none.next_work(), "next_work", &mut printed);
    let doc = documented(SPEC, "Next work field");
    assert!(!doc.is_empty(), "spec.md has no table Next work field");
    assert_eq!(doc, printed);
    assert!(
        SPEC.contains("| `next_work` |"),
        "the Status field table has no row next_work"
    );
}

/// Issue 299: a body line `Depends on the belt's step 2.` names no Issue number, so the scan leaves
/// the Issue out with the reason `dependency_unreadable` and the line; an Issue whose line names a
/// closed Issue (`Depends on #70.`, #70 not open) is a candidate.
#[test]
fn a_dependency_written_in_words_is_left_out_of_the_next_work() {
    let org = Org::scanned(
        "words",
        true,
        &[
            issue(71, "Step three", "Depends on the belt's step 2."),
            issue(72, "Step four", "Depends on #70."),
        ],
    );
    let v = org.next_work();
    let left = entry(&v, "left_out", "issue", json!(71));
    assert_eq!(left["kind"], json!("dependency_unreadable"), "{v}");
    assert!(
        left["reason"]
            .as_str()
            .is_some_and(|r| r.contains("Depends on the belt's step 2.")),
        "{v}"
    );
    let issues: Vec<&Value> = v["candidates"]
        .as_array()
        .unwrap_or_else(|| panic!("no candidates: {v}"))
        .iter()
        .map(|c| &c["issue"])
        .collect();
    assert!(issues.contains(&&json!(72)), "{v}");
    assert!(!issues.contains(&&json!(71)), "{v}");
}

/// Issue 410: an Issue the scan leaves out carries its title, as the scan read it; an entry for a
/// whole repository has the key `title`, null.
#[test]
fn an_issue_left_out_carries_its_title() {
    let mut aside = issue(94, "Set aside by hand", "x");
    aside["labels"] = json!([{"name": "on-hold"}]);
    let org = Org::scanned(
        "titled",
        true,
        &[
            issue(91, "Step three in words", "Depends on the belt's step 2."),
            issue(92, "Held by another", "Blocked by #93"),
            issue(93, "The blocker", NAMES_G1),
            aside,
        ],
    );
    let v = org.next_work();
    let words = entry(&v, "left_out", "issue", json!(91));
    assert_eq!(words["kind"], json!("dependency_unreadable"), "{v}");
    assert_eq!(words["title"], json!("Step three in words"), "{v}");
    let held = entry(&v, "left_out", "issue", json!(92));
    assert_eq!(held["kind"], json!("blocked"), "{v}");
    assert_eq!(held["title"], json!("Held by another"), "{v}");
    let put = entry(&v, "left_out", "issue", json!(94));
    assert_eq!(put["kind"], json!("put_aside"), "{v}");
    assert_eq!(put["title"], json!("Set aside by hand"), "{v}");

    let bare = Org::scanned("untitled", false, &[issue(95, "Any", "x")]);
    let w = bare.next_work();
    let e = entry(&w, "left_out", "project", json!("untitled"));
    assert_eq!(e["kind"], json!("no_goal_ledger"), "{w}");
    assert!(
        e.as_object().is_some_and(|o| o.contains_key("title")),
        "{w}"
    );
    assert_eq!(e["title"], Value::Null, "{w}");
}
