//! Issue 134: preparing a `claude_code` launch puts `<share>/makiba`, the entry point the role
//! hooks run, as a link to this binary, and points a link to another file back here. Preparing a
//! launch of the test agent writes nothing into the share folder.

use makiba::base::BaseDenies;
use makiba::daemon::dispatch::Dispatch;
use makiba::daemon::prepare::{Places, Prepare};
use makiba::db::writer::{RunUpsert, StateWriter};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// A document launch of `task` by `agent`, taken by a dispatch pass.
fn taken(w: &StateWriter, root: &Path, task: &str, agent: &str) {
    w.upsert_run(&RunUpsert {
        task_id: task,
        project_slug: "p",
        status: Some("queued"),
        ..Default::default()
    })
    .unwrap();
    w.conn()
        .execute(
            "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree)
             VALUES ((SELECT id FROM runs WHERE task_id = ?1), ?1, 'document', ?2, 'b', ?3)",
            [task, agent, &root.join("docs").join(task).to_string_lossy()],
        )
        .unwrap();
    Dispatch::new(4).pass(w).unwrap();
}

#[test]
fn a_claude_code_launch_links_the_entry_point_and_the_test_agent_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let share = root.join("share");
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let claude = bin.join("claude");
    std::fs::write(&claude, "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
    // SAFETY: the only test of this binary, and no thread of it has started yet.
    unsafe {
        std::env::set_var("PATH", &bin);
        std::env::set_var("MAKIBA_SHARE_DIR", &share);
        std::env::set_var("MAKIBA_PERSON_KEY", root.join("person.key"));
        std::env::remove_var("MAKIBA_FAKE_AGENT");
    }
    let w = StateWriter::open(&root.join("state.db")).unwrap();
    let prepare = Prepare::new(
        Places {
            workers: root.join("workers"),
            docs: root.join("docs"),
            repos: root.join("repos"),
            settings: root.join("settings"),
            state_db: root.join("state.db"),
        },
        BaseDenies::default(),
    );
    let link = share.join("makiba");
    let exe = std::env::current_exe().unwrap();

    taken(&w, &root, "t-fake", "fake");
    assert_eq!(prepare.pass(&w).unwrap(), ["t-fake"]);
    assert!(
        !share.exists(),
        "the test agent wrote into the share folder"
    );

    taken(&w, &root, "t-cc", "claude_code");
    assert_eq!(prepare.pass(&w).unwrap(), ["t-cc"]);
    assert_eq!(std::fs::read_link(&link).unwrap(), exe);
    // The share folder as resolved is denied for write, wherever it is.
    let text = std::fs::read_to_string(root.join("settings/t-cc.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    let share_s = share.to_string_lossy();
    let named = |list: &serde_json::Value, want: &str| {
        list.as_array()
            .unwrap()
            .iter()
            .any(|x| x.as_str() == Some(want))
    };
    assert!(
        named(&v["sandbox"]["filesystem"]["denyWrite"], &share_s),
        "{text}"
    );
    assert!(
        named(&v["permissions"]["deny"], &format!("Edit(/{share_s}/**)")),
        "{text}"
    );

    std::fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink(root.join("elsewhere"), &link).unwrap();
    taken(&w, &root, "t-cc2", "claude_code");
    assert_eq!(prepare.pass(&w).unwrap(), ["t-cc2"]);
    assert_eq!(std::fs::read_link(&link).unwrap(), exe);
}
