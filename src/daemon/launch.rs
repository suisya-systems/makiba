//! Launching a prepared run (Issue M3-4): the step where a worker's environment is decided.
//!
//! A launch at `settings_ready` (M3-2) gets a key, whose hash goes into `agents` (M3-3); the agent
//! module builds `argv`, which carries the door's address and the token (spec section 4). The base
//! is asked for a terminal named `w-<task_id>` with the four tags of spec section 6.3, an `env`
//! that carries no key, and the unset list of spec section 6.4. Then `worker_spawned` and stage
//! `launched` are written in one transaction.
//!
//! A launch without a run (Issue 166, `add_launch`) takes the same steps from its row and role:
//! its role's rules and settings, a terminal named `curator` or `secretary` without the
//! `makiba.task_id` tag, and a key of its role with no run.
//!
//! A launch the base refused, once settled, fails by the path of M3-6 (Issue 124), as does one
//! that cannot be prepared (prepare.rs): the run `failed` and the secretary told.
//!
//! A launch whose record could not be written is not launched again by this process: only the
//! record is retried. A launch the base refused is looked for by its `makiba.spawn` tag before it
//! is failed, since an error does not prove that no terminal exists.
//!
//! Before a key is issued the base is asked for a terminal carrying the launch's `makiba.spawn`
//! (Issue M3-7): one that exists was launched by a process that died before its record, and is
//! adopted (recorded, no key, no second launch). Two or more fail the launch and its run as
//! resume.rs does.
//!
//! A process killed while `Base::launch` was in flight (Issue 122) may leave a terminal that
//! appears late. The base refuses a second live terminal of the same name (`name_taken`) before it
//! starts the child, and a worker's terminal is named `w-<task_id>`: of the dead process's request
//! and this one's, exactly one child runs. With none seen, this process launches with a new key and
//! settles the keys by the outcome: a launch that returns revokes the keys the dead process left; a
//! refusal while a terminal carries the `makiba.spawn` tag adopts that terminal: it may carry this
//! attempt's key (only the reply was lost) or a left one.
//!
//! Every terminal carries the hash of its key in the `makiba.key` tag (Issue 131), so an adoption
//! keeps that key and revokes every other key of the launch. A terminal without the tag (started
//! before it existed) revokes none.

use crate::agents::{self, Agent, AgentLaunch, DoorAccess, Role};
use crate::base::Terminal;
use crate::base::{Base, LaunchSpec};
use crate::db::Result;
use crate::db::writer::{NewEvent, StateWriter};
use crate::spec::{self, TAG_KEY, TAG_ORG, TAG_ROLE, TAG_SPAWN, TAG_TASK_ID};
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
    // Bare mode turns skills off (Issue 182).
    "CLAUDE_CODE_SIMPLE",
    "HC_SESSION",
    "HC_SOCKET",
    "HC_SCOPE",
    "HC_TERMINAL_ID",
    "HC_TERMINAL_SOCKET",
];

/// Spec section 6.4: given to the secretary as `makibad` has them, removed when it has none, so
/// the base's configuration folder its terminals inherit does not reach it (Issue 295).
pub const PERSON_CONFIG: &[&str] = &["XDG_CONFIG_HOME", "GH_CONFIG_DIR"];

/// What a resumed worker is given in place of its brief (Issue 41): the conversation already holds
/// the task, and the brief again reads as a second request.
pub const RESUME_PROMPT: &str = "The organization was restarted. Go on with the task you were working on in this conversation. If it is already done and reported, do nothing and do not report again.";

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
    /// More than one terminal carries the launch's spawn id: failed as in resume.rs.
    Several(String),
}

struct Row {
    id: i64,
    /// `None` for a launch without a run (Issue 166), and `task` with it.
    run_id: Option<i64>,
    task: Option<String>,
    role: String,
    spawn: String,
    agent: String,
    model: Option<String>,
    brief: String,
    worktree: String,
    /// The run's project, for its artifact language (Issue 48).
    project: Option<String>,
    /// Set on a launch that resumes a conversation (`makiba up`, Issue 41).
    resume: Option<String>,
    /// Whether the run had an earlier launch: a relaunch first ends the run's old terminals.
    relaunch: bool,
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
                "SELECT l.id, r.id, r.task_id, l.role, l.spawn_id, l.agent, l.model, l.brief,
                        l.worktree, l.conversation_id,
                        EXISTS (SELECT 1 FROM run_launch e WHERE e.run_id = l.run_id AND e.id < l.id),
                        p.slug
                 FROM run_launch l LEFT JOIN runs r ON r.id = l.run_id
                 LEFT JOIN projects p ON p.id = r.project_id
                 WHERE l.stage = 'settings_ready' ORDER BY l.id",
            )?;
            st.query_map([], |r| {
                Ok(Row {
                    id: r.get(0)?,
                    run_id: r.get(1)?,
                    task: r.get(2)?,
                    role: r.get(3)?,
                    spawn: r.get(4)?,
                    agent: r.get(5)?,
                    model: r.get(6)?,
                    brief: r.get(7)?,
                    worktree: r.get(8)?,
                    resume: r.get(9)?,
                    relaunch: r.get(10)?,
                    project: r.get(11)?,
                })
            })?
            .collect::<std::result::Result<_, _>>()?
        };
        // A launch this process started and has not recorded, failed meanwhile by somebody else
        // (Issue 163): its terminal is ended once the base can list.
        let gone: Vec<String> = self
            .lock()
            .keys()
            .filter(|s| !rows.iter().any(|r| &r.spawn == *s))
            .cloned()
            .collect();
        for spawn in gone {
            if super::resume::end_terminals(base, &spawn) {
                self.lock().remove(&spawn);
            }
        }
        let mut launched = Vec::new();
        for mut row in rows {
            // The language sentences are read at each first launch, so a change reaches the next
            // conversation; a resume keeps the one it had (Issue 48).
            if row.resume.is_none() {
                row.brief = super::language::task_text(
                    w.conn(),
                    &row.role,
                    row.project.as_deref(),
                    &row.brief,
                )?;
            }
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
                    if record(w, &row)? {
                        super::crash::point("launched");
                        self.lock().remove(&row.spawn);
                        launched.push(row.name());
                    } else {
                        // Failed meanwhile by somebody else (Issue 163): nothing runs for it,
                        // and the key this pass issued for it is revoked.
                        super::resume::revoke_keys(w, &row.spawn)?;
                        if super::resume::end_terminals(base, &row.spawn) {
                            self.lock().remove(&row.spawn);
                        }
                    }
                }
                Outcome::Down(why) => {
                    self.lock().remove(&row.spawn);
                    let task = row.task.as_deref();
                    super::resume::fail(w, row.id, task, &row.spawn, "settings_ready", &why)?;
                }
                Outcome::Unsure { hash, refused } => {
                    self.lock()
                        .insert(row.spawn.clone(), Pending::Unsure { hash, refused });
                }
                Outcome::Later => {}
                Outcome::Several(why) => {
                    let task = row.task.as_deref();
                    super::resume::fail(w, row.id, task, &row.spawn, "settings_ready", &why)?;
                }
            }
        }
        Ok(launched)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Pending>> {
        self.pending.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The launch spec of `row` with `token` in its argv (spec sections 4, 6.3, 6.4).
    fn spec(&self, row: &Row, role: Role, agent: &dyn Agent, token: String) -> LaunchSpec {
        let key_hash = super::door::sha256_hex(&token);
        let door = DoorAccess {
            address: self.door.clone(),
            token,
        };
        let settings = settings_file(&self.settings, row.task.as_deref(), &row.role, &row.spawn);
        let rules = agents::rules_path(role);
        // A first launch names its conversation (Issue 41); a resume names the stored one.
        let session = row.resume.is_none().then(|| agents::session_id(&row.spawn));
        let argv = agent.argv_with_session(
            &AgentLaunch {
                role,
                brief: Some(match row.resume {
                    Some(_) => RESUME_PROMPT,
                    None => &row.brief,
                }),
                rules_file: &rules,
                settings_file: &settings,
                model: row.model.as_deref(),
                resume: row.resume.as_deref(),
                door: &door,
            },
            session.as_deref(),
        );
        let mut tags = BTreeMap::from([
            (TAG_ROLE.to_string(), row.role.clone()),
            (TAG_SPAWN.to_string(), row.spawn.clone()),
            (TAG_ORG.to_string(), self.org.clone()),
            (TAG_KEY.to_string(), key_hash),
        ]);
        if let Some(task) = &row.task {
            tags.insert(TAG_TASK_ID.to_string(), task.clone());
        }
        // Where the person's app shows the terminal (Issue 196).
        tags.extend(crate::base::app_labels(&self.org, &row.name()));
        // No key goes into the environment. The role hooks find the entry point by
        // MAKIBA_SHARE_DIR, as this process resolved it.
        let mut env = BTreeMap::from([(
            "MAKIBA_SHARE_DIR".to_string(),
            crate::cli::share_dir().display().to_string(),
        )]);
        let mut unset: Vec<String> = UNSET.iter().map(|v| v.to_string()).collect();
        // Workers and the curator get an empty GH_CONFIG_DIR (spec section 6.4).
        if role != Role::Secretary {
            env.insert("GH_CONFIG_DIR".to_string(), String::new());
        }
        // The secretary's merge gate hook finds its key file and the door where this process
        // put them (spec section 3): the paths, never the key.
        if role == Role::Secretary {
            // The person's own configuration folders, as makibad has them, over the base's that
            // its terminals inherit (Issue 295): set when makibad has one, removed when not.
            for var in PERSON_CONFIG {
                match std::env::var(var).ok().filter(|v| !v.is_empty()) {
                    Some(v) => {
                        env.insert(var.to_string(), v);
                    }
                    None => unset.push(var.to_string()),
                }
            }
            let abs = |p: PathBuf| std::path::absolute(&p).unwrap_or(p).display().to_string();
            env.insert(
                "MAKIBA_GATE_KEY".to_string(),
                abs(crate::cli::gate_key_path()),
            );
            if let Some(door) = crate::cli::door_path().filter(|_| !crate::cli::door_off()) {
                env.insert("MAKIBA_DOOR".to_string(), abs(door));
            }
        }
        // The launch's own temporary folder (Issue 243), when its preparation made one: a launch
        // prepared before it keeps the shared one its settings name.
        let tmp = super::prepare::tmp_dir(&self.settings, row.id);
        if tmp.is_dir() {
            for var in agents::TMP_VARS {
                env.insert(var.to_string(), tmp.display().to_string());
            }
        }
        LaunchSpec {
            argv,
            cwd: PathBuf::from(&row.worktree),
            name: row.name(),
            tags,
            env,
            unset,
        }
    }

    /// Adopt the launch's terminal if it exists; otherwise issue the key and ask the base. A
    /// refusal is settled by looking for the terminal.
    fn start(&self, w: &StateWriter, base: &dyn Base, row: &Row) -> Outcome {
        let Some(agent) = agents::by_name(&row.agent) else {
            return Outcome::Down(format!("no agent module named {:?}", row.agent));
        };
        let Some((role, agent_role)) = roles(&row.role) else {
            return Outcome::Down(format!("no launch role named {:?}", row.role));
        };
        let Ok(terminals) = base.list() else {
            return Outcome::Later;
        };
        let ours: Vec<&Terminal> = terminals
            .iter()
            .filter(|t| t.tags.get(TAG_SPAWN) == Some(&row.spawn))
            .collect();
        match ours.len() {
            0 => {}
            1 => return adopt(w, row, ours[0]),
            n => return Outcome::Several(format!("{n} terminals carry {TAG_SPAWN}={}", row.spawn)),
        }
        // A relaunch ends the run's earlier terminals of this organization first: the shell a core
        // stop left in place of a kept one, or a failed resume's (both hold the name).
        if row.relaunch
            && let Some(task) = &row.task
        {
            for t in terminals.iter().filter(|t| {
                t.tags.get(TAG_ORG) == Some(&self.org)
                    && t.tags.get(TAG_TASK_ID) == Some(task)
                    && t.tags.get(TAG_SPAWN) != Some(&row.spawn)
            }) {
                if let Err(e) = base.end(&t.id) {
                    return Outcome::Down(format!(
                        "cannot end the earlier terminal {}: {e}",
                        t.id.0
                    ));
                }
            }
        }
        // The secretary's skills, on every launch of it, before its terminal exists (Issue 182).
        // The test agent's tests leave the share folder at its default.
        if agent_role == Role::Secretary
            && agent.name() != "fake"
            && let Err(e) = agents::place_skills()
        {
            return Outcome::Down(format!("the secretary's skills: {e}"));
        }
        let token = match super::door::issue_key(w, role, &row.name(), row.run_id, Some(&row.spawn))
        {
            Ok(t) => t,
            Err(why) => return Outcome::Down(why),
        };
        let hash = super::door::sha256_hex(&token);
        match base.launch(&self.spec(row, agent_role, &*agent, token)) {
            Ok(_) => {
                // The child carries this key: the ones a dead process left are dead keys.
                let _ = w.conn().execute(
                    "UPDATE agents SET revoked_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
                     WHERE spawn_id = ?1 AND key_sha256 != ?2 AND revoked_at IS NULL",
                    [&row.spawn, &hash],
                );
                super::crash::point("launch_returned");
                Outcome::Up
            }
            Err(e) => settle(w, base, row, hash, e.to_string()),
        }
    }
}

/// A refused launch: up, adopting the terminal, when one carries the launch's `makiba.spawn`; down,
/// with every key of the launch revoked, when the base lists none; unsure while the base cannot
/// list.
fn settle(w: &StateWriter, base: &dyn Base, row: &Row, hash: String, refused: String) -> Outcome {
    match base.list() {
        Ok(ts) => match ts
            .iter()
            .find(|t| t.tags.get(TAG_SPAWN) == Some(&row.spawn))
        {
            Some(t) => adopt(w, row, t),
            None => {
                let _ = super::resume::revoke_keys(w, &row.spawn);
                Outcome::Down(refused)
            }
        },
        Err(_) => Outcome::Unsure { hash, refused },
    }
}

/// The launch's terminal exists: keep the key it was started with (its `makiba.key` tag) and revoke
/// every other key of the launch. No tag, no key is revoked.
fn adopt(w: &StateWriter, row: &Row, t: &Terminal) -> Outcome {
    if let Some(carried) = t.tags.get(TAG_KEY) {
        let _ = w.conn().execute(
            "UPDATE agents SET revoked_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
             WHERE spawn_id = ?1 AND key_sha256 != ?2 AND revoked_at IS NULL",
            [&row.spawn, carried],
        );
    }
    Outcome::Up
}

/// A new launch of the run of launch `from`, by `makiba up` or after a resume that was not
/// verified (Issue 41): the same kind, agent, model, brief and workplace, a new spawn id, at
/// `worktree_ready` (the workplace is used as it is), resuming `conversation` when given, and taken
/// (`run_taken`). Returns its spawn id.
pub fn add_relaunch(w: &StateWriter, from: i64, conversation: Option<&str>) -> Result<String> {
    w.conn().execute(
        "INSERT INTO run_launch (run_id, spawn_id, kind, agent, model, brief, worktree,
                                 conversation_id, stage)
         SELECT run_id, lower(hex(randomblob(16))), kind, agent, model, brief, worktree, ?2,
                'worktree_ready'
         FROM run_launch WHERE id = ?1",
        rusqlite::params![from, conversation],
    )?;
    let (spawn, task): (String, String) = w.conn().query_row(
        "SELECT l.spawn_id, r.task_id FROM run_launch l JOIN runs r ON r.id = l.run_id
         WHERE l.id = last_insert_rowid()",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    w.append_event(&NewEvent {
        kind: "run_taken",
        actor: Some("makibad"),
        payload: Some(&json!({"task_id": task, "spawn_id": spawn})),
        run_task_id: Some(&task),
        ..Default::default()
    })?;
    Ok(spawn)
}

/// `worker_spawned` and stage `launched`, together, only over `settings_ready` (Issue 163):
/// `false`, and nothing written, when the launch was moved meanwhile.
fn record(w: &StateWriter, row: &Row) -> Result<bool> {
    super::crash::hold("launched");
    w.transaction(|w| {
        let n = w.conn().execute(
            "UPDATE run_launch SET stage = 'launched' WHERE id = ?1 AND stage = 'settings_ready'",
            [row.id],
        )?;
        if n == 0 {
            return Ok(false);
        }
        w.append_event(&NewEvent {
            kind: "worker_spawned",
            actor: Some("makibad"),
            payload: Some(&json!({"task_id": row.task, "spawn_id": row.spawn})),
            run_task_id: row.task.as_deref(),
            ..Default::default()
        })?;
        Ok(true)
    })
}

impl Row {
    /// The terminal's name and the agent id (spec section 4): `w-<task_id>`, `curator`, `secretary`.
    fn name(&self) -> String {
        match &self.task {
            Some(task) => spec::worker_agent_id(task),
            None => self.role.clone(),
        }
    }
}

/// A launch's role as stored, as the door knows it and as the agent module does. `None` for a
/// value the table does not allow.
pub(super) fn roles(role: &str) -> Option<(spec::Role, Role)> {
    match role {
        "worker" => Some((spec::Role::Worker, Role::Worker)),
        "curator" => Some((spec::Role::Curator, Role::Curator)),
        "secretary" => Some((spec::Role::Secretary, Role::Secretary)),
        _ => None,
    }
}

/// The settings file of a launch (spec section 9): `<task_id>.json` for a run's, one per launch
/// (`<role>-<spawn_id>.json`) for a launch without a run, so two of one role never share it.
pub(super) fn settings_file(dir: &Path, task: Option<&str>, role: &str, spawn: &str) -> PathBuf {
    match task {
        Some(task) => dir.join(format!("{task}.json")),
        None => dir.join(format!("{role}-{spawn}.json")),
    }
}

/// A launch that belongs to no run (Issue 166), for `makibad`'s own code: the curator or the
/// secretary, started in `folder` with `brief`, by the agent module `agent`. The row is `planned`;
/// the passes take it on without a `run_taken` and outside `max_concurrent_workers`. Returns its
/// spawn id.
pub fn add_launch(
    w: &StateWriter,
    role: spec::Role,
    folder: &Path,
    brief: &str,
    model: Option<&str>,
    agent: &str,
) -> std::result::Result<String, String> {
    if !matches!(role, spec::Role::Curator | spec::Role::Secretary) {
        return Err(format!("a {} launch belongs to a run", role.as_str()));
    }
    // The terminal starts there, whatever this process's own folder is.
    let folder = std::path::absolute(folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    let spawn = super::door::random_key().map_err(|e| e.to_string())?[..32].to_string();
    w.conn()
        .execute(
            "INSERT INTO run_launch (run_id, role, spawn_id, agent, model, brief, worktree)
             VALUES (NULL, ?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                role.as_str(),
                spawn,
                agent,
                model,
                brief,
                folder.display().to_string()
            ],
        )
        .map_err(|e| e.to_string())?;
    Ok(spawn)
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
            run_id: Some(1),
            task: Some("t1".into()),
            role: "worker".into(),
            spawn: "s1".into(),
            agent: "fake".into(),
            model: None,
            brief: "b".into(),
            worktree: "/w".into(),
            project: None,
            resume: None,
            relaunch: false,
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
        assert!(matches!(l.start(&w, &stub, &row), Outcome::Several(_)));
        // `pass` fails it by resume::fail (tested in resume.rs), with the run.
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
