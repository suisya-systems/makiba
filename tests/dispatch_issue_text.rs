//! Issue 317: a dispatch that names an Issue gives the worker the Issue's text.
//!
//! - With `issue`, `makibad` reads the Issue's title, body and comments from GitHub at dispatch
//!   and the launch's brief (the worker's first prompt) carries them after the secretary's brief.
//! - Without `issue`, nothing is read and the brief is the secretary's, unchanged.
//! - An Issue that cannot be read is said in the brief and in the answer, and the dispatch goes in.
//!
//! The door is called in this process on the stand-in base; `makibad` is not started. `gh` is a
//! stand-in script (`MAKIBA_GH`) that answers `issue view` from files in a folder per repository,
//! records each call, and refuses anything else, so no test reaches the network.

use makiba::base::stub::Stub;
use makiba::daemon::door::{Door, issue_key};
use makiba::db::writer::StateWriter;
use makiba::spec::Role;
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Once, OnceLock};

const BRIEF: &str = "Secretary brief.";

const STAND_IN: &str = r#"#!/bin/sh
# gh issue view <n> --repo github.com/<o>/<n> ...: the file issue-<n>.json, else a failure.
[ "$1 $2" = "issue view" ] || { echo "unexpected call: $1 $2" >&2; exit 2; }
num=$3; shift 3
repo=
while [ $# -gt 0 ]; do
  case "$1" in
    --repo) repo=$2; shift 2 ;;
    --json) shift 2 ;;
    *) echo "unexpected argument: $1" >&2; exit 2 ;;
  esac
done
case "$repo" in github.com/*/*) ;; *) echo "unexpected repo: $repo" >&2; exit 2 ;; esac
r=${repo#github.com/}
dir="$FAKE_GH_DIR/${r%%/*}_${r#*/}"
mkdir -p "$dir"
echo "issue view $num $repo" >> "$dir/calls"
f="$dir/issue-$num.json"
if [ -e "$f" ]; then cat "$f"; else echo "not found" >&2; exit 1; fi
"#;

static HOME: OnceLock<tempfile::TempDir> = OnceLock::new();
static INIT: Once = Once::new();

/// The folder the tests share: the `gh` stand-in and its answers, repositories and workplaces.
fn init() -> PathBuf {
    INIT.call_once(|| {
        let home = HOME.get_or_init(|| tempfile::tempdir().unwrap());
        let root = std::fs::canonicalize(home.path()).unwrap();
        let script = root.join("gh");
        std::fs::write(&script, STAND_IN).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        for d in ["answers", "repos", "workers", "docs"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        // SAFETY: before this call returns no test of this file has started a thread of its own.
        unsafe {
            std::env::set_var("MAKIBA_GH", &script);
            std::env::set_var("FAKE_GH_DIR", root.join("answers"));
            std::env::set_var("MAKIBA_REPOS_DIR", root.join("repos"));
            std::env::set_var("MAKIBA_WORKERS_DIR", root.join("workers"));
            std::env::set_var("MAKIBA_DOCS_DIR", root.join("docs"));
        }
    });
    std::fs::canonicalize(HOME.get().unwrap().path()).unwrap()
}

/// One organization with one project `slug` (GitHub `o/<slug>`) and its door.
struct Org {
    _tmp: tempfile::TempDir,
    db: PathBuf,
    answers: PathBuf,
    door: Door,
    secretary: String,
    slug: String,
}

impl Org {
    fn new(slug: &str) -> Org {
        let home = init();
        let answers = home.join("answers").join(format!("o_{slug}"));
        std::fs::create_dir_all(&answers).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(tmp.path()).unwrap().join("state");
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("state.db");
        let w = StateWriter::open(&db).unwrap();
        w.ensure_project(slug, None, Some(&format!("https://github.com/o/{slug}")))
            .unwrap();
        let secretary = issue_key(&w, Role::Secretary, "secretary", None, None).unwrap();
        let door = Door::new(w, Arc::new(Stub::new()), "fake".into());
        Org {
            _tmp: tmp,
            db,
            answers,
            door,
            secretary,
            slug: slug.into(),
        }
    }

    fn dispatch(&self, task: &str, issue: Option<i64>) -> Value {
        let mut args = json!({"kind": "code", "task_id": task, "title": "fix",
                              "project": self.slug, "brief": BRIEF});
        if let Some(n) = issue {
            args["issue"] = json!(n);
        }
        self.door
            .handle(Some(&self.secretary), "dispatch", args.clone(), None, true)
            .unwrap_or_else(|e| panic!("dispatch {args} refused: {e:?}"))
    }

    /// The brief of the task's first launch: what the worker's first prompt is made from.
    fn brief(&self, task: &str) -> String {
        Connection::open_with_flags(&self.db, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap()
            .query_row(
                "SELECT l.brief FROM run_launch l JOIN runs r ON r.id = l.run_id
                 WHERE r.task_id = ?1 ORDER BY l.id LIMIT 1",
                [task],
                |r| r.get(0),
            )
            .unwrap()
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(self.answers.join("calls")).unwrap_or_default()
    }
}

fn write(path: &Path, v: &Value) {
    std::fs::write(path, v.to_string()).unwrap();
}

#[test]
fn an_issue_named_by_dispatch_is_in_the_launch_prompt() {
    let org = Org::new("p317");
    write(
        &org.answers.join("issue-42.json"),
        &json!({"title": "Widgets lose their color",
                "body": "BODY-317: the widget turns grey after a restart.",
                "comments": [{"author": {"login": "someone"},
                              "body": "COMMENT-317: also after an upgrade.",
                              "createdAt": "2026-11-02T00:00:00Z"}]}),
    );
    let answer = org.dispatch("t42", Some(42));
    assert_eq!(answer["status"], json!("queued"), "{answer}");
    assert_eq!(answer["issue_text"], json!("read"), "{answer}");
    let brief = org.brief("t42");
    assert!(brief.starts_with(BRIEF), "{brief}");
    for part in [
        "Issue #42: Widgets lose their color",
        "BODY-317: the widget turns grey after a restart.",
        "- someone (2026-11-02T00:00:00Z):",
        "COMMENT-317: also after an upgrade.",
    ] {
        assert!(brief.contains(part), "{part:?} not in the brief: {brief}");
    }
    let calls = org.calls();
    assert!(
        calls
            .lines()
            .any(|l| l.contains("issue view 42") && l.contains("github.com/o/p317")),
        "{calls}"
    );
}

#[test]
fn a_dispatch_without_an_issue_keeps_its_brief_and_reads_nothing() {
    let org = Org::new("p317b");
    let answer = org.dispatch("t1", None);
    assert_eq!(answer["status"], json!("queued"), "{answer}");
    assert_eq!(org.brief("t1"), BRIEF);
    assert_eq!(org.calls(), "");
}

#[test]
fn an_issue_that_cannot_be_read_is_said_and_the_dispatch_goes_in() {
    let org = Org::new("p317c");
    let answer = org.dispatch("t43", Some(43));
    assert_eq!(answer["status"], json!("queued"), "{answer}");
    assert!(
        answer["issue_text"]
            .as_str()
            .is_some_and(|s| s.starts_with("unreadable")),
        "{answer}"
    );
    let brief = org.brief("t43");
    assert!(brief.starts_with(BRIEF), "{brief}");
    assert!(brief.contains("Issue #43"), "{brief}");
    assert!(brief.contains("could not read"), "{brief}");
}
