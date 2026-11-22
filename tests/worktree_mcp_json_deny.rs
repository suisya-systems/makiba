//! Issue 137: a Bash write of `.mcp.json` inside the worktree. The worktree is the sandbox's write
//! surface, so the per-task settings written for a run deny that one file for write, by its
//! absolute path, as they deny the base's folders; and line L-K5 of `docs/design/lines.md` names
//! that holder instead of `Open: #137`. The daemon runs in this process on the stand-in base, as in
//! `tests/task_settings_shape.rs`; the host run of `scripts/probes/door-and-base-reach.sh` shows
//! what a sandboxed Bash really gets.

mod common;

use common::makibad;
use makiba::base::stub::Stub;
use makiba::db::writer::{RunUpsert, StateWriter};
use serde_json::Value;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

fn git(dir: &Path, args: &[&str]) {
    let st = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@e"])
        .args(args)
        .status()
        .unwrap();
    assert!(st.success(), "git {args:?}");
}

#[test]
fn the_settings_written_for_a_run_deny_a_write_of_mcp_json_in_its_worktree() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let dir = root.join("state");
    let repo = root.join("repos/p");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "m"]);
    let agent = serde_json::json!([root.join("no-such-agent"), root.join("record")]).to_string();
    // SAFETY: the only test of this binary, and no thread of it has started yet.
    unsafe {
        std::env::set_var("MAKIBA_PERSON_KEY", root.join("person.key"));
        std::env::set_var("MAKIBA_DOCS_DIR", root.join("docs"));
        std::env::set_var("MAKIBA_WORKERS_DIR", root.join("workers"));
        std::env::set_var("MAKIBA_REPOS_DIR", root.join("repos"));
        std::env::set_var("MAKIBA_FAKE_AGENT", &agent);
    }
    let worktree = root.join("workers/p/t1");
    {
        let w = StateWriter::open(&dir.join("state.db")).unwrap();
        w.ensure_project("p", None, None).unwrap();
        w.upsert_run(&RunUpsert {
            task_id: "t1",
            project_slug: "p",
            status: Some("queued"),
            ..Default::default()
        })
        .unwrap();
        w.conn()
            .execute(
                "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree)
                 VALUES ((SELECT id FROM runs WHERE task_id = 't1'), 'spawn-t1', 'code',
                         'fake', 'the brief', ?1)",
                [worktree.to_string_lossy()],
            )
            .unwrap();
    }
    let daemon_dir = dir.clone();
    std::thread::spawn(move || makibad::run(&daemon_dir, &|| Box::new(Stub::new())));
    let file = dir.join("settings/t1.json");
    let until = Instant::now() + Duration::from_secs(20);
    let v: Value = loop {
        if let Some(v) = std::fs::read_to_string(&file)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
        {
            break v;
        }
        assert!(Instant::now() < until, "no settings at {}", file.display());
        std::thread::sleep(Duration::from_millis(25));
    };
    assert!(worktree.join(".git").exists(), "the worktree was not made");

    // 1. `<worktree>/.mcp.json` is denied for write: by the sandbox (Bash), by its absolute path.
    let mcp = worktree.join(".mcp.json").display().to_string();
    let fs = &v["sandbox"]["filesystem"];
    assert!(
        strings(&fs["denyWrite"]).contains(&mcp),
        "sandbox.filesystem.denyWrite does not name {mcp}: {v}"
    );
    assert!(
        strings(&fs["allowWrite"]).contains(&worktree.display().to_string()),
        "the worktree is not the write surface, so the deny proves nothing: {v}"
    );

    // 2. Line L-K5 names a holder, not `Open: #137`, and that holder is this test.
    let lines =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/design/lines.md"))
            .unwrap();
    let row = lines
        .lines()
        .find(|l| l.starts_with("| L-K5 "))
        .expect("lines.md has no L-K5 row");
    assert!(
        !row.contains("Open: #137"),
        "L-K5 still says Open: #137: {row}"
    );
    assert!(
        row.contains(
            "`tests/worktree_mcp_json_deny.rs::the_settings_written_for_a_run_deny_a_write_of_mcp_json_in_its_worktree`"
        ),
        "L-K5 does not name the per-task deny's holder: {row}"
    );
}
