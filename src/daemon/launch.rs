//! Launching a prepared run (Issue M3-4): the step where a worker's environment is decided.
//!
//! A launch at `settings_ready` (M3-2) gets a key, whose hash goes into `agents` (M3-3); the agent
//! module builds `argv`, which carries the door's address and the token (spec section 4). The base
//! is asked for a terminal named `w-<task_id>` with the four tags of spec section 6.3, an `env`
//! that carries no key, and the unset list of spec section 6.4. Then `worker_spawned` and stage
//! `launched` are written in one transaction.
//!
//! A launch whose record could not be written is not launched again by this process: only the
//! record is retried. A launch the base refused is looked for by its `makiba.spawn` tag before it
//! is failed, since an error does not prove that no terminal exists.
//!
//! Before a key is issued the base is asked for a terminal carrying the launch's `makiba.spawn`
//! (Issue M3-7): one that exists was launched by a process that died before its record, and is
//! adopted (recorded, no key, no second launch). With none, keys of the launch left by such a
//! process are revoked first. Two or more fail the launch. (ponytail: a process killed while
//! `Base::launch` was in flight may see none and launch again; the base has no idempotent create.)

use crate::agents::{self, Agent, AgentLaunch, DoorAccess, Role};
use crate::base::{Base, LaunchSpec};
use crate::db::Result;
use crate::db::writer::{NewEvent, StateWriter};
use crate::spec::{self, TAG_ORG, TAG_ROLE, TAG_SPAWN, TAG_TASK_ID};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Spec section 6.4: removed for every agent (carried from paddock), then the base's own.
pub const UNSET: &[&str] = &[
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDECODE",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_PID",
    "CLAUDE_TMPDIR",
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
    "HC_SESSION",
    "HC_SOCKET",
    "HC_SCOPE",
    "HC_TERMINAL_ID",
    "HC_TERMINAL_SOCKET",
];

pub struct Launch {
    org: String,
    settings: PathBuf,
    door: String,
    /// Launches this process started whose record is not written yet, by spawn id.
    pending: Mutex<BTreeMap<String, Pending>>,
}

/// A launch this process asked the base for and has not recorded.
#[derive(Clone)]
enum Pending {
    /// The terminal exists; only the record is left.
    Up,
    /// The base refused and could not be asked whether the terminal exists after all: the key
    /// (by its hash) is kept until a `list` answers.
    Unsure { hash: String, refused: String },
}

enum Outcome {
    Up,
    Down(String),
    Unsure {
        hash: String,
        refused: String,
    },
    /// The base cannot list yet: nothing was issued or launched; the next pass tries again.
    Later,
}

struct Row {
    id: i64,
    run_id: i64,
    task: String,
    spawn: String,
    agent: String,
    model: Option<String>,
    brief: String,
    worktree: String,
}

impl Launch {
    /// `state` is the state folder (the `makiba.org` tag, and `settings/`); `door` the address
    /// agents reach the door at.
    pub fn new(state: &Path, door: String) -> Launch {
        let org = std::fs::canonicalize(state).unwrap_or_else(|_| state.to_path_buf());
        Launch {
            org: org.to_string_lossy().into_owned(),
            settings: state.join("settings"),
            door,
            pending: Mutex::new(BTreeMap::new()),
        }
    }

    /// One pass: launches every launch at `settings_ready`. Returns the task ids launched.
    pub fn pass(&self, w: &StateWriter, base: &dyn Base) -> Result<Vec<String>> {
        let rows: Vec<Row> = {
            let mut st = w.conn().prepare(
                "SELECT l.id, r.id, r.task_id, l.spawn_id, l.agent, l.model, l.brief, l.worktree
                 FROM run_launch l JOIN runs r ON r.id = l.run_id
                 WHERE l.stage = 'settings_ready' ORDER BY l.id",
            )?;
            st.query_map([], |r| {
                Ok(Row {
                    id: r.get(0)?,
                    run_id: r.get(1)?,
                    task: r.get(2)?,
                    spawn: r.get(3)?,
                    agent: r.get(4)?,
                    model: r.get(5)?,
                    brief: r.get(6)?,
                    worktree: r.get(7)?,
                })
            })?
            .collect::<std::result::Result<_, _>>()?
        };
        let mut launched = Vec::new();
        for row in rows {
            let pending = self.lock().get(&row.spawn).cloned();
            let outcome = match pending {
                Some(Pending::Up) => Outcome::Up,
                Some(Pending::Unsure { hash, refused }) => settle(w, base, &row, hash, refused),
                None if !agents::launchable(&row.agent) => continue,
                None => self.start(w, base, &row),
            };
            match outcome {
                Outcome::Up => {
                    self.lock().insert(row.spawn.clone(), Pending::Up);
                    record(w, &row)?;
                    super::crash::point("launched");
                    self.lock().remove(&row.spawn);
                    launched.push(row.task);
                }
                Outcome::Down(why) => {
                    self.lock().remove(&row.spawn);
                    fail(w, &row, &why)?;
                }
                Outcome::Unsure { hash, refused } => {
                    self.lock()
                        .insert(row.spawn.clone(), Pending::Unsure { hash, refused });
                }
                Outcome::Later => {}
            }
        }
        Ok(launched)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Pending>> {
        self.pending.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The launch spec of `row` with `token` in its argv (spec sections 4, 6.3, 6.4).
    fn spec(&self, row: &Row, agent: &dyn Agent, token: String) -> LaunchSpec {
        let door = DoorAccess {
            address: self.door.clone(),
            token,
        };
        let settings = self.settings.join(format!("{}.json", row.task));
        let argv = agent.argv(&AgentLaunch {
            role: Role::Worker,
            brief: Some(&row.brief),
            // ponytail: the role rules files are M4-3; no agent module reads this yet.
            rules_file: Path::new(""),
            settings_file: &settings,
            model: row.model.as_deref(),
            resume: None,
            door: &door,
        });
        let tags = BTreeMap::from([
            (TAG_ROLE.to_string(), "worker".to_string()),
            (TAG_TASK_ID.to_string(), row.task.clone()),
            (TAG_SPAWN.to_string(), row.spawn.clone()),
            (TAG_ORG.to_string(), self.org.clone()),
        ]);
        LaunchSpec {
            argv,
            cwd: PathBuf::from(&row.worktree),
            name: spec::worker_agent_id(&row.task),
            tags,
            // Workers get an empty GH_CONFIG_DIR (spec section 6.4); no key goes here.
            env: BTreeMap::from([("GH_CONFIG_DIR".to_string(), String::new())]),
            unset: UNSET.iter().map(|v| v.to_string()).collect(),
        }
    }

    /// Adopt the launch's terminal if it exists; otherwise issue the key and ask the base. A
    /// refusal is settled by looking for the terminal.
    fn start(&self, w: &StateWriter, base: &dyn Base, row: &Row) -> Outcome {
        let Some(agent) = agents::by_name(&row.agent) else {
            return Outcome::Down(format!("no agent module named {:?}", row.agent));
        };
        let Ok(terminals) = base.list() else {
            return Outcome::Later;
        };
        match terminals
            .iter()
            .filter(|t| t.tags.get(TAG_SPAWN) == Some(&row.spawn))
            .count()
        {
            0 => {}
            1 => return Outcome::Up,
            n => return Outcome::Down(format!("{n} terminals carry {TAG_SPAWN}={}", row.spawn)),
        }
        if let Err(e) = super::resume::revoke_keys(w, &row.spawn) {
            eprintln!("makibad: launch {}: {e}", row.task);
            return Outcome::Later;
        }
        let agent_id = spec::worker_agent_id(&row.task);
        let token = match super::door::issue_key(
            w,
            spec::Role::Worker,
            &agent_id,
            Some(row.run_id),
            Some(&row.spawn),
        ) {
            Ok(t) => t,
            Err(why) => return Outcome::Down(why),
        };
        let hash = super::door::sha256_hex(&token);
        match base.launch(&self.spec(row, &*agent, token)) {
            Ok(_) => {
                super::crash::point("launch_returned");
                Outcome::Up
            }
            Err(e) => settle(w, base, row, hash, e.to_string()),
        }
    }
}

/// A refused launch: up when a terminal with its `makiba.spawn` exists; down, with its key
/// revoked, when the base lists none; unsure while the base cannot list.
fn settle(w: &StateWriter, base: &dyn Base, row: &Row, hash: String, refused: String) -> Outcome {
    match base.list() {
        Ok(ts) if ts.iter().any(|t| t.tags.get(TAG_SPAWN) == Some(&row.spawn)) => Outcome::Up,
        Ok(_) => {
            let _ = w.conn().execute(
                "UPDATE agents SET revoked_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
                 WHERE key_sha256 = ?1",
                [hash],
            );
            Outcome::Down(refused)
        }
        Err(_) => Outcome::Unsure { hash, refused },
    }
}

/// Stage `failed` with `why`, and the launch's keys revoked, together.
fn fail(w: &StateWriter, row: &Row, why: &str) -> Result<()> {
    w.transaction(|w| {
        w.conn().execute(
            "UPDATE run_launch SET stage = 'failed', failed_reason = ?2 WHERE id = ?1",
            rusqlite::params![row.id, why],
        )?;
        super::resume::revoke_keys(w, &row.spawn)
    })
}

/// `worker_spawned` and stage `launched`, together.
fn record(w: &StateWriter, row: &Row) -> Result<()> {
    w.transaction(|w| {
        w.append_event(&NewEvent {
            kind: "worker_spawned",
            actor: Some("makibad"),
            payload: Some(&json!({"task_id": row.task, "spawn_id": row.spawn})),
            run_task_id: Some(&row.task),
            ..Default::default()
        })?;
        w.conn().execute(
            "UPDATE run_launch SET stage = 'launched' WHERE id = ?1",
            [row.id],
        )?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::TerminalRef;
    use crate::base::stub::Stub;
    use crate::db::writer::RunUpsert;

    fn org() -> (tempfile::TempDir, StateWriter, Row) {
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
                 VALUES ((SELECT id FROM runs WHERE task_id = 't1'), 's1', 'fake', 'b', '/w',
                         'settings_ready')",
                [],
            )
            .unwrap();
        let row = Row {
            id: 1,
            run_id: 1,
            task: "t1".into(),
            spawn: "s1".into(),
            agent: "fake".into(),
            model: None,
            brief: "b".into(),
            worktree: "/w".into(),
        };
        (dir, w, row)
    }

    fn revoked(w: &StateWriter) -> Vec<bool> {
        let mut q = w
            .conn()
            .prepare("SELECT revoked_at IS NOT NULL FROM agents ORDER BY id")
            .unwrap();
        q.query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    /// One `worker_spawned`, linked to the run, with the launch's spawn id; stage `launched`.
    #[test]
    fn a_started_launch_is_recorded_once_with_its_spawn_id() {
        let (dir, w, row) = org();
        let stub = Stub::new();
        let l = Launch::new(dir.path(), "127.0.0.1:1".into());
        assert!(matches!(l.start(&w, &stub, &row), Outcome::Up));
        record(&w, &row).unwrap();
        let got = stub.get(&TerminalRef("w-t1".into())).unwrap();
        assert_eq!(got.tags["makiba.spawn"], "s1");
        let (n, payload, run): (i64, String, i64) = w
            .conn()
            .query_row(
                "SELECT count(*), payload_json, run_id FROM events WHERE kind = 'worker_spawned'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(n, 1);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&payload).unwrap(),
            json!({"task_id": "t1", "spawn_id": "s1"})
        );
        assert_eq!(run, 1);
        let stage: String = w
            .conn()
            .query_row("SELECT stage FROM run_launch", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stage, "launched");
        assert_eq!(revoked(&w), [false]);
    }

    /// A refusal fails the launch and revokes its key; a refusal while a terminal with the
    /// launch's spawn id exists does neither.
    #[test]
    fn a_refused_launch_revokes_its_key_unless_its_terminal_exists() {
        let (dir, w, row) = org();
        let l = Launch::new(dir.path(), "127.0.0.1:1".into());
        let taken = |spawn: &str| {
            let stub = Stub::new();
            stub.launch(&LaunchSpec {
                argv: vec![],
                cwd: "/".into(),
                name: "w-t1".into(),
                tags: BTreeMap::from([(TAG_SPAWN.to_string(), spawn.to_string())]),
                env: BTreeMap::new(),
                unset: vec![],
            })
            .unwrap();
            stub
        };
        // The name is taken by another launch's terminal: refused.
        assert!(matches!(
            l.start(&w, &taken("other"), &row),
            Outcome::Down(_)
        ));
        assert_eq!(revoked(&w), [true]);
        // The name is taken by this launch's own terminal: it exists, and is adopted without a
        // key (Issue M3-7).
        assert!(matches!(l.start(&w, &taken("s1"), &row), Outcome::Up));
        assert_eq!(revoked(&w), [true]);
    }

    /// A terminal of the launch left by a process that died before its record is adopted: no key,
    /// no second launch. Keys left by a process that died before its terminal existed are revoked
    /// before a new one is issued. Two terminals of the launch fail it.
    #[test]
    fn a_launch_looks_for_its_terminal_before_it_issues_a_key() {
        let (dir, w, row) = org();
        let l = Launch::new(dir.path(), "127.0.0.1:1".into());
        let stub = Stub::new();
        let left =
            super::super::door::issue_key(&w, spec::Role::Worker, "w-t1", Some(1), Some("s1"))
                .unwrap();
        assert!(!left.is_empty());
        assert!(matches!(l.start(&w, &stub, &row), Outcome::Up));
        assert_eq!(revoked(&w), [true, false], "the left key is revoked first");
        assert!(matches!(l.start(&w, &stub, &row), Outcome::Up));
        assert_eq!(revoked(&w), [true, false], "adopted: no key");
        stub.launch(&LaunchSpec {
            argv: vec![],
            cwd: "/".into(),
            name: "w-t1-again".into(),
            tags: BTreeMap::from([(TAG_SPAWN.to_string(), "s1".to_string())]),
            env: BTreeMap::new(),
            unset: vec![],
        })
        .unwrap();
        assert!(matches!(l.start(&w, &stub, &row), Outcome::Down(_)));
        assert_eq!(stub.list().unwrap().len(), 2, "no terminal is ended");
    }

    /// A refusal from a base that cannot list either keeps the launch and its key: the next pass
    /// asks again, and fails the launch only once the base lists no terminal for it.
    #[test]
    fn a_refusal_the_base_cannot_settle_waits_for_a_list() {
        let (dir, w, row) = org();
        let l = Launch::new(dir.path(), "127.0.0.1:1".into());
        let gone = crate::base::connect_as(
            &dir.path().join("none.sock"),
            std::time::Duration::from_secs(1),
            "org",
        );
        // A base that cannot list is not asked to launch: no key is issued.
        assert!(matches!(l.start(&w, &*gone, &row), Outcome::Later));
        assert!(revoked(&w).is_empty());
        // A launch refused and then not settled (the list after the refusal failed).
        let token =
            super::super::door::issue_key(&w, spec::Role::Worker, "w-t1", Some(1), Some("s1"))
                .unwrap();
        let hash = super::super::door::sha256_hex(&token);
        let Outcome::Unsure { hash, refused } = settle(&w, &*gone, &row, hash, "refused".into())
        else {
            panic!("a base that cannot list settles nothing");
        };
        assert_eq!(revoked(&w), [false]);
        l.lock()
            .insert("s1".into(), Pending::Unsure { hash, refused });
        assert!(l.pass(&w, &*gone).unwrap().is_empty());
        assert_eq!(revoked(&w), [false], "still unsure: the key stays");
        assert!(l.pass(&w, &Stub::new()).unwrap().is_empty());
        assert_eq!(revoked(&w), [true]);
        let stage: String = w
            .conn()
            .query_row("SELECT stage FROM run_launch", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stage, "failed");
    }

    /// An unknown agent module fails the launch with a reason and issues no key.
    #[test]
    fn an_unknown_agent_fails_the_launch_without_a_key() {
        let (dir, w, _) = org();
        w.conn()
            .execute("UPDATE run_launch SET agent = 'nope'", [])
            .unwrap();
        let l = Launch::new(dir.path(), "127.0.0.1:1".into());
        assert!(l.pass(&w, &Stub::new()).unwrap().is_empty());
        let (stage, why): (String, String) = w
            .conn()
            .query_row("SELECT stage, failed_reason FROM run_launch", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(stage, "failed");
        assert!(why.contains("nope"), "{why}");
        assert!(revoked(&w).is_empty());
    }
}
