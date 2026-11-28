//! Issue 646: a dependency line whose refs are separated by spaces is read.
//!
//! The issue skill (`skills/issue/SKILL.md`) tells the secretary to end a body with one line
//! `Depends on #N #M`, refs separated by a space, while the one form the parser read
//! (`src/work_discovery/refs.rs` `STRICT_FORM`) joined refs only by `,`, `and` or `&`: every Issue
//! written by the skill with two or more dependencies was left out of every range and of the
//! triage as `dependency_unreadable`. Now a run of refs separated by whitespace is the one form
//! too, in `dependency_lines` and in the belt's candidate read; the form line of the skill is read
//! by the parser, so the two cannot drift apart; and what was unreadable before (words, prose
//! around the refs, a URL, a second keyword, a ref on the next line) stays unreadable.
//!
//! The belt's scaffolding is that of `tests/belt_dependency_words.rs`: the daemon runs in this
//! process on the stand-in base, `gh` is a stand-in script answering from files, and no test
//! reaches the network.

mod common;

use common::makibad;
use makiba::base::stub::Stub;
use makiba::daemon::door::{Door, issue_key};
use makiba::db::writer::StateWriter;
use makiba::spec::Role;
use makiba::work_discovery::refs::{DepLines, dependency_lines};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Once, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const SKILL: &str = include_str!("../skills/issue/SKILL.md");

fn read(text: &str) -> DepLines {
    dependency_lines(text)
}

fn home(numbers: &[i64]) -> DepLines {
    DepLines {
        home: numbers.to_vec(),
        cross: Vec::new(),
        unreadable: None,
    }
}

/// The skill's form line: the backtick-quoted `Depends on ...` of the step that says what the
/// last line of the body is.
fn skill_form() -> String {
    let step = SKILL
        .lines()
        .find(|l| l.contains("the last line of the body is"))
        .unwrap_or_else(|| panic!("the issue skill has no step naming the last line:\n{SKILL}"));
    let forms: Vec<&str> = step
        .split('`')
        .skip(1)
        .step_by(2)
        .filter(|s| s.starts_with("Depends on"))
        .collect();
    assert_eq!(forms.len(), 1, "one form on the step's line: {step}");
    forms[0].to_string()
}

/// The skill's form line, as the skill writes it, is read as the numbers it names.
#[test]
fn the_form_line_of_the_issue_skill_is_read_as_its_numbers() {
    let form = skill_form();
    // The frontmatter's description quotes the same form.
    assert!(
        SKILL.contains(&format!("\"{form}\"")),
        "the description names the form {form:?}:\n{SKILL}"
    );
    // Each placeholder `#<capital letters>` becomes a number: 3, 4, ...
    let mut numbers = Vec::new();
    let example: Vec<String> = form
        .split(' ')
        .map(|word| match word.strip_prefix('#') {
            Some(p) if !p.is_empty() && p.chars().all(|c| c.is_ascii_uppercase()) => {
                let n = 3 + numbers.len() as i64;
                numbers.push(n);
                format!("#{n}")
            }
            _ => word.to_string(),
        })
        .collect();
    let example = example.join(" ");
    assert!(
        numbers.len() >= 2,
        "the form names two refs or more: {form}"
    );
    assert_eq!(read(&example), home(&numbers), "{example}");
    let body = format!("What was seen.\n\nG2 The flow of work\n{example}\n");
    assert_eq!(read(&body), home(&numbers), "{body}");
}

/// The keyword followed by two refs separated by a space, and by three, is read as those numbers
/// with no unreadable line.
#[test]
fn refs_separated_by_spaces_are_read_as_those_numbers() {
    assert_eq!(read("Depends on #3 #4"), home(&[3, 4]));
    assert_eq!(read("Depends on #3 #4 #5"), home(&[3, 4, 5]));
    assert_eq!(read("Body.\n\nDepends on #591 #592\n"), home(&[591, 592]));
    assert_eq!(read("- Blocked by: #3  #4."), home(&[3, 4]));
    assert_eq!(
        read("Requires #3 o/r#4"),
        DepLines {
            home: vec![3],
            cross: vec![("o/r".to_string(), 4)],
            unreadable: None,
        }
    );
}

/// What the one form did not read before stays unreadable.
#[test]
fn what_was_unreadable_stays_unreadable() {
    for (text, line) in [
        (
            "Depends on the belt's step 2.",
            "Depends on the belt's step 2.",
        ),
        ("This mentions depends on #5", "This mentions depends on #5"),
        (
            "Depends on https://github.com/o/r/issues/30",
            "Depends on https://github.com/o/r/issues/30",
        ),
        ("Depends on #3 requires #4", "Depends on #3 requires #4"),
        ("Depends on #3 see #4", "Depends on #3 see #4"),
        ("Depends on #3 #4 later", "Depends on #3 #4 later"),
        ("Depends on #3#4", "Depends on #3#4"),
        ("Depends on\n#3 #4", "Depends on"),
    ] {
        assert_eq!(
            read(text).unreadable.as_deref(),
            Some(line),
            "{text:?} is unreadable"
        );
    }
}

// ---- The belt's candidate read ------------------------------------------------------------------

const WAIT: Duration = Duration::from_secs(60);
const HEAD: &str = "Brief head of the range: design first, then code; stop and ask before anything outside the issue.";

const STAND_IN: &str = r#"#!/bin/sh
# gh issue|pr list --repo github.com/<o>/<n> --state <s> ...: the file <what>-<state>.json, else [].
# gh issue|pr view <n> --repo github.com/<o>/<n> ...: the file <what>-<n>.json, else a failure.
what="$1 $2"; shift 2
num=
case "$what" in
  "issue list"|"pr list") ;;
  "issue view"|"pr view") num=$1; shift ;;
  *) echo "unexpected call: $what" >&2; exit 2 ;;
esac
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
dir="$FAKE_GH_DIR/${r%%/*}_${r#*/}"
mkdir -p "$dir"
if [ -n "$num" ]; then
  f="$dir/${what%% *}-$num.json"
  if [ -e "$f" ]; then cat "$f"; else echo "not found" >&2; exit 1; fi
else
  f="$dir/${what%% *}-$state.json"
  if [ -e "$f" ]; then cat "$f"; else echo '[]'; fi
fi
"#;

static HOME: OnceLock<tempfile::TempDir> = OnceLock::new();
static INIT: Once = Once::new();

/// The folder the belt's test uses: the `gh` stand-in and its answers, the repositories, the
/// workplaces.
fn init() -> PathBuf {
    INIT.call_once(|| {
        let home = HOME.get_or_init(|| tempfile::tempdir().unwrap());
        let root = std::fs::canonicalize(home.path()).unwrap();
        let script = root.join("gh");
        common::exe::write(&script, STAND_IN);
        for d in [
            "answers", "goals", "repos", "origins", "workers", "docs", "share", "config",
        ] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        // SAFETY: before this call returns no test of this file has started a thread of its own.
        unsafe {
            std::env::set_var("MAKIBA_GH", &script);
            std::env::set_var("FAKE_GH_DIR", root.join("answers"));
            std::env::set_var("MAKIBA_GOALS_DIR", root.join("goals"));
            std::env::set_var("MAKIBA_REPOS_DIR", root.join("repos"));
            std::env::set_var("MAKIBA_WORKERS_DIR", root.join("workers"));
            std::env::set_var("MAKIBA_DOCS_DIR", root.join("docs"));
            std::env::set_var("MAKIBA_SHARE_DIR", root.join("share"));
            std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
            std::env::set_var("MAKIBA_LAUNCH_AGENT", "fake");
            std::env::remove_var("MAKIBA_FAKE_AGENT");
            std::env::set_var("MAKIBA_MAX_CONCURRENT_WORKERS", "1");
            std::env::set_var("MAKIBA_MERGE_APPROVAL_MODE", "human");
            std::env::set_var("MAKIBA_PRWATCH_BUDGET_MS", "1000");
            std::env::set_var("MAKIBA_PRWATCH_INTERVAL_MS", "200");
            std::env::set_var("MAKIBA_RANGE_READ_INTERVAL_MS", "300");
        }
    });
    std::fs::canonicalize(HOME.get().unwrap().path()).unwrap()
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args([
            "-c",
            "init.defaultBranch=main",
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
}

fn issue(number: i64, title: &str, body: &str) -> Value {
    json!({"number": number, "title": title, "body": body, "labels": [], "milestone": null,
           "createdAt": "2026-11-01T00:00:00Z", "updatedAt": "2026-11-01T00:00:00Z",
           "comments": []})
}

/// One organization with one project `slug` (GitHub `o/<slug>`), its door and its daemon.
struct Org {
    _tmp: tempfile::TempDir,
    db: PathBuf,
    answers: PathBuf,
    door: Door,
    person: String,
    secretary: String,
    slug: String,
    stop: Arc<AtomicBool>,
    daemon: Option<JoinHandle<Result<(), String>>>,
}

impl Org {
    /// `issues`: the open Issues the stand-in answers.
    fn new(slug: &str, issues: &[Value]) -> Org {
        let home = init();
        let repo = home.join("repos").join(slug);
        let origin = home.join("origins").join(format!("{slug}.git"));
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "first"]);
        git(&home, &["init", "-q", "--bare", &origin.to_string_lossy()]);
        git(
            &repo,
            &["remote", "add", "origin", &origin.to_string_lossy()],
        );
        git(&repo, &["push", "-q", "origin", "main"]);
        git(&repo, &["fetch", "-q", "origin"]);
        let answers = home.join("answers").join(format!("o_{slug}"));
        std::fs::create_dir_all(&answers).unwrap();
        std::fs::write(
            answers.join("issue-open.json"),
            Value::Array(issues.to_vec()).to_string(),
        )
        .unwrap();

        let tmp = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(tmp.path()).unwrap().join("state");
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("state.db");
        let w = StateWriter::open(&db).unwrap();
        w.ensure_project(slug, None, Some(&format!("https://github.com/o/{slug}")))
            .unwrap();
        let person = issue_key(&w, Role::Person, "person", None, None).unwrap();
        let secretary = issue_key(&w, Role::Secretary, "secretary", None, None).unwrap();
        let door = Door::new(w, Arc::new(Stub::new()), "fake".into());
        let stop = Arc::new(AtomicBool::new(false));
        let daemon = {
            let (dir, flag) = (dir.clone(), Arc::clone(&stop));
            Some(std::thread::spawn(move || {
                makibad::run_until(&dir, &|| Box::new(Stub::new()), flag)
            }))
        };
        Org {
            _tmp: tmp,
            db,
            answers,
            door,
            person,
            secretary,
            slug: slug.into(),
            stop,
            daemon,
        }
    }

    fn call(&self, key: &str, verb: &str, args: Value) -> Value {
        self.door
            .handle(Some(key), verb, args.clone(), None, true)
            .unwrap_or_else(|e| panic!("{verb} {args} refused: {e:?}"))
    }

    fn read_done(&self, id: i64) -> bool {
        let c = Connection::open_with_flags(&self.db, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let mut q = c
            .prepare("SELECT payload_json FROM events WHERE kind = 'range_read' ORDER BY id")
            .unwrap();
        q.query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|p| serde_json::from_str::<Value>(&p.unwrap()).unwrap())
            .any(|p| p["range_id"] == json!(id) && p["complete"] == json!(true))
    }

    /// The range `id` on the person's read surface.
    fn range(&self, id: i64) -> Value {
        let s = self.call(&self.person, "status", json!({}));
        s["ranges"]
            .as_array()
            .unwrap_or_else(|| panic!("status has no ranges[]: {s}"))
            .iter()
            .find(|r| r["id"] == json!(id))
            .cloned()
            .unwrap_or_else(|| panic!("range {id} not listed: {s}"))
    }
}

impl Drop for Org {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(d) = self.daemon.take() {
            let _ = d.join();
        }
    }
}

fn wait(what: &str, mut ok: impl FnMut() -> bool) {
    let until = Instant::now() + WAIT;
    while !ok() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn candidate(range: &Value, n: i64) -> Value {
    range["candidates"]
        .as_array()
        .unwrap_or_else(|| panic!("no candidates[]: {range}"))
        .iter()
        .find(|c| c["issue"] == json!(n))
        .cloned()
        .unwrap_or_else(|| panic!("no candidate {n}: {range}"))
}

/// The belt's candidate read: two refs separated by a space naming closed Issues are admitted;
/// three, the last open, wait for the open one; prose around the refs stays unreadable.
#[test]
fn the_belt_reads_refs_separated_by_spaces() {
    let org = Org::new(
        "spaced",
        &[
            issue(30, "Open step", "Step."),
            issue(31, "Two refs", "Step.\n\nDepends on #3 #4\n"),
            issue(32, "Three refs", "Step.\n\nDepends on #3 #4 #30\n"),
            issue(33, "Prose", "Step.\n\nDepends on #3 see #4\n"),
        ],
    );
    for n in [3, 4] {
        std::fs::write(
            org.answers.join(format!("issue-{n}.json")),
            json!({"number": n, "title": "Done", "state": "CLOSED"}).to_string(),
        )
        .unwrap();
    }
    let r = org.call(
        &org.secretary,
        "range",
        json!({"action": "propose", "project": org.slug, "issues": [31, 32, 33], "at_once": 1,
               "budget_tasks": 3, "brief_head": HEAD}),
    );
    let id = r["id"]
        .as_i64()
        .unwrap_or_else(|| panic!("no range id: {r}"));
    wait("the read of the draft", || org.read_done(id));
    let draft = org.range(id);

    let two = candidate(&draft, 31);
    assert_eq!(two["admitted"], json!(true), "{draft}");
    assert_eq!(two["left_out_why"], Value::Null, "{draft}");

    let three = candidate(&draft, 32);
    assert_eq!(three["admitted"], json!(false), "{draft}");
    assert_eq!(three["left_out_why"], json!("waits_for #30"), "{draft}");

    let prose = candidate(&draft, 33);
    assert_eq!(prose["admitted"], json!(false), "{draft}");
    assert_eq!(
        prose["left_out_why"],
        json!("dependency_unreadable (Depends on #3 see #4)"),
        "{draft}"
    );
}
