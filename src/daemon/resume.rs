//! The start pass of a launch (Issue M3-7, spec section 5.3): on start, every launch at
//! `launched` or `dialog_passed` is looked for by its `makiba.spawn` tag. Exactly one terminal
//! carrying it: adopted; nothing is written, and the dialog and verify steps take it on from its
//! stage. None, or more than one (`makibad` cannot tell which one is the launch's): the launch is
//! failed by the path of M3-6 (`worker_spawn_failed`, stage `failed`, the run `failed`, a notice)
//! and its keys are revoked, together. A terminal is never ended here.
//!
//! The stages before `launched` need no start pass: each pass reads the stage, so a new process
//! continues from it, and the launch step looks for the tag before it issues a key (launch.rs).

use crate::base::Base;
use crate::db::Result;
use crate::db::writer::StateWriter;
use crate::spec::TAG_SPAWN;

/// Revoke every live key issued for the launch `spawn`.
pub(super) fn revoke_keys(w: &StateWriter, spawn: &str) -> Result<()> {
    w.conn().execute(
        "UPDATE agents SET revoked_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE spawn_id = ?1 AND revoked_at IS NULL",
        [spawn],
    )?;
    Ok(())
}

/// End every terminal carrying the launch's `makiba.spawn`: a pass found its launch failed or
/// suspended by somebody else while it worked on it (Issue 163), so nothing runs for it. Best
/// effort: an error is printed. `false` when the base could not list.
pub(super) fn end_terminals(base: &dyn Base, spawn: &str) -> bool {
    let terminals = match base.list() {
        Ok(ts) => ts,
        Err(e) => {
            eprintln!("makibad: cannot list to end the terminal of {spawn}: {e}");
            return false;
        }
    };
    for t in terminals
        .iter()
        .filter(|t| t.tags.get(TAG_SPAWN).map(String::as_str) == Some(spawn))
    {
        if let Err(e) = base.end(&t.id) {
            eprintln!("makibad: cannot end {}: {e}", t.id.0);
        }
    }
    true
}

/// A launch at `stage` whose terminal cannot be adopted: failed by the path of M3-6 and its keys
/// revoked, in one transaction. No terminal is ended. Nothing is written for a launch no longer at
/// `stage` (Issue 163).
pub(super) fn fail(
    w: &StateWriter,
    id: i64,
    task: &str,
    spawn: &str,
    stage: &str,
    why: &str,
) -> Result<()> {
    w.transaction(|w| {
        if super::verify::fail(w, id, task, spawn, stage, why, &[])? {
            revoke_keys(w, spawn)?;
        }
        Ok(())
    })
}

/// One try of the start pass. `true` once it is done; `false` while the base cannot list.
pub fn pass(w: &StateWriter, base: &dyn Base) -> Result<bool> {
    let rows: Vec<(i64, String, String, String)> = {
        let mut st = w.conn().prepare(
            "SELECT l.id, r.task_id, l.spawn_id, l.stage
             FROM run_launch l JOIN runs r ON r.id = l.run_id
             WHERE l.stage IN ('launched', 'dialog_passed') ORDER BY l.id",
        )?;
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<std::result::Result<_, _>>()?
    };
    if rows.is_empty() {
        return Ok(true);
    }
    let Ok(terminals) = base.list() else {
        return Ok(false);
    };
    for (id, task, spawn, stage) in rows {
        let n = terminals
            .iter()
            .filter(|t| t.tags.get(TAG_SPAWN) == Some(&spawn))
            .count();
        let why = match n {
            1 => continue,
            0 => format!("no terminal carries {TAG_SPAWN}={spawn} after a restart"),
            n => format!("{n} terminals carry {TAG_SPAWN}={spawn}"),
        };
        fail(w, id, &task, &spawn, &stage, &why)?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::LaunchSpec;
    use crate::base::stub::Stub;
    use crate::db::writer::RunUpsert;
    use std::collections::BTreeMap;

    /// One launch of `t1` at `stage`, with spawn id `s1` and one live key.
    fn org(stage: &str) -> (tempfile::TempDir, StateWriter) {
        let dir = tempfile::tempdir().unwrap();
        let w = StateWriter::open(&dir.path().join("state.db")).unwrap();
        w.upsert_run(&RunUpsert {
            task_id: "t1",
            project_slug: "p",
            status: Some("queued"),
            ..Default::default()
        })
        .unwrap();
        w.conn()
            .execute(
                "INSERT INTO run_launch (run_id, spawn_id, agent, brief, worktree, stage)
                 VALUES (1, 's1', 'fake', 'b', '/w', ?1)",
                [stage],
            )
            .unwrap();
        w.conn()
            .execute(
                "INSERT INTO agents (agent_id, role, run_id, key_sha256, spawn_id)
                 VALUES ('w-t1', 'worker', 1, 'h1', 's1')",
                [],
            )
            .unwrap();
        (dir, w)
    }

    fn terminal(stub: &Stub, name: &str, spawn: &str) {
        stub.launch(&LaunchSpec {
            argv: vec![],
            cwd: "/".into(),
            name: name.into(),
            tags: BTreeMap::from([(TAG_SPAWN.to_string(), spawn.to_string())]),
            env: BTreeMap::new(),
            unset: vec![],
        })
        .unwrap();
    }

    /// (stage, run status, key revoked, worker_spawn_failed count)
    fn state(w: &StateWriter) -> (String, String, bool, i64) {
        w.conn()
            .query_row(
                "SELECT (SELECT stage FROM run_launch), (SELECT status FROM runs),
                        (SELECT revoked_at IS NOT NULL FROM agents),
                        (SELECT count(*) FROM events WHERE kind = 'worker_spawn_failed')",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap()
    }

    #[test]
    fn one_terminal_is_adopted_and_nothing_is_written() {
        for stage in ["launched", "dialog_passed"] {
            let (_dir, w) = org(stage);
            let stub = Stub::new();
            terminal(&stub, "w-t1", "s1");
            terminal(&stub, "w-t2", "other");
            assert!(pass(&w, &stub).unwrap());
            assert_eq!(state(&w), (stage.into(), "queued".into(), false, 0));
        }
    }

    #[test]
    fn no_terminal_fails_the_launch_and_revokes_its_key() {
        for stage in ["launched", "dialog_passed"] {
            let (_dir, w) = org(stage);
            let stub = Stub::new();
            terminal(&stub, "w-t2", "other");
            assert!(pass(&w, &stub).unwrap());
            assert_eq!(state(&w), ("failed".into(), "failed".into(), true, 1));
        }
    }

    #[test]
    fn two_terminals_fail_the_launch_and_both_are_left() {
        let (_dir, w) = org("dialog_passed");
        let stub = Stub::new();
        terminal(&stub, "w-t1", "s1");
        terminal(&stub, "w-t1b", "s1");
        assert!(pass(&w, &stub).unwrap());
        assert_eq!(state(&w), ("failed".into(), "failed".into(), true, 1));
        assert_eq!(stub.list().unwrap().len(), 2);
    }

    #[test]
    fn a_base_that_cannot_list_leaves_the_pass_undone() {
        let (dir, w) = org("launched");
        let gone = crate::base::connect_as(
            &dir.path().join("none.sock"),
            std::time::Duration::from_secs(1),
            "org",
        );
        assert!(!pass(&w, &*gone).unwrap());
        assert_eq!(state(&w), ("launched".into(), "queued".into(), false, 0));
    }
}
