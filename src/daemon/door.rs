//! The door (spec section 3): one request handler and its two entrances. The person's `makiba`
//! uses a Unix socket, one JSON line in and one out. Agents call the tools of an MCP server on
//! 127.0.0.1, one tool per verb, with their token as the key. The same port also carries the
//! channel servers' held requests (`GET /channel/next`, `crate::channel::Hub`).
//!
//! The key is the only thing that says who calls. A request with no key, an empty one, or one
//! that is unknown or revoked is `denied`, whatever process sent it; nothing here takes the
//! absence of a key to mean the person. Peer credentials on the socket are used only to refuse a
//! connection from another user id, before anything is read.
//!
//! Every request with a verb is recorded in `commands` (spec section 5.2) except a `usage` error,
//! and `(actor, idem)` returns the first answer again. The requests are handled one at a time
//! (one lock around the door's own connection to `state.db`).

use crate::base::Base;
use crate::channel::Hub;
use crate::cli::LINE_MAX;
use crate::db::writer::{NewEvent, RunUpsert, StateWriter};
use crate::spec::{DoorError, ErrorKind, Role, Verb, worker_agent_id};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// The MCP server's one path.
pub const MCP_PATH: &str = "/mcp";
/// The `actor` of a request that carries no valid key.
pub const UNKNOWN_ACTOR: &str = "unknown";
/// Every blocking read and write of a connection gives up after this.
const IO_LIMIT: Duration = Duration::from_secs(5);
/// MCP protocol versions answered as asked; any other gets the first.
const PROTOCOLS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];
/// The settings `config` knows (spec section 12).
const CONFIG_KEYS: [&str; 7] = [
    "max_concurrent_workers",
    "merge_approval_mode",
    "conversation_language",
    "artifact_language",
    super::project::OWNERS_KEY,
    super::project::HOSTS_KEY,
    super::call::KEY,
];

/// What a request comes to: the response's `result`, or its `error`.
pub type Response = Result<Value, DoorError>;

pub fn sha256_hex(text: &str) -> String {
    hex_sha256(text.as_bytes())
}

fn hex_sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A new key: 32 random bytes, hex (spec section 4).
pub fn random_key() -> std::io::Result<String> {
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Make a key for an agent and store only its hash. The key goes into the agent's launch
/// arguments (M3-4), never into its environment.
pub fn issue_key(
    w: &StateWriter,
    role: Role,
    agent_id: &str,
    run_id: Option<i64>,
    spawn_id: Option<&str>,
) -> Result<String, String> {
    let key = random_key().map_err(|e| format!("cannot make a key: {e}"))?;
    w.conn()
        .execute(
            "INSERT INTO agents (agent_id, role, run_id, key_sha256, spawn_id)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![agent_id, role.as_str(), run_id, sha256_hex(&key), spawn_id],
        )
        .map_err(|e| format!("cannot store the key's hash: {e}"))?;
    Ok(key)
}

fn caller(conn: &Connection, key: &str) -> rusqlite::Result<Option<(String, Role)>> {
    if key.is_empty() {
        return Ok(None);
    }
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT agent_id, role FROM agents WHERE key_sha256 = ?1 AND revoked_at IS NULL",
            [sha256_hex(key)],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(row.and_then(|(id, role)| {
        let role = Role::ALL.iter().copied().find(|r| r.as_str() == role)?;
        Some((id, role))
    }))
}

/// The agent a token is bound to, for the channel servers' requests (`Hub::new`).
pub fn agent_of(db: &Path, token: &str) -> Option<String> {
    let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    conn.busy_timeout(IO_LIMIT).ok()?;
    caller(&conn, token).ok().flatten().map(|(id, _)| id)
}

pub(super) fn refuse(
    kind: ErrorKind,
    message: impl Into<String>,
    hint: impl Into<String>,
) -> DoorError {
    DoorError {
        kind,
        message: message.into(),
        hint: hint.into(),
    }
}

/// Open a request's write transaction (`BEGIN IMMEDIATE`) through the writer, which logs a hold
/// of the lock over a second as `label` at the caller's line (Issue 561). A SQLite error keeps
/// SQLite's own text, as the door answered before.
#[track_caller]
fn begin_as(w: &StateWriter, label: &str) -> Result<(), String> {
    w.begin_as(label).map_err(|e| match e {
        crate::db::Error::Sqlite(e) => e.to_string(),
        e => e.to_string(),
    })
}

pub(super) fn internal(e: impl std::fmt::Display) -> DoorError {
    refuse(
        ErrorKind::Internal,
        e.to_string(),
        "makibad's log has the detail",
    )
}

pub(super) fn usage(message: impl Into<String>) -> DoorError {
    refuse(ErrorKind::Usage, message, "")
}

/// The response line of spec section 3.
pub fn line(r: &Response) -> Value {
    match r {
        Ok(v) => json!({"ok": true, "result": v}),
        Err(e) => json!({"ok": false, "error": error_json(e)}),
    }
}

fn error_json(e: &DoorError) -> Value {
    json!({"kind": e.kind.as_str(), "message": e.message, "hint": e.hint})
}

fn from_line(v: &Value) -> Response {
    if v["ok"] == json!(true) {
        return Ok(v["result"].clone());
    }
    let e = &v["error"];
    let kind = ErrorKind::ALL
        .iter()
        .copied()
        .find(|k| Some(k.as_str()) == e["kind"].as_str())
        .unwrap_or(ErrorKind::Internal);
    let text = |k: &str| e[k].as_str().unwrap_or_default().to_string();
    Err(refuse(kind, text("message"), text("hint")))
}

/// The status a response is recorded with; `None`: not recorded (spec section 3, `usage`).
fn status_of(r: &Response) -> Option<&'static str> {
    match r {
        Ok(_) => Some("done"),
        Err(e) => match e.kind {
            ErrorKind::Usage => None,
            ErrorKind::Denied | ErrorKind::Conflict | ErrorKind::NotFound | ErrorKind::Stale => {
                Some("rejected")
            }
            ErrorKind::Unavailable | ErrorKind::Internal => Some("failed"),
        },
    }
}

/// A verb that has a tool for agents (spec section 12: all but `up`, `down`, `base`, `config`,
/// `gate`, `events`).
pub fn has_tool(v: Verb) -> bool {
    !matches!(
        v,
        Verb::Up | Verb::Down | Verb::Base | Verb::Config | Verb::Gate | Verb::Events
    )
}

/// The tools a role is listed: its verbs (spec section 4) that have a tool.
pub fn tools_of(role: Role) -> Vec<Verb> {
    role.verbs()
        .iter()
        .copied()
        .filter(|v| has_tool(*v))
        .collect()
}

pub(super) fn str_arg<'a>(args: &'a Value, name: &str) -> Result<&'a str, DoorError> {
    args.get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| usage(format!("the argument {name} (text) is missing")))
}

/// The door's state: its own connection to `state.db`, and the base for typed sends.
pub struct Door {
    w: Mutex<StateWriter>,
    base: Arc<dyn Base>,
    /// The agent module `dispatch` gives a launch: `MAKIBA_LAUNCH_AGENT`, read when `makibad`
    /// starts, so no request can choose it.
    agent: String,
    /// The state folder, where `state.db` is: the places a review reads (Issue 135).
    state: PathBuf,
    /// The readers' programs, read when the door is made, so no request chooses them.
    review: super::review::Programs,
    /// The tasks whose review runs now: one at a time per task.
    reviewing: Mutex<std::collections::HashSet<String>>,
}

impl Door {
    pub fn new(w: StateWriter, base: Arc<dyn Base>, agent: String) -> Door {
        let state = w
            .conn()
            .path()
            .and_then(|p| Path::new(p).parent().map(Path::to_path_buf))
            .unwrap_or_default();
        Door {
            w: Mutex::new(w),
            base,
            agent,
            state,
            review: super::review::Programs::from_env(),
            reviewing: Mutex::new(Default::default()),
        }
    }

    /// The readers of `review` (Issue 135): the other model's program and the same model's.
    pub fn with_review_programs(mut self, other: PathBuf, same: PathBuf) -> Door {
        self.review.other = other;
        self.review.same = same;
        self
    }

    /// The boundary the readers run in (Issue 262): a program taking bubblewrap's arguments.
    pub fn with_review_boundary(mut self, boundary: PathBuf) -> Door {
        self.review.boundary = boundary;
        self
    }

    fn writer(&self) -> std::sync::MutexGuard<'_, StateWriter> {
        self.w.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Who holds `key`, if anyone.
    pub fn caller(&self, key: Option<&str>) -> Option<(String, Role)> {
        let w = self.writer();
        key.and_then(|k| caller(w.conn(), k).ok().flatten())
    }

    /// One request. `tool` is true on the MCP server, where a verb without a tool is refused.
    pub fn handle(
        &self,
        key: Option<&str>,
        verb: &str,
        args: Value,
        idem: Option<&str>,
        tool: bool,
    ) -> Response {
        let w = self.writer();
        let who = match key {
            Some(k) => caller(w.conn(), k).map_err(internal)?,
            None => None,
        };
        let Some((actor, role)) = who else {
            // No replay for a caller nobody knows: its `idem` is not kept.
            let r = Err(refuse(
                ErrorKind::Denied,
                "no valid key: the request carries no key, or one this door does not know",
                "an agent's key is in its MCP configuration; the person's is the person key file",
            ));
            let _ = record(&w, UNKNOWN_ACTOR, verb, &args, None, &r);
            return r;
        };
        if let Some(idem) = idem
            && let Some(first) = replay(&w, &actor, idem).map_err(internal)?
        {
            return first;
        }
        let Some(v) = Verb::ALL.iter().copied().find(|v| v.as_str() == verb) else {
            return Err(usage(format!("no verb {verb:?}")));
        };
        if !role.verbs().contains(&v) || (tool && !has_tool(v)) {
            let r = Err(refuse(
                ErrorKind::Denied,
                format!("{} may not use {verb}", role.as_str()),
                "",
            ));
            let _ = record(&w, &actor, verb, &args, idem, &r);
            return r;
        }
        if v == Verb::Gate {
            return self.gate(w, &actor, &args, idem);
        }
        if v == Verb::Merge {
            return self.merge(w, &actor, role, &args, idem);
        }
        if v == Verb::Review && role == Role::Worker {
            return self.review(w, &actor, &args, idem);
        }
        // Issue 307: the check before CI reads GitHub and git with the lock released, as the gate.
        let mut base_merge = false;
        // A rerun records no new head (Issue 375): the head was checked when it was recorded.
        let rerun = args.get("state").and_then(Value::as_str) == Some("rerun");
        let w = match (v, gate_args(&args)) {
            (Verb::Pr, Ok((repo, pr, head))) if !rerun => {
                let read = (|| -> rusqlite::Result<_> {
                    let projects: Vec<(String, Option<String>)> = w
                        .conn()
                        .prepare(
                            "SELECT slug, origin_url FROM projects WHERE status = 'active' ORDER BY slug",
                        )?
                        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                        .collect::<rusqlite::Result<_>>()?;
                    Ok((projects, failed_heads(w.conn(), &repo, pr)?))
                })();
                let (projects, failed) = read.map_err(internal)?;
                let before = newest_watch(&w, &repo, pr).map_err(internal)?;
                // Issue 358: the head recorded last, which a pushed merge of the base is judged against.
                let prev = match args.get("state").and_then(Value::as_str) {
                    Some("pushed") => newest_head(&w, &repo, pr).map_err(internal)?,
                    _ => None,
                };
                drop(w);
                let mut found = pr_before_ci(&projects, &failed, &repo, pr, &head, prev.as_deref());
                let w = self.writer();
                // The same request may have been recorded while the lock was released.
                if let Some(idem) = idem
                    && let Some(first) = replay(&w, &actor, idem).map_err(internal)?
                {
                    return first;
                }
                // Another record of this pull request went in meanwhile: this one may be older.
                if found.is_ok() && newest_watch(&w, &repo, pr).map_err(internal)? != before {
                    found = Err(format!(
                        "{repo}#{pr} was recorded again while this record was checked; nothing was recorded, record the newest head again"
                    ));
                }
                match found {
                    Err(why) => {
                        let r = Err(refuse(
                            ErrorKind::Conflict,
                            why,
                            "push the test commit alone with push and head, record it with pr, and record a later head once CI failed there",
                        ));
                        let _ = record(&w, &actor, verb, &args, idem, &r);
                        return r;
                    }
                    Ok(merge) => base_merge = merge,
                }
                w
            }
            _ => w,
        };
        // Issue 317: the Issue a dispatch names is read from GitHub with the lock released.
        let (w, issue) = match (v, dispatch_issue(&args)) {
            (Verb::Dispatch, Some(n)) => {
                let origin = dispatch_origin(&w, &args).map_err(internal)?;
                drop(w);
                let read = read_issue(&args, origin.as_deref(), n);
                let w = self.writer();
                // The same request may have been recorded while the lock was released.
                if let Some(idem) = idem
                    && let Some(first) = replay(&w, &actor, idem).map_err(internal)?
                {
                    return first;
                }
                (w, Some(read))
            }
            _ => (w, None),
        };
        if v == Verb::Events {
            // A read that a client repeats: recorded, it would write a row and an event per
            // poll and show the client its own polls (spec section 3). Refusals are recorded above.
            return events(&w, &args);
        }
        if args.get("typed") == Some(&json!(true)) {
            // Typed sends are recorded before anything is typed, outside the verb's transaction.
            let target = match v {
                Verb::Send => str_arg(&args, "to")
                    .and_then(|to| Ok((to, str_arg(&args, "text")?)))
                    .and_then(|(to, text)| {
                        recipient(&w, role, to)?;
                        // A parked worker has no terminal: the message is stored, which wakes it.
                        if parked(&w, to)? {
                            return Ok(None);
                        }
                        Ok(Some((to.to_string(), text.to_string(), None)))
                    }),
                // An answer to a stop makibad wrote goes to the secretary, as a message: never typed.
                // Nor is one to a parked worker, which has no terminal (Issue 283).
                Verb::Answer => question(&w, &args).and_then(|q| match q {
                    Some(q) if q.to != crate::spec::AGENT_SECRETARY && !parked(&w, &q.to)? => {
                        Ok(Some((q.to.clone(), q.body.clone(), Some(q))))
                    }
                    _ => Ok(None),
                }),
                _ => Ok(None),
            };
            match target {
                Err(e) => {
                    let r = Err(e);
                    let _ = record(&w, &actor, verb, &args, idem, &r);
                    return r;
                }
                Ok(Some((to, text, q))) => {
                    // `None`: the run was parked before the send was recorded (Issue 332), so
                    // nothing was typed or stored; the verb's own path below stores it instead.
                    if let Some(r) = self.typed(
                        &w,
                        &actor,
                        verb,
                        &args,
                        idem,
                        &to,
                        &text,
                        |w, id| match &q {
                            None => Ok(json!({"id": id})),
                            Some(q) => q.answered(w, &actor, id),
                        },
                    ) {
                        return r;
                    }
                }
                // An answer to a prompt types no text: the verb's own path.
                Ok(None) => {}
            }
        }
        // The verb's writes and its record are one transaction (a verb's own transaction joins it).
        // IMMEDIATE takes the write lock first: a deferred one that reads, then writes after
        // makibad's loop has written, fails at once with `database is locked` (WAL snapshot).
        let label = format!("door {verb}");
        if let Err(e) = begin_as(&w, &label) {
            return Err(internal(e));
        }
        // A decided candidate's file, or a placed ledger, is written outside the transaction: its
        // old bytes (or its absence) come back when its record does not stand.
        let mut undo = Vec::new();
        let r = match v {
            Verb::Candidate => candidate(&w, &actor, &args, &mut undo),
            Verb::Goal => goal(&w, &actor, &args, &mut undo),
            Verb::Dispatch => dispatch(&w, &self.agent, &actor, &args, issue.as_ref()),
            _ => run(
                &w,
                &*self.base,
                &self.agent,
                &actor,
                role,
                v,
                &args,
                base_merge,
            ),
        };
        if r.is_err() && w.rollback().is_ok() {
            let _ = begin_as(&w, &label);
        }
        // A saved note is a file outside the transaction: it goes when its record does.
        let unsave = || {
            if let (Verb::Note, Ok(saved)) = (v, &r)
                && let Some(file) = saved["file"].as_str()
            {
                let _ = std::fs::remove_file(super::curator::knowledge_dir().join(file));
            }
            for (path, old) in undo.iter().rev() {
                let _ = match old {
                    Some(old) => replace_file(path, old),
                    None => std::fs::remove_file(path),
                };
            }
        };
        // A verb's writes stand only with their record: without it a retry would repeat them.
        // A transaction SQLite rolled back by itself (a full disk) is gone: nothing was done.
        if !record(&w, &actor, verb, &args, idem, &r) || w.conn().is_autocommit() {
            let _ = w.rollback();
            unsave();
            return Err(internal(
                "the request could not be recorded; nothing was done",
            ));
        }
        if let Err(e) = w.commit() {
            let _ = w.rollback();
            unsave();
            return Err(internal(e));
        }
        r
    }

    /// `send` with `typed` (spec section 7). The command is recorded before anything is typed, as
    /// not finished, so a retry of the same `idem` after a crash answers that the outcome is not
    /// known instead of typing again. `send_typed` records the message before it types.
    /// `after` runs once the text was typed, with the message id, and gives the result (an
    /// answer marks its entry answered there). `None` when the recipient's run was parked before
    /// the message was recorded: nothing was typed or stored, and the command is not recorded.
    #[allow(clippy::too_many_arguments)]
    fn typed(
        &self,
        w: &StateWriter,
        actor: &str,
        verb: &str,
        args: &Value,
        idem: Option<&str>,
        to: &str,
        text: &str,
        after: impl FnOnce(&StateWriter, i64) -> Response,
    ) -> Option<Response> {
        let unknown: Response = Err(internal(
            "the outcome of an earlier typed send with this idem is not known; it is not sent again",
        ));
        let Some(id) = insert_command(w, actor, verb, args, idem, "accepted", &unknown) else {
            return Some(Err(internal("cannot record the request")));
        };
        let r = super::delivery::send_typed(w, &*self.base, actor, to, "message", text);
        if matches!(r, Err(super::delivery::TypedError::Parked)) {
            if let Err(e) = w.conn().execute("DELETE FROM commands WHERE id = ?1", [id]) {
                eprintln!("makibad: cannot remove command {id}: {e}");
                return Some(Err(internal(e)));
            }
            return None;
        }
        let r = r
            .map_err(|e| match e {
                super::delivery::TypedError::Usage(m) => usage(m),
                super::delivery::TypedError::Conflict(m) => refuse(
                    ErrorKind::Conflict,
                    m,
                    "nothing was typed or stored; send again later or without typed",
                ),
                super::delivery::TypedError::Failed(m) => internal(m),
                super::delivery::TypedError::Parked => {
                    internal("the run was parked; handled above")
                }
            })
            .and_then(|id| after(w, id));
        let done = match status_of(&r) {
            None => w
                .conn()
                .execute("DELETE FROM commands WHERE id = ?1", [id])
                .map(|_| ()),
            Some(status) => w
                .conn()
                .execute(
                    "UPDATE commands SET status = ?1, result_json = ?2, error_kind = ?3,
                       finished_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?4",
                    params![
                        status,
                        line(&r).to_string(),
                        r.as_ref().err().map(|e| e.kind.as_str()),
                        id
                    ],
                )
                .map(|_| ()),
        };
        if let Err(e) = done {
            eprintln!("makibad: cannot finish command {id}: {e}");
        }
        Some(r)
    }
}

fn replay(w: &StateWriter, actor: &str, idem: &str) -> rusqlite::Result<Option<Response>> {
    let first: Option<Option<String>> = w
        .conn()
        .query_row(
            "SELECT result_json FROM commands WHERE actor = ?1 AND idem = ?2",
            params![actor, idem],
            |r| r.get(0),
        )
        .optional()?;
    Ok(first.map(|j| {
        j.and_then(|j| serde_json::from_str::<Value>(&j).ok())
            .map_or_else(
                || Err(internal("the first answer was not kept")),
                |v| from_line(&v),
            )
    }))
}

/// One `commands` row and its `command_received` event. Returns the row's id.
fn insert_command(
    w: &StateWriter,
    actor: &str,
    verb: &str,
    args: &Value,
    idem: Option<&str>,
    status: &str,
    r: &Response,
) -> Option<i64> {
    let run = || -> crate::db::Result<i64> {
        let finished = status != "accepted";
        w.conn().execute(
            "INSERT INTO commands (actor, verb, args_json, idem, status, result_json, error_kind, finished_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7,
                     CASE WHEN ?8 THEN strftime('%Y-%m-%dT%H:%M:%fZ','now') END)",
            params![
                actor,
                verb,
                args.to_string(),
                idem,
                status,
                line(r).to_string(),
                r.as_ref().err().map(|e| e.kind.as_str()),
                finished
            ],
        )?;
        let id = w.conn().last_insert_rowid();
        let payload = json!({"command_id": id});
        w.append_event(&NewEvent {
            kind: "command_received",
            actor: Some(actor),
            payload: Some(&payload),
            ..Default::default()
        })?;
        Ok(id)
    };
    run()
        .map_err(|e| eprintln!("makibad: cannot record a {verb} request of {actor}: {e}"))
        .ok()
}

/// Finish the `dispatch` command of `task` while it is `accepted` (spec section 5.2, Issue 145):
/// `done`, or `failed` with `error_kind` and the reason as the `error` beside its accepted line,
/// which stays, so a retry of the same idem still answers it. Called inside the transaction that
/// settles the run's launch.
pub(super) fn finish_dispatch(
    w: &StateWriter,
    task: &str,
    failed: Option<(ErrorKind, &str)>,
) -> rusqlite::Result<()> {
    let (status, kind, error) = match failed {
        None => ("done", None, None),
        Some((k, m)) => (
            "failed",
            Some(k.as_str()),
            Some(json!({"kind": k.as_str(), "message": m, "hint": ""}).to_string()),
        ),
    };
    w.conn().execute(
        "UPDATE commands SET status = ?2, error_kind = ?3,
           result_json = CASE WHEN ?4 IS NULL THEN result_json
                              ELSE json_set(COALESCE(result_json, '{}'), '$.error', json(?4)) END,
           finished_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE verb = 'dispatch' AND status = 'accepted'
           AND json_extract(args_json, '$.task_id') = ?1",
        params![task, status, kind, error],
    )?;
    Ok(())
}

fn record(
    w: &StateWriter,
    actor: &str,
    verb: &str,
    args: &Value,
    idem: Option<&str>,
    r: &Response,
) -> bool {
    // `dispatch`, `close`, `push` and `update` return at `accepted` and finish later (spec section 5.2); a
    // close of a run closed already is `done` again.
    let status = match (verb, r) {
        ("dispatch" | "project", Ok(_)) => Some("accepted"),
        ("close", Ok(v)) if v.get("closed").is_none() => Some("accepted"),
        // `makibad`'s secretary pass carries a renewal out (Issue 46).
        ("renew", Ok(_)) => Some("accepted"),
        ("push" | "update", Ok(_)) => Some("accepted"),
        _ => status_of(r),
    };
    match status {
        Some(status) => insert_command(w, actor, verb, args, idem, status, r).is_some(),
        None => true,
    }
}

/// `base_merge`: what the door judged outside its lock for `pr pushed` (Issue 358), never an
/// argument of the request.
#[allow(clippy::too_many_arguments)]
fn run(
    w: &StateWriter,
    base: &dyn Base,
    agent: &str,
    actor: &str,
    role: Role,
    v: Verb,
    args: &Value,
    base_merge: bool,
) -> Response {
    let r = match v {
        // Its Issue is read before the transaction (Issue 317): `handle` calls `dispatch`.
        Verb::Dispatch => Err(internal("dispatch is handled by the door's handle")),
        Verb::Status => status(w),
        Verb::Peek => peek(w, base, args),
        Verb::Pending => pending(w, args),
        Verb::Answer => answer(w, base, actor, args),
        Verb::Renew => renew(w, args),
        Verb::Handover => handover(w, args),
        Verb::Send => {
            let to = str_arg(args, "to")?;
            let text = str_arg(args, "text")?;
            recipient(w, role, to)?;
            let id = store_message(w, actor, to, "message", None, text)?;
            Ok(json!({"id": id}))
        }
        Verb::Report => report(w, actor, role, args),
        Verb::Close => close(w, actor, role, args),
        Verb::Show => show(w, actor, args),
        Verb::Inbox => {
            let b = super::inbox::inbox(w, actor).map_err(internal)?;
            let messages: Vec<Value> = b
                .messages
                .iter()
                .map(|m| json!({"id": m.id, "text": m.text}))
                .collect();
            Ok(json!({"messages": messages, "more": b.more}))
        }
        Verb::Config => config(w, actor, args),
        Verb::Pr => pr(w, args, base_merge),
        Verb::Note => note(w, actor, args),
        Verb::Push => push(w, args),
        Verb::Project => {
            let slug = str_arg(args, "slug")?;
            plain_name("slug", slug)?;
            let hosts = args.get("hosts").and_then(Value::as_str);
            super::project::accept(w, role, actor, slug, str_arg(args, "repo")?, hosts)
        }
        Verb::Update => update(w, args),
        Verb::Range => super::belt::range(w, actor, role, args),
        Verb::Review => review_allowance(w, actor, args),
        other => Err(internal(format!("{} is not built yet", other.as_str()))),
    }?;
    // A done report, a close, an escalation and a stop written for a recorded head free a slot:
    // the belt fills it in the same transaction (belt.md 3.4, 4.1). Any other answer of these
    // verbs fills nothing new.
    if matches!(v, Verb::Report | Verb::Close | Verb::Pending | Verb::Pr) {
        super::belt::fill(w, agent).map_err(internal)?;
    }
    Ok(r)
}

/// `renew` (spec section 12, Issue 46): the handover is stored in the command's own row, which
/// `makibad`'s secretary pass takes up. One renewal at a time.
fn renew(w: &StateWriter, args: &Value) -> Response {
    handover_checks(w, args)?;
    Ok(json!({"renewal": "accepted"}))
}

/// `handover` (spec section 12, Issue 502): the handover is stored in the command's own row,
/// `done`, and the secretary is not replaced; the next decision of `makiba up` resumes from it
/// (spec section 5.3).
fn handover(w: &StateWriter, args: &Value) -> Response {
    handover_checks(w, args)?;
    Ok(json!({"handover": "stored"}))
}

/// What `renew` and `handover` refuse: a handover that is missing, empty or over `HANDOVER_MAX`
/// (`usage`), and any while a renewal is under way (`conflict`): that renewal's launch carries its
/// own handover.
fn handover_checks(w: &StateWriter, args: &Value) -> Result<(), DoorError> {
    let handover = str_arg(args, "handover")?;
    if handover.trim().is_empty() {
        return Err(usage("the handover is empty"));
    }
    if handover.len() > super::secretary::HANDOVER_MAX {
        return Err(usage(format!(
            "the handover is over {} bytes",
            super::secretary::HANDOVER_MAX
        )));
    }
    let under_way: bool = w
        .conn()
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM commands WHERE verb = 'renew' AND status = 'accepted')",
            [],
            |r| r.get(0),
        )
        .map_err(internal)?;
    if under_way {
        return Err(refuse(
            ErrorKind::Conflict,
            "a renewal of the secretary is under way",
            "wait for it to finish (makiba status)",
        ));
    }
    Ok(())
}

/// `status` (spec section 12): what the person's local `makiba status --json` prints, read from
/// the door's own database file. Read in `makibad`, its `merge_approval_mode` is the gate's.
fn status(w: &StateWriter) -> Response {
    crate::status_local::read_as(&db_file(w)?, crate::status_local::Reader::Makibad)
        .map_err(internal)
}

/// `events` (Issue 168): the person's `makiba events --json`, read from the door's own file.
fn events(w: &StateWriter, args: &Value) -> Response {
    let after = args
        .get("after")
        .and_then(Value::as_i64)
        .ok_or_else(|| usage("the argument after (an event id) is missing"))?;
    let limit = match args.get("limit") {
        None | Some(Value::Null) => None,
        Some(n) => Some(
            n.as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .ok_or_else(|| usage("limit is a count"))?,
        ),
    };
    crate::status_local::events(&db_file(w)?, after, limit, None).map_err(internal)
}

/// The path of the door's own database file.
fn db_file(w: &StateWriter) -> Result<std::path::PathBuf, DoorError> {
    let file: String = w
        .conn()
        .query_row(
            "SELECT file FROM pragma_database_list WHERE name = 'main'",
            [],
            |r| r.get(0),
        )
        .map_err(internal)?;
    Ok(file.into())
}

/// How many lines `peek` returns when not told, and at most.
const PEEK_LINES: u64 = 40;
const PEEK_MAX: u64 = 1000;

/// The one live terminal of `task`'s worker, or `not_found`.
fn worker_terminal(base: &dyn Base, task: &str) -> Result<crate::base::Terminal, DoorError> {
    super::delivery::find_terminal(base, &worker_agent_id(task))
        .map_err(|e| refuse(ErrorKind::Unavailable, format!("the base: {e}"), ""))?
        .ok_or_else(|| {
            refuse(
                ErrorKind::NotFound,
                format!("task {task:?} has no single live worker terminal"),
                "",
            )
        })
}

/// `peek` (spec section 12): the last lines of a worker's screen, read through the base.
fn peek(w: &StateWriter, base: &dyn Base, args: &Value) -> Response {
    let task = str_arg(args, "task_id")?;
    let lines = match args.get("lines") {
        None | Some(Value::Null) => PEEK_LINES,
        Some(n) => n
            .as_u64()
            .filter(|n| (1..=PEEK_MAX).contains(n))
            .ok_or_else(|| usage(format!("lines is a number from 1 to {PEEK_MAX}")))?,
    };
    if !run_exists(w, task)? {
        return Err(refuse(ErrorKind::NotFound, format!("no task {task:?}"), ""));
    }
    let t = worker_terminal(base, task)?;
    let screen = base
        .read(&t.id, lines as usize)
        .map_err(|e| refuse(ErrorKind::Unavailable, format!("the base: {e}"), ""))?;
    Ok(json!({"task_id": task, "lines": screen}))
}

fn id_arg(args: &Value) -> Result<i64, DoorError> {
    args.get("id")
        .and_then(Value::as_i64)
        .ok_or_else(|| usage("the argument id (a pending id) is missing"))
}

/// `pending` (spec section 12): the table of questions kept by hand, the logic of claude-org-ja's
/// `pending_decisions.py` on the `pending` table.
fn pending(w: &StateWriter, args: &Value) -> Response {
    let c = w.conn();
    match str_arg(args, "action")? {
        "add" => {
            let task = str_arg(args, "task")?;
            let q = str_arg(args, "question")?;
            if q.trim().is_empty() {
                return Err(usage("the question is not empty"));
            }
            // Issue 255's context parts, and the belt's stop fields beside them (Issue 257).
            let mut context = context_arg(args)?;
            if let Value::Object(m) = &mut context {
                m.extend(super::belt::stop_fields(args)?);
            }
            if !run_exists(w, task)? {
                return Err(refuse(ErrorKind::NotFound, format!("no task {task:?}"), ""));
            }
            // The same question of the same task, still open, is that entry (pending_decisions.py
            // `append`); once escalated, withdrawn or answered, it opens a new one. An entry made
            // from a screen is never it. The parts given are written into its context (Issue 255).
            let open: Option<i64> = c
                .query_row(
                    "SELECT p.id FROM pending p JOIN runs r ON r.id = p.run_id
                     WHERE r.task_id = ?1 AND p.kind = 'question' AND p.question = ?2
                       AND p.status = 'open' AND p.prompt_text IS NULL ORDER BY p.id LIMIT 1",
                    params![task, q],
                    |r| r.get(0),
                )
                .optional()
                .map_err(internal)?;
            if let Some(id) = open {
                c.execute(
                    "UPDATE pending SET context_json = json_patch(context_json, ?1) WHERE id = ?2",
                    params![context.to_string(), id],
                )
                .map_err(internal)?;
                return Ok(json!({"id": id, "status": "open"}));
            }
            c.execute(
                "INSERT INTO pending (run_id, kind, question, context_json)
                 VALUES ((SELECT id FROM runs WHERE task_id = ?1), 'question', ?2, ?3)",
                params![task, q, context.to_string()],
            )
            .map_err(internal)?;
            Ok(json!({"id": c.last_insert_rowid(), "status": "open"}))
        }
        action @ ("escalate" | "withdraw") => {
            let id = id_arg(args)?;
            // Issue 432: a pending merge stays open for the person's key (an escalated one could
            // not be approved) and is withdrawn by merge withdraw, with its notice.
            let merge: bool = c
                .query_row(
                    "SELECT EXISTS (SELECT 1 FROM pending WHERE id = ?1 AND kind = 'merge')",
                    [id],
                    |r| r.get(0),
                )
                .map_err(internal)?;
            if merge {
                return Err(refuse(
                    ErrorKind::Denied,
                    format!("pending entry {id} is a merge, which pending does not {action}"),
                    "withdraw it with merge, action withdraw and its id; the person approves or refuses it with makiba merge",
                ));
            }
            if action == "escalate" {
                missing_context(c, id)?;
                // A range task's stop is recorded with its kind and the recommended answer
                // (belt.md 5.2): whether the person takes the recommendation is counted from them.
                let missing = super::belt::stop_parts_missing(c, id).map_err(internal)?;
                if !missing.is_empty() {
                    return Err(refuse(
                        ErrorKind::Usage,
                        format!(
                            "pending entry {id} is a stop of a range task and cannot be escalated without {}",
                            missing.join(" and ")
                        ),
                        "add them with pending add (the same task and question): stop_kind is design, scope, order, naming or other; recommend_answer is yes, no or none",
                    ));
                }
            }
            let (to, from) = if action == "escalate" {
                ("escalated", "('open')")
            } else {
                ("withdrawn", "('open','escalated')")
            };
            let changed = c
                .execute(
                    &format!("UPDATE pending SET status = ?1 WHERE id = ?2 AND status IN {from}"),
                    params![to, id],
                )
                .map_err(internal)?;
            if changed == 1 {
                if action == "escalate" {
                    super::belt::stopped(w, "makibad", id).map_err(internal)?;
                } else {
                    // A parked run whose last question is withdrawn waits for nobody (Issue 283).
                    super::park::settle(w).map_err(internal)?;
                }
                return Ok(json!({"id": id, "status": to}));
            }
            let now: Option<String> = c
                .query_row("SELECT status FROM pending WHERE id = ?1", [id], |r| {
                    r.get(0)
                })
                .optional()
                .map_err(internal)?;
            Err(match now {
                None => refuse(ErrorKind::NotFound, format!("no pending entry {id}"), ""),
                Some(st) => refuse(
                    ErrorKind::Conflict,
                    format!("pending entry {id} is {st}; it cannot be {to}"),
                    "",
                ),
            })
        }
        "list" => {
            let mut q = c
                .prepare(
                    "SELECT p.id, r.task_id, p.kind, p.question, p.status FROM pending p
                     LEFT JOIN runs r ON r.id = p.run_id
                     WHERE p.status IN ('open','escalated') ORDER BY p.id",
                )
                .map_err(internal)?;
            let rows = q
                .query_map([], |r| {
                    Ok(
                        json!({"id": r.get::<_, i64>(0)?, "task_id": r.get::<_, Option<String>>(1)?,
                              "kind": r.get::<_, String>(2)?, "question": r.get::<_, String>(3)?,
                              "status": r.get::<_, String>(4)?}),
                    )
                })
                .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
                .map_err(internal)?;
            Ok(json!({"entries": rows}))
        }
        other => Err(usage(format!(
            "action is add, escalate, withdraw or list, not {other:?}"
        ))),
    }
}

/// The parts of a pending entry's context (Issue 255), each required to escalate an entry made
/// by `add`.
pub const CONTEXT_PARTS: [&str; 5] = ["background", "if_yes", "if_no", "recommend", "why"];

/// The context parts `add` was given, not blank, as one object.
fn context_arg(args: &Value) -> Result<Value, DoorError> {
    let mut m = Map::new();
    for part in CONTEXT_PARTS {
        match args.get(part) {
            None | Some(Value::Null) => {}
            Some(Value::String(t)) if t.trim().is_empty() => {}
            Some(Value::String(t)) => {
                m.insert(part.into(), json!(t));
            }
            Some(_) => return Err(usage(format!("the argument {part} is text"))),
        }
    }
    Ok(Value::Object(m))
}

/// Refuses to escalate an entry made by `add` whose context lacks a part, naming those parts.
/// An entry made from a screen (`prompt_text`) is not checked: its screen is its context.
fn missing_context(c: &Connection, id: i64) -> Result<(), DoorError> {
    let row: Option<(bool, String)> = c
        .query_row(
            "SELECT prompt_text IS NOT NULL, context_json FROM pending WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(internal)?;
    let Some((false, context)) = row else {
        return Ok(());
    };
    let context: Value = serde_json::from_str(&context).unwrap_or_default();
    let missing: Vec<&str> = CONTEXT_PARTS
        .into_iter()
        .filter(|p| context[*p].as_str().is_none_or(|t| t.trim().is_empty()))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    Err(refuse(
        ErrorKind::Usage,
        format!(
            "pending entry {id} cannot be escalated without its context: {} is missing",
            missing.join(", ")
        ),
        "add them with pending add (the same task and question)",
    ))
}

/// A question an `answer` names, ready to be sent to its worker.
struct Question {
    id: i64,
    answer: String,
    to: String,
    task: Option<String>,
    body: String,
}

impl Question {
    /// Mark the entry answered, after its message `message` was stored (or typed).
    fn answered(&self, w: &StateWriter, actor: &str, message: i64) -> Response {
        let changed = w
            .conn()
            .execute(
                "UPDATE pending SET status = 'answered', answer = ?1, answered_by = ?2,
                   answered_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
                 WHERE id = ?3 AND status IN ('open','escalated')",
                params![self.answer, actor, self.id],
            )
            .map_err(internal)?;
        if changed != 1 {
            return Err(internal(format!(
                "pending entry {} changed while it was answered",
                self.id
            )));
        }
        // A `no` to a failed dependency's question releases a design-only wait (Issue 284).
        super::ahead::answered(w, self.id, &self.answer).map_err(internal)?;
        // A stop answered to the secretary may have been a parked run's last (Issue 283).
        super::park::settle(w).map_err(internal)?;
        Ok(json!({"id": self.id, "status": "answered", "message": message}))
    }
}

/// The entry an `answer` names: `Some` for an open question, `None` for a prompt (answered by
/// `answer` itself); refused when there is none or it is closed.
fn question(w: &StateWriter, args: &Value) -> Result<Option<Question>, DoorError> {
    let id = id_arg(args)?;
    let answer = str_arg(args, "answer")?;
    if !["yes", "no"].contains(&answer) {
        return Err(usage(format!("answer is yes or no, not {answer:?}")));
    }
    let note = args
        .get("note")
        .and_then(Value::as_str)
        .filter(|n| !n.is_empty());
    let row: Option<(String, String, Option<String>, String, bool)> = w
        .conn()
        .query_row(
            "SELECT p.kind, p.status, r.task_id, p.question,
                    coalesce(json_extract(p.context_json, '$.by') = 'makibad'
                             AND json_extract(p.context_json, '$.range_id') IS NOT NULL, 0)
             FROM pending p LEFT JOIN runs r ON r.id = p.run_id WHERE p.id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()
        .map_err(internal)?;
    let Some((kind, status, task, question, belt_stop)) = row else {
        return Err(refuse(
            ErrorKind::NotFound,
            format!("no pending entry {id}"),
            "",
        ));
    };
    // Issue 432: a pending merge lands only on the person's key, never on an answer.
    if kind == "merge" {
        return Err(refuse(
            ErrorKind::Denied,
            format!(
                "pending entry {id} is a merge: approve it with makiba merge approve {id} (the person's key)"
            ),
            "tell the person the entry's id; they approve or refuse it in their shell",
        ));
    }
    if !["open", "escalated"].contains(&status.as_str()) {
        return Err(refuse(
            ErrorKind::Conflict,
            format!("pending entry {id} is {status} already"),
            "",
        ));
    }
    // The question goes with the answer: two answers of one task are told apart.
    let mut body = format!("Answer to pending entry {id} ({question}): {answer}");
    if let Some(note) = note {
        body.push_str(&format!("\nNote: {note}"));
    }
    // A stop makibad wrote itself (a gate refusal, a budget; belt.md section 5) is answered to
    // the secretary, who acts on it; it may be a stop of a whole range, with no task.
    if belt_stop && kind == "question" {
        return Ok(Some(Question {
            id,
            answer: answer.to_string(),
            to: crate::spec::AGENT_SECRETARY.to_string(),
            task,
            body,
        }));
    }
    let Some(task) = task else {
        return Err(refuse(
            ErrorKind::Conflict,
            format!("pending entry {id} belongs to no task, so nobody can be answered"),
            "",
        ));
    };
    if kind == "prompt" {
        return Ok(None);
    }
    Ok(Some(Question {
        id,
        answer: answer.to_string(),
        to: worker_agent_id(&task),
        task: Some(task),
        body,
    }))
}

/// `answer` (spec sections 7 and 12). A question: its answer is queued to the worker and the
/// entry is answered, in the caller's transaction. A prompt (M6-8): exactly one key, and only
/// while the same episode's prompt is on a fresh read of the screen; any difference is `stale`
/// and nothing is pressed.
fn answer(w: &StateWriter, base: &dyn Base, actor: &str, args: &Value) -> Response {
    if let Some(q) = question(w, args)? {
        // A parked worker is woken by its answer, named as such (Issue 283).
        if q.to != crate::spec::AGENT_SECRETARY
            && let Some(task) = &q.task
        {
            super::park::unpark(w, task, "answer", json!(q.id)).map_err(internal)?;
        }
        let id = store_message(w, actor, &q.to, "message", q.task.as_deref(), &q.body)?;
        return q.answered(w, actor, id);
    }
    let id = id_arg(args)?;
    let answer = str_arg(args, "answer")?;
    let (task, sum, created): (String, Option<String>, String) = w
        .conn()
        .query_row(
            "SELECT r.task_id, p.prompt_sum, p.created_at FROM pending p
             JOIN runs r ON r.id = p.run_id WHERE p.id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(internal)?;
    let stale = |why: String| {
        refuse(
            ErrorKind::Stale,
            format!("pending entry {id}: {why}; nothing was typed"),
            "list the pending entries again (an entry whose prompt changed can be withdrawn; \
             makibad then records the prompt shown now)",
        )
    };
    let base_err =
        |e: crate::base::BaseError| refuse(ErrorKind::Unavailable, format!("the base: {e}"), "");
    let Some(sum) = sum else {
        return Err(stale("it holds no recognized prompt".into()));
    };
    // The same episode: the launch is the one recorded, and no status came since the entry.
    let scene = super::prompts::scene(w, &task).map_err(internal)?;
    let Some(scene) = scene.filter(|s| s.blocked_since.as_ref().is_some_and(|at| *at <= created))
    else {
        return Err(stale(format!("{task} is no longer blocked on that prompt")));
    };
    let t = match super::delivery::find_terminal(base, &worker_agent_id(&task)) {
        Ok(Some(t)) if super::prompts::blocked_terminal(&t, &scene.spawn) => t,
        Ok(_) => {
            return Err(stale(format!(
                "{task}'s terminal is not blocked on that prompt"
            )));
        }
        Err(e) => return Err(base_err(e)),
    };
    let screen = base.read(&t.id, 0).map_err(base_err)?;
    let agent = crate::agents::by_name(&scene.agent)
        .ok_or_else(|| internal(format!("no agent module {:?}", scene.agent)))?;
    let Some(view) = agent
        .recognize_prompt(&screen)
        .filter(|v| sha256_hex(&v.text) == sum)
    else {
        return Err(stale(format!(
            "the prompt on {task}'s screen is not the one recorded"
        )));
    };
    let key = if answer == "yes" {
        view.yes_key
    } else {
        view.no_key
    };
    base.keys(&t.id, &[key]).map_err(base_err)?;
    let changed = w
        .conn()
        .execute(
            "UPDATE pending SET status = 'answered', answer = ?1, answered_by = ?2,
               answered_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
             WHERE id = ?3 AND status IN ('open','escalated')",
            params![answer, actor, id],
        )
        .map_err(internal)?;
    if changed != 1 {
        return Err(internal(format!(
            "pending entry {id} changed while it was answered"
        )));
    }
    let payload = json!({"pending_id": id, "prompt_sum": sum, "key": key});
    w.append_event(&NewEvent {
        kind: "prompt_answered",
        actor: Some(actor),
        payload: Some(&payload),
        run_task_id: Some(&task),
        ..Default::default()
    })
    .map_err(internal)?;
    Ok(json!({"id": id, "status": "answered", "key": key}))
}

/// A name that becomes a path component and part of an agent id: `[A-Za-z0-9._-]`, not starting
/// with `.` or `-`.
fn plain_name(what: &str, s: &str) -> Result<(), DoorError> {
    let ok = !s.is_empty()
        && !s.starts_with(['.', '-'])
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if ok {
        Ok(())
    } else {
        Err(usage(format!(
            "{what} is letters, digits, '.', '_' and '-', and does not start with '.' or '-': {s:?}"
        )))
    }
}

/// The project of a `document` dispatch given none (spec section 12 requires one for `code` only).
const DOCUMENTS_PROJECT: &str = "documents";

/// How long a dispatch waits for the Issue it names (Issue 317): under the 30 s `makiba dispatch`
/// waits for the door's answer.
const ISSUE_READ_TIMEOUT: Duration = Duration::from_secs(20);

/// The Issue's text as the brief carries it, or why it could not be read (Issue 317).
type IssueRead = Result<String, String>;

/// The Issue a dispatch names, when it names a valid one.
/// Only for a dispatch whose kind and project are valid: the project becomes a path in the read
/// (`triage::repo_of`), so nothing is read for one that `dispatch` refuses. A code dispatch has a
/// project of its own: `documents` is a document's only (Issue 355).
fn dispatch_issue(args: &Value) -> Option<i64> {
    let kind = args.get("kind").and_then(Value::as_str);
    let named = args
        .get("project")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty());
    if !matches!(kind, Some("code" | "document"))
        || (kind == Some("code") && !named)
        || plain_name("project", dispatch_project(args)).is_err()
    {
        return None;
    }
    args.get("issue").and_then(Value::as_i64).filter(|n| *n > 0)
}

/// The GitHub URL of the dispatch's project, if it has one recorded.
fn dispatch_origin(w: &StateWriter, args: &Value) -> rusqlite::Result<Option<String>> {
    let slug = dispatch_project(args);
    Ok(w.conn()
        .query_row(
            "SELECT origin_url FROM projects WHERE slug = ?1",
            [slug],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten())
}

fn dispatch_project(args: &Value) -> &str {
    args.get("project")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or(DOCUMENTS_PROJECT)
}

/// Read Issue `n` of the dispatch's project with `gh issue view`, outside the door's lock: its
/// title, body and comments, in the form a range's brief has (`belt::issue_text`).
fn read_issue(args: &Value, origin: Option<&str>, n: i64) -> IssueRead {
    // One deadline for the whole read: finding the repository, then `gh`.
    let started = Instant::now();
    let slug = dispatch_project(args);
    let repos = super::prepare::Places::from_env(Path::new("")).repos;
    let repo = super::triage::repo_of_within(slug, origin, &repos, ISSUE_READ_TIMEOUT).ok_or_else(
        || format!("project {slug} has no GitHub repository, or its clone did not say in time"),
    )?;
    let left = ISSUE_READ_TIMEOUT.saturating_sub(started.elapsed());
    if left.is_zero() {
        return Err("gh timed out".into());
    }
    let program = super::gh::program();
    let never = std::sync::atomic::AtomicBool::new(false);
    let text = super::gh::run_gh_for(
        &program,
        &[
            "issue",
            "view",
            &n.to_string(),
            "--repo",
            &format!("github.com/{repo}"),
            "--json",
            "title,body,comments",
        ],
        &never,
        left,
    )
    .map_err(|e| match e {
        super::gh::GhError::Missing => format!("{program} not found"),
        super::gh::GhError::Cancelled => "cancelled".into(),
        super::gh::GhError::Failed(f) => f.message,
    })?;
    let v: Value =
        serde_json::from_str(&text).map_err(|_| "gh issue view wrote no JSON".to_string())?;
    let comments: Vec<Value> = v["comments"]
        .as_array()
        .map(|cs| {
            cs.iter()
                .map(|c| {
                    json!({"author": c["author"]["login"].as_str().unwrap_or("-"),
                           "at": c["createdAt"], "body": c["body"]})
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(super::belt::issue_text(
        n,
        v["title"].as_str().unwrap_or(""),
        v["body"].as_str().unwrap_or(""),
        &comments,
    ))
}

/// `dispatch` (spec section 12): a `queued` run, its first `run_launch` row and `delegate_sent`.
/// The caller's transaction holds them together with the command's record. `issue` is the read of
/// the Issue it names, made before the transaction (Issue 317): its text follows the brief.
fn dispatch(
    w: &StateWriter,
    agent: &str,
    actor: &str,
    args: &Value,
    issue_read: Option<&IssueRead>,
) -> Response {
    let kind = str_arg(args, "kind")?;
    if !["code", "document"].contains(&kind) {
        return Err(usage(format!("kind is code or document, not {kind:?}")));
    }
    let task = str_arg(args, "task_id")?;
    plain_name("task_id", task)?;
    let title = str_arg(args, "title")?;
    let brief = str_arg(args, "brief")?;
    if title.trim().is_empty() || brief.trim().is_empty() {
        return Err(usage("the title and the brief are not empty"));
    }
    let opt = |name: &str| {
        args.get(name)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
    };
    let project = match (opt("project"), kind) {
        (Some(p), _) => p,
        (None, "code") => return Err(usage("--project is required for kind code")),
        (None, _) => DOCUMENTS_PROJECT,
    };
    plain_name("project", project)?;
    if kind == "code" && super::project::under_way(w, project).map_err(internal)? {
        return Err(refuse(
            ErrorKind::Conflict,
            format!("the repository of project {project} is being fetched"),
            "dispatch once the notice says the project tool is done",
        ));
    }
    if run_exists(w, task)? {
        return Err(refuse(
            ErrorKind::Conflict,
            format!("task {task:?} was dispatched already"),
            "a task id is used once; dispatch under another id",
        ));
    }
    let issue = match args.get("issue") {
        None | Some(Value::Null) => None,
        Some(n) => Some(
            n.as_i64()
                .filter(|n| *n > 0)
                .ok_or_else(|| usage("issue is an Issue number"))?,
        ),
    };
    // The brief the worker starts from, and what the answer says of the Issue's text.
    let (brief, issue_text) = match (issue, issue_read) {
        (Some(_), Some(Ok(text))) => (format!("{}{text}", brief.trim_end()), Some("read".into())),
        (Some(n), read) => {
            let why = match read {
                Some(Err(why)) => why.as_str(),
                _ => "it was not read",
            };
            (
                format!(
                    "{}\n\nIssue #{n}: makibad could not read its text at dispatch ({why}). Ask the secretary for it.\n",
                    brief.trim_end()
                ),
                Some(format!("unreadable: {why}")),
            )
        }
        (None, _) => (brief.to_string(), None),
    };
    // Issue 337: a task whose test commit waits for the person's approval of its design.
    let design_first = match args.get("design_first") {
        None | Some(Value::Null) => false,
        Some(v) => v
            .as_bool()
            .ok_or_else(|| usage("design_first is true or false"))?,
    };
    let refs = issue.map(|n| super::belt::issue_ref(project, n));
    let refs: Vec<&str> = refs.iter().map(String::as_str).collect();
    let spawn = random_key().map_err(internal)?[..32].to_string();
    let worktree = super::prepare::Places::from_env(Path::new(""))
        .workplace(kind, project, task)
        .display()
        .to_string();
    let store = || -> crate::db::Result<()> {
        w.upsert_run(&RunUpsert {
            task_id: task,
            project_slug: project,
            title: Some(title),
            status: Some("queued"),
            branch: opt("branch"),
            issue_refs: (!refs.is_empty()).then_some(&refs[..]),
            ..Default::default()
        })?;
        w.conn().execute(
            "INSERT INTO run_launch (run_id, spawn_id, kind, agent, model, brief, worktree)
             VALUES ((SELECT id FROM runs WHERE task_id = ?1), ?2, ?3, ?4, ?5, ?6, ?7)",
            params![task, spawn, kind, agent, opt("model"), brief, worktree],
        )?;
        let mut sent = json!({"task_id": task, "spawn_id": spawn, "kind": kind});
        if design_first {
            sent["design_first"] = json!(true);
        }
        w.append_event(&NewEvent {
            kind: "delegate_sent",
            actor: Some(actor),
            payload: Some(&sent),
            run_task_id: Some(task),
            ..Default::default()
        })?;
        Ok(())
    };
    store().map_err(internal)?;
    let mut answer = json!({"task_id": task, "spawn_id": spawn, "status": "queued"});
    if let Some(t) = issue_text {
        answer["issue_text"] = json!(t);
    }
    Ok(answer)
}

/// May `role` send to `to`, and is there such a recipient?
fn recipient(w: &StateWriter, role: Role, to: &str) -> Result<(), DoorError> {
    if let Some(only) = role.send_only_to()
        && !only.contains(&to)
    {
        return Err(refuse(
            ErrorKind::Denied,
            format!("a {} sends to {} only", role.as_str(), only.join(", ")),
            "",
        ));
    }
    let known = match to.strip_prefix(crate::spec::WORKER_AGENT_PREFIX) {
        _ if to == crate::spec::AGENT_SECRETARY || to == crate::spec::AGENT_CURATOR => true,
        Some(task) => run_exists(w, task)?,
        None => false,
    };
    if known {
        Ok(())
    } else {
        Err(refuse(
            ErrorKind::NotFound,
            format!("no recipient {to:?}"),
            "",
        ))
    }
}

/// Whether `agent` is the worker of a parked run (Issue 283).
fn parked(w: &StateWriter, agent: &str) -> Result<bool, DoorError> {
    super::park::parked_worker(w, agent)
        .map(|t| t.is_some())
        .map_err(internal)
}

fn run_exists(w: &StateWriter, task: &str) -> Result<bool, DoorError> {
    w.conn()
        .query_row("SELECT 1 FROM runs WHERE task_id = ?1", [task], |_| Ok(()))
        .optional()
        .map(|r| r.is_some())
        .map_err(internal)
}

/// A message, its `message_sent` event and its delivery row, as `delivery::pass` takes them.
fn store_message(
    w: &StateWriter,
    sender: &str,
    recipient: &str,
    kind: &str,
    task: Option<&str>,
    body: &str,
) -> Result<i64, DoorError> {
    message_row(w, sender, recipient, kind, task, body).map_err(internal)
}

/// A notice of `makibad` to `recipient` about `task` (a pass that cannot finish, Issue 37).
pub(super) fn notice(
    w: &StateWriter,
    recipient: &str,
    task: &str,
    body: &str,
) -> crate::db::Result<i64> {
    message_row(w, "makibad", recipient, "notice", Some(task), body)
}

/// A notice of `makibad` to `recipient` about no task (the triage scan, Issue 40).
pub(super) fn notice_untasked(
    w: &StateWriter,
    recipient: &str,
    body: &str,
) -> crate::db::Result<i64> {
    message_row(w, "makibad", recipient, "notice", None, body)
}

fn message_row(
    w: &StateWriter,
    sender: &str,
    recipient: &str,
    kind: &str,
    task: Option<&str>,
    body: &str,
) -> crate::db::Result<i64> {
    let store = || -> crate::db::Result<i64> {
        // A message to a parked worker wakes it (Issue 283); an answer has already done so with
        // its own reason, and a run no longer parked is left as it is.
        if let Some(task) = super::park::parked_worker(w, recipient)? {
            super::park::unpark(w, &task, "message", json!({"from": sender, "kind": kind}))?;
        }
        let event = w.append_event(&NewEvent {
            kind: "message_sent",
            actor: Some(sender),
            run_task_id: task,
            ..Default::default()
        })?;
        w.conn().execute(
            "INSERT INTO messages (event_id, sender, recipient, kind, run_id, body)
             VALUES (?1, ?2, ?3, ?4, (SELECT id FROM runs WHERE task_id = ?5), ?6)",
            params![event, sender, recipient, kind, task, body],
        )?;
        let id = w.conn().last_insert_rowid();
        w.conn().execute(
            "UPDATE events SET payload_json = json_object('message_id', ?1) WHERE id = ?2",
            params![id, event],
        )?;
        w.conn().execute(
            "INSERT INTO event_deliveries (source_event_id, recipient, status, attempt)
             VALUES (?1, ?2, 'pending', 0)",
            params![event, recipient],
        )?;
        Ok(id)
    };
    store()
}

fn report(w: &StateWriter, actor: &str, role: Role, args: &Value) -> Response {
    let state = str_arg(args, "state")?;
    if !["start", "done", "blocked"].contains(&state) {
        return Err(usage(format!(
            "state is start, done or blocked, not {state:?}"
        )));
    }
    let task = str_arg(args, "task")?;
    let text = str_arg(args, "text")?;
    if role == Role::Worker && actor != worker_agent_id(task) {
        return Err(refuse(
            ErrorKind::Denied,
            format!("{actor} reports on its own task only, not on {task}"),
            "",
        ));
    }
    if !run_exists(w, task)? {
        return Err(refuse(ErrorKind::NotFound, format!("no task {task:?}"), ""));
    }
    // A code task's done report carries the state of its review, recorded by the door (Issue 135).
    let review = if state == "done" && role == Role::Worker {
        last_review_clear(w, actor)?;
        review_state(w, actor, task)?
    } else {
        None
    };
    // A design-only run waiting for a landing (Issue 284): its worker's `blocked` is its design;
    // its `done`, whoever reports it, waits for the landing (else `close` could end it first).
    let ahead = match super::ahead::report(w.conn(), task, state).map_err(internal)? {
        super::ahead::Report::Design(..) if role != Role::Worker => super::ahead::Report::Plain,
        other => other,
    };
    if let super::ahead::Report::Refused(why) = &ahead {
        return Err(refuse(
            ErrorKind::Conflict,
            why.clone(),
            "you are woken on this conversation when what you wait for lands",
        ));
    }
    let mut body = match &review {
        Some(r) => format!("{text}\n\n[makiba] {}", review_line(r)),
        None => text.to_string(),
    };
    if let super::ahead::Report::Design(line, _) = &ahead {
        body.push_str(&format!("\n\n{line}"));
    }
    let kind = format!("report_{state}");
    let id = store_message(
        w,
        actor,
        crate::spec::AGENT_SECRETARY,
        &kind,
        Some(task),
        &body,
    )?;
    // Every worker report is in the history (Issue 409), its review null when there is none.
    if role == Role::Worker {
        let payload = json!({"task": task, "state": state, "review": review});
        w.append_event(&NewEvent {
            kind: "worker_reported",
            actor: Some(actor),
            payload: Some(&payload),
            run_task_id: Some(task),
            ..Default::default()
        })
        .map_err(internal)?;
    }
    if let super::ahead::Report::Design(_, wait) = &ahead {
        super::ahead::design_reported(w, task, wait).map_err(internal)?;
    }
    if state == "done" {
        let from: Option<String> = w
            .conn()
            .query_row("SELECT status FROM runs WHERE task_id = ?1", [task], |r| {
                r.get(0)
            })
            .optional()
            .map_err(internal)?;
        let moved = w
            .conn()
            .execute(
                "UPDATE runs SET status = 'review' WHERE task_id = ?1 AND status IN ('queued','in_use','suspended')",
                [task],
            )
            .map_err(internal)?;
        // The move into review: a range item's age in review starts here (Issue 414).
        if moved == 1 {
            let at: String = w
                .conn()
                .query_row(
                    "SELECT e.occurred_at FROM messages m JOIN events e ON e.id = m.event_id
                     WHERE m.id = ?1",
                    [id],
                    |r| r.get(0),
                )
                .map_err(internal)?;
            let payload = json!({"task_id": task, "message_id": id, "from": from});
            w.append_event(&NewEvent {
                kind: "run_in_review",
                actor: Some(actor),
                payload: Some(&payload),
                occurred_at: Some(&at),
                run_task_id: Some(task),
                ..Default::default()
            })
            .map_err(internal)?;
        }
    }
    // A worker that starts again from review (rework) is at work again, and counts again.
    if state == "start" {
        w.conn()
            .execute(
                "UPDATE runs SET status = 'in_use' WHERE task_id = ?1 AND status = 'review'",
                [task],
            )
            .map_err(internal)?;
    }
    Ok(json!({"id": id}))
}

/// `close` (spec section 12), the gate of Issue 36: refused without a `report_done` unless
/// `abandon`, or `limit` (Issue 320: a run whose current pull request merged, with a known limit
/// the person accepted); a run closed already is `done` again. A launch under way is failed.
/// Ending the terminal and removing the working folder are Issue 37's. The outcome is recorded in
/// `run_closed`'s payload (`status`, `reason`, `limit`), once: a retry of a failed teardown changes
/// neither it nor the run's status.
fn close(w: &StateWriter, actor: &str, role: Role, args: &Value) -> Response {
    let task = str_arg(args, "task_id")?;
    let abandon = args.get("abandon") == Some(&json!(true));
    let line = |name: &str| -> Result<Option<String>, DoorError> {
        match args.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(t)) if t.trim().is_empty() || t.contains(['\n', '\r']) => {
                Err(usage(format!("{name} is one line of text, not blank")))
            }
            Some(Value::String(t)) => Ok(Some(t.clone())),
            Some(_) => Err(usage(format!("the argument {name} is text"))),
        }
    };
    let limit = line("limit")?;
    let reason = line("reason")?;
    let pending = match args.get("pending") {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            v.as_i64()
                .filter(|n| *n > 0)
                .ok_or_else(|| usage("pending is the id of a pending entry"))?,
        ),
    };
    if limit.is_some() && abandon {
        return Err(usage(
            "a close with a limit is a completion: it takes no abandon",
        ));
    }
    if limit.is_some() && reason.is_none() {
        return Err(usage("a limit takes a reason"));
    }
    if reason.is_some() && !abandon && limit.is_none() {
        return Err(usage("a reason is given with abandon or with a limit"));
    }
    if pending.is_some() && limit.is_none() {
        return Err(usage(
            "pending names the person's yes to a limit: give the limit",
        ));
    }
    // The newest close of the run that was accepted (an `already` answer is not one).
    let row: Option<(i64, bool, bool, Option<String>)> = w
        .conn()
        .query_row(
            "SELECT r.id,
                    EXISTS (SELECT 1 FROM events WHERE run_id = r.id AND kind = 'run_closed'),
                    EXISTS (SELECT 1 FROM messages WHERE run_id = r.id AND kind = 'report_done'),
                    (SELECT c.status FROM commands c WHERE c.verb = 'close'
                       AND json_extract(c.args_json, '$.task_id') = r.task_id
                       AND c.status IN ('accepted', 'done', 'failed')
                       AND json_extract(c.result_json, '$.result.closed') IS NULL
                     ORDER BY c.id DESC LIMIT 1)
             FROM runs r WHERE r.task_id = ?1",
            [task],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .map_err(internal)?;
    let Some((run, closed, done, last)) = row else {
        return Err(refuse(ErrorKind::NotFound, format!("no task {task:?}"), ""));
    };
    // A close whose teardown failed is accepted again (Issue 37); any other close of a closed
    // run is `done` again.
    let retry = closed && last.as_deref() == Some("failed");
    if closed && !retry {
        return Ok(json!({"task_id": task, "closed": "already"}));
    }
    let record = match (&limit, retry) {
        (Some(_), true) => {
            // A limit is recorded only by the close that writes `run_closed`.
            let had: bool = w
                .conn()
                .query_row(
                    "SELECT EXISTS (SELECT 1 FROM events WHERE run_id = ?1 AND kind = 'run_closed'
                       AND json_type(payload_json, '$.limit') = 'object')",
                    [run],
                    |r| r.get(0),
                )
                .map_err(internal)?;
            if !had {
                return Err(refuse(
                    ErrorKind::Conflict,
                    format!("task {task:?} was closed without a limit"),
                    "close it again without one, or with --abandon",
                ));
            }
            Value::Null
        }
        (Some(finding), false) => accepted_limit(
            w,
            role,
            task,
            run,
            finding,
            reason.as_deref().unwrap_or(""),
            pending,
        )?,
        (None, _) => Value::Null,
    };
    if !closed && !done && !abandon && limit.is_none() {
        return Err(refuse(
            ErrorKind::Conflict,
            format!("task {task:?} has no done report"),
            "wait for the worker's report done, or close with --abandon (and a --reason); a \
             merged run with a limit the person accepted closes with --limit",
        ));
    }
    if !abandon {
        unsaved_work(w, run)?;
    }
    let outcome = if (done || limit.is_some()) && !abandon {
        "completed"
    } else {
        "abandoned"
    };
    let abandon_reason = if abandon { reason.as_deref() } else { None };
    let store = || -> crate::db::Result<()> {
        if retry {
            return Ok(());
        }
        w.conn().execute(
            "UPDATE runs SET status = ?1, completed_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
             WHERE id = ?2",
            params![outcome, run],
        )?;
        // A launch under way would start a worker for a closed run (as `down` fails them).
        w.conn().execute(
            "UPDATE run_launch SET stage = 'failed', failed_reason = 'makiba close'
             WHERE run_id = ?1 AND stage NOT IN ('verified', 'failed')",
            [run],
        )?;
        finish_dispatch(
            w,
            task,
            Some((
                ErrorKind::Conflict,
                "the run was closed before its launch was verified",
            )),
        )?;
        w.append_event(&NewEvent {
            kind: "run_closed",
            actor: Some(actor),
            payload: Some(&json!({
                "task_id": task, "status": outcome, "reason": abandon_reason, "limit": record,
            })),
            run_task_id: Some(task),
            ..Default::default()
        })?;
        Ok(())
    };
    store().map_err(internal)?;
    let now: String = w
        .conn()
        .query_row("SELECT status FROM runs WHERE id = ?1", [run], |r| r.get(0))
        .map_err(internal)?;
    Ok(json!({"task_id": task, "status": now}))
}

/// The question the secretary asks the person for a limit of `task` (Issue 320), word for word:
/// the person's yes to it, with the reason as its `why`, is the person's word for that limit.
fn limit_question(task: &str, finding: &str) -> String {
    format!("Accept as a known limit of {task} and close it as completed: {finding}")
}

/// The record of a limit the person accepted for the run `run` of `task` (Issue 320), or the
/// refusal: the person's word (the person's own key, or the pending entry `pending` of the run
/// that the person answered yes, asking `limit_question` with `reason` as its `why`, made at or
/// after the worker's last diff review) and a `pr_merged` of the run's current pull request.
fn accepted_limit(
    w: &StateWriter,
    role: Role,
    task: &str,
    run: i64,
    finding: &str,
    reason: &str,
    pending: Option<i64>,
) -> Result<Value, DoorError> {
    let c = w.conn();
    // The worker's last diff review, read as `report` reads it.
    let review: Option<(i64, String, Value)> = c
        .query_row(
            "SELECT id, received_at, result_json FROM commands
             WHERE verb = 'review' AND actor = ?1 AND status = 'done'
               AND json_extract(result_json, '$.result.kind') = 'diff'
             ORDER BY id DESC LIMIT 1",
            [worker_agent_id(task)],
            |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, String>(2)?)),
        )
        .optional()
        .map_err(internal)?
        .map(|(id, at, text)| (id, at, serde_json::from_str(&text).unwrap_or(Value::Null)));
    let question = limit_question(task, finding);
    let no_word = || {
        refuse(
            ErrorKind::Conflict,
            format!(
                "no accepted limit: the person has not answered yes to the limit's question of {task}"
            ),
            format!(
                "pending add, for the task, the question {question:?} with the reason as why, \
                 escalate it, and close with --pending once the person answers yes"
            ),
        )
    };
    let answered_at: Option<String> = match pending {
        None if role == Role::Person => None,
        None => return Err(no_word()),
        Some(id) => {
            let entry: Option<(String, String)> = c
                .query_row(
                    "SELECT created_at, answered_at FROM pending
                     WHERE id = ?1 AND run_id = ?2 AND kind = 'question' AND status = 'answered'
                       AND answer = 'yes' AND answered_by = ?3 AND question = ?4
                       AND json_extract(context_json, '$.why') = ?5 AND answered_at IS NOT NULL",
                    params![id, run, crate::spec::AGENT_PERSON, question, reason],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(internal)?;
            let Some((created, answered)) = entry else {
                return Err(no_word());
            };
            if review.as_ref().is_some_and(|(_, at, _)| created < *at) {
                return Err(refuse(
                    ErrorKind::Conflict,
                    format!("pending entry {id} was asked before the last diff review of {task}"),
                    "ask the person again about the last review",
                ));
            }
            Some(answered)
        }
    };
    let merge: Option<(i64, Value)> = c
        .query_row(
            &format!(
                "SELECT e.id, e.payload_json FROM events e JOIN runs r ON r.id = e.run_id
                 WHERE e.run_id = ?1 AND e.kind = 'pr_merged' AND {}
                 ORDER BY e.id DESC LIMIT 1",
                super::belt::is_current_pr_sql(
                    "r",
                    "json_extract(e.payload_json, '$.repo')",
                    "json_extract(e.payload_json, '$.pr')"
                )
            ),
            [run],
            |r| Ok((r.get(0)?, r.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(internal)?
        .map(|(id, p)| (id, serde_json::from_str(&p).unwrap_or(Value::Null)));
    let Some((merge_id, merged)) = merge else {
        return Err(refuse(
            ErrorKind::Conflict,
            format!("task {task:?} has no merge of its current pull request recorded"),
            "close with --abandon and a --reason, or wait for the merge to be recorded",
        ));
    };
    let read = review.map(|(id, _, r)| {
        json!({
            "command_id": id,
            "head": r["result"]["head"],
            "findings": r["result"]["findings"],
        })
    });
    Ok(json!({
        "finding": finding,
        "reason": reason,
        "accepted_by": crate::spec::AGENT_PERSON,
        "pending_id": pending,
        "answered_at": answered_at,
        "review": read,
        "merge": {
            "event_id": merge_id,
            "repo": merged["repo"],
            "pr": merged["pr"],
            "merge_commit": merged["merge_commit"],
        },
    }))
}

/// Refuse a close (without `--abandon`) while a code worktree of the run holds unsaved work.
fn unsaved_work(w: &StateWriter, run: i64) -> Result<(), DoorError> {
    let rows: Vec<(String, String, String, String)> = w
        .conn()
        .prepare(
            "SELECT DISTINCT l.worktree, r.task_id, COALESCE(p.slug, ''),
                    COALESCE(r.branch, 'makiba/' || r.task_id)
             FROM run_launch l JOIN runs r ON r.id = l.run_id
             LEFT JOIN projects p ON p.id = r.project_id
             WHERE l.run_id = ?1 AND l.kind = 'code'",
        )
        .and_then(|mut st| {
            st.query_map([run], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                .collect::<rusqlite::Result<_>>()
        })
        .map_err(internal)?;
    // The run's git folder is in the state folder, beside `state.db` (Issue 241).
    let state = w.conn().path().map(Path::new).and_then(Path::parent);
    let places = super::prepare::Places::from_env(state.unwrap_or(Path::new("")));
    for (p, task, project, branch) in &rows {
        let p = Path::new(p);
        if !p.exists() {
            continue;
        }
        let gitdir = super::run_repo::dir(&places.settings, task);
        let repo = places.repos.join(project);
        if let Some(why) = super::close::unsaved(&gitdir, p, &repo, branch) {
            return Err(refuse(
                ErrorKind::Conflict,
                why,
                "push the work (commit it first), or close with --abandon, which removes the \
                 worktree and keeps its branch in the project's repository",
            ));
        }
    }
    Ok(())
}

/// `pr opened|pushed|rerun` (spec section 12): the event, the older watches of the PR `superseded`
/// and the watch of the new head, as `prwatch::register` does in one transaction. `base_merge`:
/// the door found the pushed head to be a clean merge of the base (Issue 358). makibad's own
/// update on its queue readiness judgement records its head through this too (`push.rs`, Issue
/// 475), in the transaction of the update's end.
pub(super) fn pr(w: &StateWriter, args: &Value, base_merge: bool) -> Response {
    let state = str_arg(args, "state")?;
    let kind = match state {
        "opened" => "pr_opened",
        "pushed" => "fix_pushed",
        "rerun" => "ci_rerun",
        _ => {
            return Err(usage(format!(
                "state is opened, pushed or rerun, not {state:?}"
            )));
        }
    };
    let repo = str_arg(args, "repo")?;
    let repo_ok = repo.split_once('/').is_some_and(|(o, n)| {
        let part = |s: &str| {
            !s.is_empty()
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
        };
        part(o) && part(n)
    });
    if !repo_ok {
        return Err(usage(format!("repo is owner/name, not {repo:?}")));
    }
    let number = args
        .get("pr")
        .and_then(Value::as_i64)
        .filter(|n| *n > 0)
        .ok_or_else(|| usage("the argument pr (a pull request number) is missing"))?;
    let head = str_arg(args, "head")?;
    if head.len() != 40 || !head.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')) {
        return Err(usage(format!(
            "head is a full commit id (40 lowercase hex), not {head:?}"
        )));
    }
    let task = str_arg(args, "task")?;
    if !run_exists(w, task)? {
        return Err(refuse(ErrorKind::NotFound, format!("no task {task:?}"), ""));
    }
    if kind == "ci_rerun" {
        // Issue 375: the same head watched again after a rerun of its failed jobs. No new head
        // is recorded, so neither the check before CI nor the belt reads it.
        use super::prwatch::RerunRefused;
        let short = |h: &str| h[..h.len().min(12)].to_string();
        return match super::prwatch::rerun(w, task, repo, number, head).map_err(internal)? {
            Ok(r) => Ok(json!({"watch_id": r.watch_id, "status": r.status})),
            Err(RerunRefused::NotRecorded) => Err(refuse(
                ErrorKind::NotFound,
                format!("{repo}#{number} was never recorded at {}", short(head)),
                "record the head with pr opened or pushed; rerun watches a head already recorded",
            )),
            Err(RerunRefused::NotCurrent(current)) => Err(refuse(
                ErrorKind::Conflict,
                format!(
                    "the head recorded last for {repo}#{number} is {}, not {}; a rerun is watched only on the pull request's current head",
                    short(&current),
                    short(head)
                ),
                "",
            )),
            Err(RerunRefused::Passed) => Err(refuse(
                ErrorKind::Conflict,
                format!(
                    "CI already passed for {repo}#{number} at {}; there is nothing to rerun",
                    short(head)
                ),
                "",
            )),
        };
    }
    // Issue 431 [R1]: the run's current pull request stays current while makibad's merge act on it
    // awaits its landing, so the PR watch, which follows the current one, reads that landing.
    let current: Option<(Option<String>, Option<i64>)> = w
        .conn()
        .query_row(
            &format!(
                "SELECT {}, {} FROM runs r WHERE r.task_id = ?1",
                super::belt::current_pr_sql("r", "repo"),
                super::belt::current_pr_sql("r", "pr")
            ),
            [task],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(internal)?;
    if let Some((Some(crepo), Some(cpr))) = current
        && !(crepo.eq_ignore_ascii_case(repo) && cpr == number)
        && let Some(a) = act_awaiting(w.conn(), &crepo, cpr).map_err(internal)?
    {
        return Err(refuse(
            ErrorKind::Conflict,
            format!(
                "{crepo}#{cpr}, the current pull request of {task}, has a merge act of makibad awaiting its landing (attempt {}); {repo}#{number} was not recorded",
                a.attempt
            ),
            "wait for its landing, or for the queue to drop it",
        ));
    }
    // Issue 431 [R4]: a queue entry makibad made is bound to its head; a new head recorded for the
    // pull request ends that queue act.
    let dequeued = match act_awaiting(w.conn(), repo, number).map_err(internal)? {
        Some(a) if kind == "fix_pushed" && a.queue && a.head != head => Some(a),
        _ => None,
    };
    let base_merge = base_merge && kind == "fix_pushed";
    let r = super::prwatch::register_with(w, kind, task, repo, number, head, base_merge)
        .map_err(internal)?;
    if let Some(a) = dequeued {
        merge_dequeued(w, task, repo, number, &a, "head recorded").map_err(internal)?;
    }
    // Issue 432: a pending merge of another head of this pull request, or of the pull request this
    // one replaces as the run's current one, is no longer what the person would approve.
    withdraw_merge_entries(
        w,
        task,
        repo,
        number,
        Some(head),
        true,
        &format!("a new head {} was recorded", &head[..12]),
    )
    .map_err(internal)?;
    // The belt (Issue 282): the stops of an older head go, and a fix that reaches the range's
    // `budget_fixes` is a stop of the task; a clean merge of the base is no fix (Issue 358). The
    // push is recorded whatever the count.
    super::belt::head_recorded(w, task, repo, number, head).map_err(internal)?;
    Ok(json!({"watch_id": r.watch_id, "status": r.status}))
}

/// The four headings of a raw note (skills/curate, roles/curator).
const NOTE_HEADINGS: [&str; 4] = [
    "## Facts",
    "## Judgment",
    "## Grounds",
    "## Where it applies",
];

/// `note` (spec section 12, Issue 189): the secretary's raw note, saved by `makibad` as
/// `<knowledge>/raw/<date>-<topic>[-n].md` with `Task: <task_id>` first. The same idem returns
/// the first file through the door's replay.
// ponytail: makibad dying between the file's publication and the commit leaves the note without
// its record, so a retry of the same idem saves a second copy (never a lost note). A stored
// intent reconciled at start would close it.
fn note(w: &StateWriter, actor: &str, args: &Value) -> Response {
    let task = str_arg(args, "task")?;
    let topic = str_arg(args, "topic")?;
    let text = str_arg(args, "text")?;
    kebab("topic", topic)?;
    let missing: Vec<&str> = NOTE_HEADINGS
        .into_iter()
        .filter(|h| !text.lines().any(|l| l.trim_end() == *h))
        .collect();
    if !missing.is_empty() {
        return Err(usage(format!(
            "a raw note has the four headings {}; missing: {}",
            NOTE_HEADINGS.join(", "),
            missing.join(", ")
        )));
    }
    if !run_exists(w, task)? {
        return Err(refuse(ErrorKind::NotFound, format!("no task {task:?}"), ""));
    }
    let date: String = w
        .conn()
        .query_row("SELECT date('now', 'localtime')", [], |r| r.get(0))
        .map_err(internal)?;
    let mut body = format!("Task: {task}\n\n{text}");
    if !body.ends_with('\n') {
        body.push('\n');
    }
    let raw = super::curator::knowledge_dir().join("raw");
    let name = save_note(&raw, &format!("{date}-{topic}"), &body)
        .map_err(|e| internal(format!("cannot save the note in {}: {e}", raw.display())))?;
    let file = format!("raw/{name}");
    let payload = json!({"task_id": task, "file": file});
    if let Err(e) = w.append_event(&NewEvent {
        kind: "raw_note_saved",
        actor: Some(actor),
        payload: Some(&payload),
        run_task_id: Some(task),
        ..Default::default()
    }) {
        let _ = std::fs::remove_file(raw.join(&name));
        return Err(internal(e));
    }
    Ok(json!({"file": file}))
}

/// English kebab-case: a-z, 0-9, single dashes, at most 64.
fn kebab(what: &str, s: &str) -> Result<(), DoorError> {
    let ok = s.len() <= 64
        && s.split('-').all(|p| {
            !p.is_empty()
                && p.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        });
    if ok {
        Ok(())
    } else {
        Err(usage(format!(
            "{what} is English kebab-case (a-z, 0-9, single dashes, at most 64), not {s:?}"
        )))
    }
}

/// `candidate` (spec section 12, Issue 191): the person's decision on a skill candidate, written
/// by `makibad` into the entry of `pattern` in `<knowledge>/skill-candidates.md`. The file is
/// replaced whole; `undo` gets its old bytes for the door to put back if the record fails.
// ponytail: makibad dying between the rename and the commit leaves the decision in the file without
// its record; a retry is then `conflict`, which tells the truth about the file. A stored intent
// reconciled at start would close it.
fn candidate(
    w: &StateWriter,
    actor: &str,
    args: &Value,
    undo: &mut Vec<(PathBuf, Option<Vec<u8>>)>,
) -> Response {
    let pattern = str_arg(args, "pattern")?;
    let decision = str_arg(args, "decision")?;
    let reason = str_arg(args, "reason")?.trim();
    kebab("pattern", pattern)?;
    // The words of skills/curate.
    let status = match decision {
        "approved" | "rejected" | "deferred" => decision.to_string(),
        d => match d.strip_prefix("into-") {
            Some(skill) => {
                kebab("the skill of into-<skill>", skill)?;
                format!("merged-into-{skill}")
            }
            None => {
                return Err(usage(format!(
                    "decision is approved, rejected, deferred or into-<skill>, not {d:?}"
                )));
            }
        },
    };
    if reason.is_empty() || reason.chars().any(char::is_control) {
        return Err(usage("reason is one line of text, not blank"));
    }
    if curator_at_work(w)? {
        return Err(refuse(
            ErrorKind::Conflict,
            "the curator is at work and may be writing skill-candidates.md; nothing was written",
            "decide after its report, once it has ended",
        ));
    }
    let path = super::curator::knowledge_dir().join(CANDIDATES_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(refuse(
                ErrorKind::NotFound,
                format!("no {CANDIDATES_FILE}, so no pattern {pattern:?}"),
                "",
            ));
        }
        Err(e) => return Err(internal(format!("cannot read {}: {e}", path.display()))),
    };
    let decided: String = w
        .conn()
        .query_row("SELECT date('now', 'localtime')", [], |r| r.get(0))
        .map_err(internal)?;
    let new = decide_entry(&text, pattern, &status, &decided, reason)?;
    let payload =
        json!({"pattern": pattern, "status": status, "decided": decided, "reason": reason});
    w.append_event(&NewEvent {
        kind: "skill_candidate_decided",
        actor: Some(actor),
        payload: Some(&payload),
        ..Default::default()
    })
    .map_err(internal)?;
    replace_file(&path, new.as_bytes())
        .map_err(|e| internal(format!("cannot write {}: {e}", path.display())))?;
    undo.push((path, Some(text.into_bytes())));
    Ok(payload)
}

/// `goal` (spec section 12, Issue 271): `show`, `check` and `place` a project's goal ledger
/// (`docs/goals.md`). Only a draft the ledger's own parser accepts is written, and it is read back
/// with the loader the scan uses; the old file is kept beside it as `<repo>.md.prev`. The file is
/// written outside the transaction: `undo` gets its old bytes (none: it was absent) for the door
/// to put back if the record fails.
fn goal(
    w: &StateWriter,
    actor: &str,
    args: &Value,
    undo: &mut Vec<(PathBuf, Option<Vec<u8>>)>,
) -> Response {
    use crate::work_discovery::goals::{GoalStatus, load_goals, parse_ledger, split_slug};
    let action = str_arg(args, "action")?;
    let repo = str_arg(args, "repo")?;
    let Some((owner, name)) = split_slug(repo) else {
        return Err(usage(format!("repo is OWNER/NAME, not {repo:?}")));
    };
    let slug = format!("{owner}/{name}");
    let dir = crate::cli::goals_dir();
    let clauses = |c: &[crate::work_discovery::goals::Clause]| -> Vec<Value> {
        c.iter().map(|c| c.to_json()).collect()
    };
    let invalid = |e: String| usage(format!("the ledger is not valid: {e}"));
    let before = load_goals(&dir, &slug);
    let target = dir.join(&owner).join(format!("{name}.md"));
    match action {
        "show" => {
            let text = before
                .path
                .as_ref()
                .and_then(|p| std::fs::read_to_string(p).ok());
            let placed: Option<(String, String)> = w
                .conn()
                .query_row(
                    "SELECT actor, occurred_at FROM events WHERE kind = 'goal_ledger_placed'
                     AND json_extract(payload_json, '$.repo') = ?1 ORDER BY id DESC LIMIT 1",
                    [&slug],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(internal)?;
            let status = match before.status {
                GoalStatus::Ok => "ok",
                GoalStatus::Unset => "unset",
                GoalStatus::Error => "error",
            };
            Ok(
                json!({"repo": slug, "status": status, "text": text, "clauses": clauses(&before.clauses),
                      "error": before.error, "placed_by": placed.as_ref().map(|p| &p.0),
                      "placed_at": placed.as_ref().map(|p| &p.1)}),
            )
        }
        "check" | "place" => {
            let text = str_arg(args, "text")?;
            let parsed = parse_ledger(text).map_err(invalid)?;
            if action == "check" {
                return Ok(json!({"repo": slug, "status": "valid", "clauses": clauses(&parsed)}));
            }
            // A file under another case of the name, or in a folder of another case, is the
            // loader's error: it is not replaced behind the person's back.
            if before.path.as_deref().is_some_and(|p| p != target) {
                return Err(refuse(
                    ErrorKind::Conflict,
                    format!(
                        "the ledger of {slug} is at {:?}, not {}; nothing was written",
                        before.path.unwrap_or_default(),
                        target.display()
                    ),
                    "rename it by hand, the case of the name must be lower case",
                ));
            }
            if before
                .error
                .as_deref()
                .is_some_and(|e| e.starts_with("goals dir unreadable"))
            {
                return Err(internal(before.error.unwrap_or_default()));
            }
            let old = match std::fs::read(&target) {
                Ok(b) => Some(b),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(internal(format!("cannot read {}: {e}", target.display()))),
            };
            let payload = json!({
                "repo": slug, "clauses": parsed.len(), "bytes": text.len(),
                "by": actor,
                "previous_sha256": old.as_ref().map(|b| hex_sha256(b)),
                "previous_clauses": (before.status == GoalStatus::Ok).then_some(before.clauses.len()),
            });
            w.append_event(&NewEvent {
                kind: "goal_ledger_placed",
                actor: Some(actor),
                payload: Some(&payload),
                ..Default::default()
            })
            .map_err(internal)?;
            let io =
                |e: std::io::Error| internal(format!("cannot write {}: {e}", target.display()));
            std::fs::create_dir_all(target.parent().unwrap_or(&dir)).map_err(io)?;
            // Each file's old bytes (or absence) are noted before it is written, so a failure at
            // any step, or a record that does not stand, puts every file back.
            let prev = target.with_file_name(format!("{name}.md.prev"));
            let restore = |undo: &mut Vec<(PathBuf, Option<Vec<u8>>)>| {
                for (path, old) in undo.drain(..).rev() {
                    let _ = match old {
                        Some(old) => replace_file(&path, &old),
                        None => std::fs::remove_file(&path),
                    };
                }
            };
            if let Some(old) = &old {
                let was = std::fs::read(&prev).ok();
                undo.push((prev.clone(), was));
                if let Err(e) = replace_file(&prev, old) {
                    restore(undo);
                    return Err(io(e));
                }
            }
            undo.push((target.clone(), old.clone()));
            if let Err(e) = replace_file(&target, text.as_bytes()) {
                restore(undo);
                return Err(io(e));
            }
            let after = load_goals(&dir, &slug);
            if after.status != GoalStatus::Ok || after.clauses != parsed {
                // An error is not unsaved by the door: put the old files back here.
                restore(undo);
                return Err(internal(format!(
                    "the placed ledger does not read back ({}); it was not kept",
                    after.error.unwrap_or_default()
                )));
            }
            Ok(
                json!({"repo": slug, "status": "placed", "clauses": clauses(&after.clauses),
                      "previous": payload["previous_clauses"], "by": actor}),
            )
        }
        a => Err(usage(format!("action is show, check or place, not {a:?}"))),
    }
}

const CANDIDATES_FILE: &str = "skill-candidates.md";

/// Whether a curator may be writing the knowledge folder: a launch under way, or a curator key
/// not yet revoked (`makibad` revokes it when the curator reported or ended). The door's
/// transaction holds the write lock, so no launch can be added before the file is replaced.
fn curator_at_work(w: &StateWriter) -> Result<bool, DoorError> {
    w.conn()
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM run_launch
                            WHERE role = 'curator' AND stage NOT IN ('verified', 'failed'))
                 OR EXISTS (SELECT 1 FROM agents WHERE role = 'curator' AND revoked_at IS NULL)",
            [],
            |r| r.get(0),
        )
        .map_err(internal)
}

/// The lines of an entry of `skill-candidates.md` (skills/curate, roles/curator).
const CANDIDATE_KEYS: [&str; 8] = [
    "pattern", "date", "notes", "when", "skill", "status", "decided", "reason",
];

/// A line `<key>: <value>`, after leading spaces and an optional `- `: the key and the byte
/// offset just past its `:`.
fn candidate_field(line: &str) -> Option<(&str, usize)> {
    let body = line.trim_start();
    let body = body.strip_prefix("- ").unwrap_or(body);
    let (key, _) = body.split_once(':')?;
    let key = CANDIDATE_KEYS.into_iter().find(|k| *k == key)?;
    Some((key, line.len() - body.len() + key.len() + 1))
}

/// `text` with the `status`, `decided` and `reason` values of the one open entry of `pattern`
/// replaced; every other byte as it was. An entry is open while its status is `pending` or
/// `deferred` (held by the person, Issue 270); entries already decided stay beside it as history
/// when the pattern returned. A held entry is not held again.
fn decide_entry(
    text: &str,
    pattern: &str,
    status: &str,
    decided: &str,
    reason: &str,
) -> Result<String, DoorError> {
    let mut lines: Vec<String> = text.split_inclusive('\n').map(String::from).collect();
    let value = |line: &str, at: usize| line[at..].trim().to_string();
    let starts: Vec<usize> = (0..lines.len())
        .filter(|i| candidate_field(&lines[*i]).is_some_and(|(k, _)| k == "pattern"))
        .collect();
    let ours: Vec<(usize, usize)> = starts
        .iter()
        .enumerate()
        .filter(|(_, i)| {
            let (_, at) = candidate_field(&lines[**i]).unwrap();
            let v = value(&lines[**i], at);
            v.strip_prefix('`')
                .and_then(|v| v.strip_suffix('`'))
                .unwrap_or(&v)
                == pattern
        })
        .map(|(n, i)| (*i, starts.get(n + 1).copied().unwrap_or(lines.len())))
        .collect();
    if ours.is_empty() {
        return Err(refuse(
            ErrorKind::NotFound,
            format!("no entry of pattern {pattern:?} in {CANDIDATES_FILE}"),
            "",
        ));
    }
    // Each entry's three lines (the line and the offset past its key), and its status as written.
    let mut entries: Vec<(Vec<(usize, usize)>, String)> = Vec::new();
    for (from, to) in &ours {
        let mut at = Vec::new();
        for key in ["status", "decided", "reason"] {
            let Some(found) = (*from..*to).find_map(|i| {
                candidate_field(&lines[i])
                    .filter(|(k, _)| *k == key)
                    .map(|(_, at)| (i, at))
            }) else {
                return Err(refuse(
                    ErrorKind::Conflict,
                    format!(
                        "an entry of pattern {pattern:?} has no {key} line; nothing was written"
                    ),
                    "",
                ));
            };
            at.push(found);
        }
        let was = value(&lines[at[0].0], at[0].1);
        entries.push((at, was));
    }
    let open: Vec<&(Vec<(usize, usize)>, String)> = entries
        .iter()
        .filter(|(_, was)| was == "pending" || was == "deferred")
        .collect();
    let (at, was) = match open[..] {
        [] => {
            return Err(refuse(
                ErrorKind::Conflict,
                format!(
                    "the entry of pattern {pattern:?} is decided already: {}",
                    entries.last().map_or("", |(_, was)| was)
                ),
                "",
            ));
        }
        [one] => one,
        _ => {
            return Err(refuse(
                ErrorKind::Conflict,
                format!(
                    "{} open entries of pattern {pattern:?} in {CANDIDATES_FILE}; nothing was written",
                    open.len()
                ),
                "the curator must merge them into one",
            ));
        }
    };
    if status == "deferred" && was == "deferred" {
        return Err(refuse(
            ErrorKind::Conflict,
            format!("the entry of pattern {pattern:?} is deferred already"),
            "decide it with approved, rejected or into-<skill>",
        ));
    }
    let set: Vec<(usize, usize, &str)> = at
        .iter()
        .zip([status, decided, reason])
        .map(|((i, at), new)| (*i, *at, new))
        .collect();
    for (i, at, new) in set {
        let line = &lines[i];
        let body = line.trim_end_matches(['\n', '\r']);
        let end = &line[body.len()..];
        lines[i] = format!("{} {new}{end}", &line[..at]);
    }
    Ok(lines.concat())
}

/// Replace `path` whole: `bytes` go to a dot file beside it, flushed, then renamed over it, so a
/// reader sees the old file or the new one, never a part.
fn replace_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let staged = path.with_file_name(format!(".{name}-{}", random_key()?));
    let r = (|| {
        let mut f = std::fs::File::create(&staged)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        std::fs::rename(&staged, path)
    })();
    if r.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    r
}

/// Write `body` whole to a staging file (a dot name, which the curator's count skips), then
/// publish it under the first free `<stem>.md`, `<stem>-2.md`, ... by a hard link, which never
/// replaces a file. A name in `raw/archive/` is not free. Returns the name.
fn save_note(raw: &Path, stem: &str, body: &str) -> std::io::Result<String> {
    std::fs::create_dir_all(raw)?;
    let staged = raw.join(format!(".note-{}", random_key()?));
    std::fs::write(&staged, body)?;
    let r = publish_note(raw, stem, &staged);
    let _ = std::fs::remove_file(&staged);
    r
}

fn publish_note(raw: &Path, stem: &str, staged: &Path) -> std::io::Result<String> {
    let archived = |name: &str| raw.join("archive").join(name).try_exists();
    for n in 1..=1000 {
        let name = match n {
            1 => format!("{stem}.md"),
            n => format!("{stem}-{n}.md"),
        };
        if archived(&name)? {
            continue;
        }
        match std::fs::hard_link(staged, raw.join(&name)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
        // The curator moved `raw/<name>` to the archive between the check and the link.
        if archived(&name)? {
            let _ = std::fs::remove_file(raw.join(&name));
            continue;
        }
        return Ok(name);
    }
    Err(std::io::Error::other(format!("no free name for {stem}")))
}

/// `push` (Issue 190): accepted for a run that has a code worktree; `makibad` pushes it on a
/// thread of its own (push.rs) and ends the command `done` or `failed` with a notice.
fn push(w: &StateWriter, args: &Value) -> Response {
    let task = str_arg(args, "task")?;
    // Issue 307: `head`, the branch's test commit pushed alone (push.rs checks it is that).
    if let Some(h) = args.get("head").filter(|h| !h.is_null()) {
        let ok = h.as_str().is_some_and(|h| {
            h.len() == 40 && h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        });
        if !ok {
            return Err(usage(format!(
                "head is a full commit id (40 lowercase hex), not {h}"
            )));
        }
    }
    let worktree: Option<Option<String>> = w
        .conn()
        .query_row(
            "SELECT (SELECT worktree FROM run_launch WHERE run_id = r.id AND kind = 'code'
                     ORDER BY id DESC LIMIT 1)
             FROM runs r WHERE r.task_id = ?1",
            [task],
            |r| r.get(0),
        )
        .optional()
        .map_err(internal)?;
    match worktree {
        None => Err(refuse(ErrorKind::NotFound, format!("no task {task:?}"), "")),
        Some(None) => Err(refuse(
            ErrorKind::Conflict,
            format!("task {task:?} has no code worktree to push"),
            "",
        )),
        Some(Some(_)) => Ok(json!({"task": task, "status": "accepted"})),
    }
}

/// `update` (Issue 183): `base` a valid branch name, then accepted as `push` is; `makibad` merges
/// `origin/<base>` into the branch and pushes it on a thread of its own (push.rs).
fn update(w: &StateWriter, args: &Value) -> Response {
    let base = str_arg(args, "base")?;
    let valid = !base.starts_with('-')
        && std::process::Command::new("git")
            .args(["check-ref-format", "--branch", base])
            .stdin(std::process::Stdio::null())
            .output()
            .is_ok_and(|o| o.status.success());
    if !valid {
        return Err(refuse(
            ErrorKind::Usage,
            format!("{base:?} is not a branch name"),
            "",
        ));
    }
    push(w, args).map(|mut v| {
        v["base"] = base.into();
        v
    })
}

fn show(w: &StateWriter, actor: &str, args: &Value) -> Response {
    let id = args
        .get("id")
        .and_then(Value::as_i64)
        .ok_or_else(|| usage("the argument id (a message id) is missing"))?;
    let row: Option<(String, String, String, String)> = w
        .conn()
        .query_row(
            "SELECT sender, recipient, kind, body FROM messages WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .map_err(internal)?;
    let Some((from, to, kind, body)) = row else {
        return Err(refuse(ErrorKind::NotFound, format!("no message {id}"), ""));
    };
    if actor != from && actor != to {
        return Err(refuse(
            ErrorKind::Denied,
            format!("only the sender and the recipient of message {id} may read it"),
            "",
        ));
    }
    Ok(json!({"id": id, "from": from, "to": to, "kind": kind, "body": body}))
}

/// `config` (spec section 12, Issue 48). A set is a `config_set` event in the command's own
/// transaction; `max_concurrent_workers` and `merge_approval_mode` are not stored yet.
fn config(w: &StateWriter, actor: &str, args: &Value) -> Response {
    let key = str_arg(args, "key")?;
    if !CONFIG_KEYS.contains(&key) {
        return Err(usage(format!(
            "no setting {key:?}; the settings are {}",
            CONFIG_KEYS.join(", ")
        )));
    }
    let action = str_arg(args, "action")?;
    if !["get", "set"].contains(&action) {
        return Err(usage(format!("action is get or set, not {action:?}")));
    }
    if key == super::project::OWNERS_KEY {
        return allowed_owners(w, actor, action, args);
    }
    if key == super::project::HOSTS_KEY {
        return network_hosts(w, actor, action, args);
    }
    if key == super::call::KEY {
        return notify_command(w, actor, action, args);
    }
    let Some((kind, per_project)) = super::language::kind_of(key) else {
        return match action {
            "get" => Ok(json!({"key": key, "value": null})),
            _ => Err(internal(format!(
                "config set {key} is not built yet (its environment variable sets it)"
            ))),
        };
    };
    let project = args
        .get("project")
        .and_then(Value::as_str)
        .filter(|p| !p.is_empty());
    let project = match (project, per_project) {
        (Some(p), true) => {
            plain_name("project", p)?;
            p
        }
        (None, true) => {
            return Err(usage(format!(
                "{key} is a project's: name it with --project"
            )));
        }
        (Some(_), false) => {
            return Err(usage(format!("{key} is the organization's: no --project")));
        }
        (None, false) => "",
    };
    let shown = |v: Option<&str>| json!({"key": key, "project": project, "value": v});
    if action == "get" {
        let v = super::language::get(w.conn(), project, key).map_err(internal)?;
        return Ok(shown(v.as_deref()));
    }
    let value = str_arg(args, "value")?;
    if super::language::sentence(kind, value).is_none() {
        return Err(usage(format!(
            "{key} is one of {}, not {value:?}",
            super::language::codes(kind).join(", ")
        )));
    }
    let store = || -> crate::db::Result<()> {
        w.append_event(&NewEvent {
            kind: "config_set",
            actor: Some(actor),
            payload: Some(&json!({"project": project, "key": key, "value": value})),
            ..Default::default()
        })?;
        // The secretary opens the project's pull requests and issues (skills makiba:pull-request, makiba:issue).
        if per_project {
            notice_untasked(
                w,
                crate::spec::AGENT_SECRETARY,
                &format!(
                    "The artifact language of project {project} is now {value}: write its pull requests and issues in it."
                ),
            )?;
        }
        Ok(())
    };
    store().map_err(internal)?;
    Ok(shown(Some(value)))
}

/// `config get|set allowed_repo_owners` (Issue 184): the organization's, a comma list of GitHub
/// owners whose repositories the secretary may have fetched. Stored as `config_set`.
fn allowed_owners(w: &StateWriter, actor: &str, action: &str, args: &Value) -> Response {
    let key = super::project::OWNERS_KEY;
    if args
        .get("project")
        .and_then(Value::as_str)
        .is_some_and(|p| !p.is_empty())
    {
        return Err(usage(format!("{key} is the organization's: no --project")));
    }
    let shown = |v: Option<&str>| json!({"key": key, "project": "", "value": v});
    if action == "get" {
        let v = super::language::get(w.conn(), "", key).map_err(internal)?;
        return Ok(shown(v.as_deref()));
    }
    let value = str_arg(args, "value")?;
    let Some(value) = super::project::owners_value(value) else {
        return Err(usage(format!(
            "{key} is a comma list of GitHub owners, not {value:?}"
        )));
    };
    w.append_event(&NewEvent {
        kind: "config_set",
        actor: Some(actor),
        payload: Some(&json!({"project": "", "key": key, "value": value})),
        ..Default::default()
    })
    .map_err(internal)?;
    Ok(shown(Some(&value)))
}

/// `config get|set notify_command` (Issue 272): the organization's, the absolute path of the
/// person's command that calls the person back (docs/design/person-call.md); empty is off. A path
/// an agent could write is refused (`call::check_set`). Stored as `config_set`.
fn notify_command(w: &StateWriter, actor: &str, action: &str, args: &Value) -> Response {
    let key = super::call::KEY;
    if args
        .get("project")
        .and_then(Value::as_str)
        .is_some_and(|p| !p.is_empty())
    {
        return Err(usage(format!("{key} is the organization's: no --project")));
    }
    let shown = |v: Option<&str>| json!({"key": key, "project": "", "value": v});
    if action == "get" {
        let v = super::language::get(w.conn(), "", key).map_err(internal)?;
        return Ok(shown(v.as_deref()));
    }
    let value = str_arg(args, "value")?;
    let state = w
        .conn()
        .path()
        .and_then(|p| Path::new(p).parent().map(Path::to_path_buf))
        .unwrap_or_default();
    super::call::check_set(w.conn(), value, &state).map_err(usage)?;
    w.append_event(&NewEvent {
        kind: "config_set",
        actor: Some(actor),
        payload: Some(&json!({"project": "", "key": key, "value": value})),
        ..Default::default()
    })
    .map_err(internal)?;
    Ok(shown(Some(value)))
}

/// `config get|set network_hosts --project <slug>` (Issue 244): the hosts a project's workers may
/// reach, a comma list of host names, never GitHub's. Stored as `config_set`; a launch reads it
/// when its settings are written, so a running worker keeps the list it was launched with.
fn network_hosts(w: &StateWriter, actor: &str, action: &str, args: &Value) -> Response {
    let key = super::project::HOSTS_KEY;
    let project = args
        .get("project")
        .and_then(Value::as_str)
        .filter(|p| !p.is_empty())
        .ok_or_else(|| usage(format!("{key} is a project's: name it with --project")))?;
    plain_name("project", project)?;
    let shown = |v: Option<&str>| json!({"key": key, "project": project, "value": v});
    if action == "get" {
        let v = super::language::get(w.conn(), project, key).map_err(internal)?;
        return Ok(shown(v.as_deref()));
    }
    let value = super::project::hosts_value(str_arg(args, "value")?).map_err(usage)?;
    w.append_event(&NewEvent {
        kind: "config_set",
        actor: Some(actor),
        payload: Some(&json!({"project": project, "key": key, "value": value})),
        ..Default::default()
    })
    .map_err(internal)?;
    let mut v = shown(Some(&value));
    v["applies"] = json!(
        "at the next launch of each worker of the project (makiba down, then makiba up, relaunches running ones)"
    );
    Ok(v)
}

// ---- the acceptance gate (Issue 47) ------------------------------------------------------------

/// What the gate reads from `state.db` for one PR.
struct GateRows {
    /// `(status, head_oid)` of the PR's latest `ci_completed`.
    latest_ci: Option<(String, String)>,
    /// The heads with a failed `ci_completed` for the PR.
    failed: std::collections::BTreeSet<String>,
    pin: Option<super::gate::Pin>,
    projects: Vec<(String, Option<String>)>,
    task: Option<String>,
    review: ReviewRows,
    /// The other open pull requests of the repository (Issue 331), as `prwatch::followed` reads
    /// them; read again when the verdict is recorded, a difference there is "ask again".
    others: Vec<OpenPr>,
    /// The merge orders the person set on the PR (Issue 338); the copy read again inside the
    /// verdict's transaction is the one judged.
    orders: Vec<super::gate::After>,
}

/// One other open pull request of the repository (Issue 331, merge-order.md 3.3): its task,
/// number and generations, so that a registration, a close or a reopen while the gate ran is seen.
#[derive(Clone, Debug, PartialEq)]
struct OpenPr {
    task: String,
    pr: i64,
    gen_reg: i64,
    gen_close: i64,
}

/// What the gate read of the PR's task's reviews (Issue 263): the kind of its newest launch and
/// its worker's newest diff review row (`commands.id`, the answer), and its newest passed diff
/// review row that names a head (Issue 557: a round is passed only when its reader read the change,
/// Issue 280; an attempt that read nothing, with or without a head, must not hide the head an
/// earlier review read). Read again when the verdict is recorded; a difference there is "ask
/// again".
#[derive(Clone, Default, PartialEq)]
struct ReviewRows {
    kind: Option<String>,
    row: Option<(i64, Value)>,
    read: Option<(i64, Value)>,
}

fn same_pr(p: &Value, repo: &str, pr: i64) -> bool {
    p["pr"].as_i64() == Some(pr)
        && p["repo"]
            .as_str()
            .is_some_and(|r| r.eq_ignore_ascii_case(repo))
}

fn gate_rows(w: &StateWriter, repo: &str, pr: i64) -> rusqlite::Result<GateRows> {
    let c = w.conn();
    let ci = ci_rows(c, repo, pr)?;
    let payloads = |kind: &str| -> rusqlite::Result<Vec<(i64, Value)>> {
        let mut st =
            c.prepare("SELECT id, payload_json FROM events WHERE kind = ?1 ORDER BY id")?;
        let rows = st.query_map([kind], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, text) = row?;
            if let Some(v) = text.and_then(|t| serde_json::from_str::<Value>(&t).ok())
                && same_pr(&v, repo, pr)
            {
                out.push((id, v));
            }
        }
        Ok(out)
    };
    let text = |v: &Value, k: &str| v[k].as_str().unwrap_or_default().to_string();
    let pin = payloads("acceptance_gate")?
        .into_iter()
        .find(|(_, v)| v["manifest_digest"].is_string())
        .map(|(id, v)| super::gate::Pin {
            id,
            commit: text(&v, "manifest_commit"),
            paths: v["paths"].clone(),
            digest: text(&v, "manifest_digest"),
        });
    let projects = c
        .prepare("SELECT slug, origin_url FROM projects WHERE status = 'active' ORDER BY slug")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let task: Option<String> = c
        .query_row(
            "SELECT r.task_id FROM pr_watches p JOIN runs r ON r.id = p.run_id
             WHERE lower(p.repo) = lower(?1) AND p.pr = ?2 ORDER BY p.id DESC LIMIT 1",
            params![repo, pr],
            |r| r.get(0),
        )
        .optional()?;
    let mut review = ReviewRows::default();
    if let Some(t) = &task {
        review.kind = c
            .query_row(
                "SELECT l.kind FROM runs r JOIN run_launch l ON l.run_id = r.id
                 WHERE r.task_id = ?1 ORDER BY l.id DESC LIMIT 1",
                [t],
                |r| r.get(0),
            )
            .optional()?;
        let newest = |headed: &str| -> rusqlite::Result<Option<(i64, Value)>> {
            let row: Option<(i64, Option<String>)> = c
                .query_row(
                    &format!(
                        "SELECT id, json_extract(result_json, '$.result') FROM commands
                         WHERE verb = 'review' AND actor = ?1 AND status = 'done'
                           AND json_extract(result_json, '$.result.kind') = 'diff'{headed}
                         ORDER BY id DESC LIMIT 1"
                    ),
                    [crate::spec::worker_agent_id(t)],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            Ok(row.and_then(|(id, t)| Some((id, serde_json::from_str(&t?).ok()?))))
        };
        review.row = newest("")?;
        review.read = newest(
            " AND json_extract(result_json, '$.result.verdict') = 'passed'
              AND json_type(result_json, '$.result.head') = 'text'",
        )?;
    }
    // Issue 331: the other pull requests makibad follows in this repository, not read closed.
    let others = super::prwatch::followed(c, None)
        .map_err(|e| match e {
            crate::db::Error::Sqlite(e) => e,
            other => rusqlite::Error::InvalidParameterName(other.to_string()),
        })?
        .into_iter()
        .filter(|f| f.repo.eq_ignore_ascii_case(repo) && f.pr != pr && !f.closed)
        .map(|f| OpenPr {
            task: f.task,
            pr: f.pr,
            gen_reg: f.gen_reg,
            gen_close: f.gen_close,
        })
        .collect();
    // Issue 338: the merge orders the person set on this pull request, from the ledger only.
    let orders = super::gate::person_orders(c, repo, pr)?;
    Ok(GateRows {
        latest_ci: ci
            .last()
            .map(|(_, v)| (text(v, "status"), text(v, "head_oid"))),
        failed: failed_of(&ci),
        pin,
        projects,
        task,
        review,
        others,
        orders,
    })
}

/// The id of the newest record of `repo#pr` (`pr_opened` or `fix_pushed`, which every
/// `prwatch::register` writes, also when it takes up an older watch row again), if any.
fn newest_watch(w: &StateWriter, repo: &str, pr: i64) -> rusqlite::Result<Option<i64>> {
    w.conn().query_row(
        "SELECT max(id) FROM events WHERE kind IN ('pr_opened', 'fix_pushed')
           AND lower(json_extract(payload_json, '$.repo')) = lower(?1)
           AND json_extract(payload_json, '$.pr') = ?2",
        params![repo, pr],
        |r| r.get(0),
    )
}

/// The head of the newest record of `repo#pr` (`pr_opened` or `fix_pushed`), if any.
fn newest_head(w: &StateWriter, repo: &str, pr: i64) -> rusqlite::Result<Option<String>> {
    w.conn()
        .query_row(
            "SELECT json_extract(payload_json, '$.head_oid') FROM events
             WHERE kind IN ('pr_opened', 'fix_pushed')
               AND lower(json_extract(payload_json, '$.repo')) = lower(?1)
               AND json_extract(payload_json, '$.pr') = ?2
             ORDER BY id DESC LIMIT 1",
            params![repo, pr],
            |r| r.get(0),
        )
        .optional()
        .map(Option::flatten)
}

/// The heads of the failed `ci_completed` events among `ci` (one pull request's).
fn failed_of(ci: &[(i64, Value)]) -> std::collections::BTreeSet<String> {
    ci.iter()
        .filter(|(_, v)| v["status"] == json!("failed"))
        .filter_map(|(_, v)| v["head_oid"].as_str().map(String::from))
        .collect()
}

/// The `ci_completed` payloads of `repo#pr`, oldest first.
fn ci_rows(c: &Connection, repo: &str, pr: i64) -> rusqlite::Result<Vec<(i64, Value)>> {
    let mut st =
        c.prepare("SELECT id, payload_json FROM events WHERE kind = 'ci_completed' ORDER BY id")?;
    let rows = st.query_map([], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, text) = row?;
        if let Some(v) = text.and_then(|t| serde_json::from_str::<Value>(&t).ok())
            && same_pr(&v, repo, pr)
        {
            out.push((id, v));
        }
    }
    Ok(out)
}

/// The heads with a failed `ci_completed` for `repo#pr`: what the merge gate takes as the failed
/// run at a test commit (`GateRows::failed`), and what Issue 307's check before CI reads.
pub(super) fn failed_heads(
    c: &Connection,
    repo: &str,
    pr: i64,
) -> rusqlite::Result<std::collections::BTreeSet<String>> {
    Ok(failed_of(&ci_rows(c, repo, pr)?))
}

/// Issue 307, before `pr opened|pushed` is recorded (outside the door's lock: it reads GitHub and
/// git). Let through only what the merge gate can never pass: a repository that is no project's,
/// or a project clone whose origin does not name it (the gate fetches only through such an
/// origin). Otherwise the pull request is read and fetched as the gate does, and
/// `gate::before_ci` judges the head against the failed runs recorded for this pull request; what
/// cannot be read refuses. Let through, the answer says whether `head` is a clean merge of the
/// base onto `prev`, the head recorded last (Issue 358); unchecked, it is not.
fn pr_before_ci(
    projects: &[(String, Option<String>)],
    failed: &std::collections::BTreeSet<String>,
    repo: &str,
    pr: i64,
    head: &str,
    prev: Option<&str>,
) -> Result<bool, String> {
    use super::gate;
    let repos = super::prepare::Places::from_env(Path::new("")).repos;
    let Some(clone) = gate::clone_of(projects, &repos, &repo.to_lowercase()) else {
        return Ok(false);
    };
    if gate::origin_of(&clone, repo).is_err() {
        return Ok(false);
    }
    let why = |e: gate::Reject| {
        format!(
            "{repo}#{pr} cannot be checked before CI (Issue 307): {}; nothing was recorded",
            e.0
        )
    };
    let program = super::gh::program();
    let (_url, base_branch, _) = gate::pr_view(&program, repo, pr).map_err(why)?;
    let (base, fetched) = gate::fetch(&clone, repo, pr, &base_branch).map_err(why)?;
    if fetched != head {
        return Err(format!(
            "{repo}#{pr} has the head {fetched} at origin, not {head}; record the head the pull request has (once makibad's push notice names it)"
        ));
    }
    gate::before_ci(
        &clone,
        &base,
        head,
        &|sha| failed.contains(sha),
        &format!("for {repo}#{pr}"),
    )
    .map_err(|e| e.0)?;
    Ok(prev.is_some_and(|p| p != head && gate::pushed_base_merge(&clone, &base, p, head)))
}

/// `(repo, pr, head)` of a `gate` request.
/// The argument `repo` as the gate takes it: `owner/name`, each part letters, digits, `.`, `_`
/// or `-` (shared by `merge` action `order`, Issue 338).
fn repo_arg(args: &Value) -> Result<&str, DoorError> {
    let repo = str_arg(args, "repo")?;
    let ok = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    };
    if !repo.split_once('/').is_some_and(|(o, n)| ok(o) && ok(n)) {
        return Err(usage(format!("repo is owner/name, not {repo:?}")));
    }
    Ok(repo)
}

fn gate_args(args: &Value) -> Result<(String, i64, String), DoorError> {
    let repo = repo_arg(args)?;
    let pr = args["pr"]
        .as_i64()
        .filter(|n| *n > 0)
        .ok_or_else(|| usage("the argument pr (a number) is missing"))?;
    let head = str_arg(args, "head")?;
    if head.len() != 40
        || !head
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(usage(
            "head is the full 40-character commit id, in lowercase",
        ));
    }
    Ok((repo.to_string(), pr, head.to_string()))
}

/// What the gate found outside the door's lock: refused before the git checks, a read of GitHub
/// that still failed after its tries (Issue 512: nothing about the pull request), or the verdict.
enum Found {
    Before(String),
    Unavailable(String),
    Ran(Box<super::gate::Verdict>, bool, Value),
}

/// The review state of the PR's head `head` (Issue 263), from the task's last diff review: `state`
/// is `passed`, `blockers` (passed, with blockers found: not a pass), `none`, `stale`,
/// `not-established` or `not-required` (only Markdown or `docs/` changed). A review of another
/// head is carried to this one only when the PR's own changes are byte-equal (a join commit of
/// the base, a rebase); otherwise it is stale. Issue 557: a docs-only PR needs no review, but when
/// its task's worker ran a passed diff review (one whose reader read the change) of a head, a head
/// after it is held the same way (stale unless carried), whatever that review found.
fn gate_review(rows: &GateRows, clone: &Path, base: &str, head: &str, docs: bool) -> Value {
    let mut r = json!({"state": "not-established", "head": head});
    if docs {
        r["state"] = json!("not-required");
        if rows.task.is_some()
            && let Some((_, l)) = &rows.review.read
        {
            r["reviewed_head"] = l["head"].clone();
            if let Some(rh) = l["head"].as_str()
                && rh != head
            {
                carry(&mut r, clone, base, rh, head, true);
            }
        }
        return r;
    }
    let rv = &rows.review;
    if rows.task.is_none() {
        r["reason"] = json!("no task of makiba is tied to this PR by a watch");
        return r;
    }
    match rv.kind.as_deref() {
        None => {
            r["reason"] = json!("the PR's task has no launched run");
            return r;
        }
        Some("code") => {}
        Some(_) => {
            r["reason"] =
                json!("the PR's task is not a code task, and the PR changes more than docs");
            return r;
        }
    }
    let Some((_, l)) = &rv.row else {
        r["state"] = json!("none");
        return r;
    };
    r["reviewed_head"] = l["head"].clone();
    if let Some(rh) = l["head"].as_str()
        && rh != head
    {
        carry(
            &mut r,
            clone,
            base,
            rh,
            head,
            l["verdict"] == json!("passed"),
        );
    }
    if r["state"] == json!("stale") {
        return r;
    }
    for k in ["reader", "round", "allowed", "findings", "reason"] {
        if !l[k].is_null() {
            r[k] = l[k].clone();
        }
    }
    r["state"] = json!(match l["verdict"].as_str() {
        Some("passed") if l["findings"]["blocker"].as_i64().unwrap_or(0) > 0 => "blockers",
        Some("passed") => "passed",
        _ => "not-established",
    });
    r
}

/// Issue 344: a review of `rh` is carried to `head` only over clean merges of the base that keep
/// the own changes (and, for code, only a passed one: `passed`). Not carried, `r` becomes `stale`
/// and names the commits no review read; `same_changes` says which.
fn carry(r: &mut Value, clone: &Path, base: &str, rh: &str, head: &str, passed: bool) {
    let after = super::gate::after_review(clone, base, rh, head);
    let merges_only = matches!(&after, Ok((_, not_merges)) if not_merges.is_empty());
    let same = merges_only && passed && super::gate::same_changes(clone, base, rh, head);
    if !same {
        r["state"] = json!("stale");
        match after {
            Ok((all, not_merges)) => {
                let unread = if not_merges.is_empty() {
                    all
                } else {
                    not_merges
                };
                r["unread"] = json!(unread);
            }
            Err(e) => r["unread_error"] = json!(e.0),
        }
    }
    r["same_changes"] = json!(same);
}

/// The line the gate's answer carries for a review state.
fn gate_review_line(r: &Value) -> String {
    let short = |v: &Value| {
        v.as_str()
            .map(|s| s[..s.len().min(12)].to_string())
            .unwrap_or_default()
    };
    let head = short(&r["head"]);
    match r["state"].as_str().unwrap_or_default() {
        "blockers" => format!(
            "review of {head}: passed by {} but with {} blocker(s) found",
            r["reader"].as_str().unwrap_or("?"),
            r["findings"]["blocker"]
        ),
        "stale" => {
            let unread: Vec<String> = r["unread"]
                .as_array()
                .map(|a| a.iter().map(short).collect())
                .unwrap_or_default();
            let named = match unread.len() {
                0 => "unknown".to_string(),
                n if n > 20 => format!("{}, and {} more", unread[..20].join(", "), n - 20),
                _ => unread.join(", "),
            };
            format!(
                "review of {head}: stale (the last diff review read {}; commits no diff review read: {named})",
                short(&r["reviewed_head"])
            )
        }
        "not-required" => {
            format!("review of {head}: not required (only Markdown or docs/ changed)")
        }
        "passed" if r["same_changes"] == json!(true) => format!(
            "{} (read {}, same changes)",
            review_line(r),
            short(&r["reviewed_head"])
        ),
        _ => review_line(r),
    }
}

/// The line of the merge order (Issue 331): the check paths, the open pull requests that hold
/// the pull request and those that could not be read.
fn merge_order_line(repo: &str, pr: i64, m: &super::gate::MergeOrder) -> String {
    let paths = if m.paths.len() > 10 {
        format!(
            "{}, and {} more",
            m.paths[..10].join(", "),
            m.paths.len() - 10
        )
    } else {
        m.paths.join(", ")
    };
    let mut line = format!(
        "{repo}#{pr} changes {paths} (it may add or tighten a check over tests/); open pull requests have test commits whose declared tests cannot change"
    );
    let waiting: Vec<String> = m
        .waits_for
        .iter()
        .map(|w| {
            format!(
                "#{} (task {}, test commit {}: {})",
                w.pr,
                w.task,
                &w.test_commit[..w.test_commit.len().min(7)],
                w.declared.join(", ")
            )
        })
        .collect();
    if !waiting.is_empty() {
        line = format!("{line}: {}", waiting.join(", "));
    }
    if !m.not_read.is_empty() {
        let unread: Vec<String> = m
            .not_read
            .iter()
            .map(|(n, t, why)| format!("#{n} (task {t}: {why})"))
            .collect();
        line = format!(
            "{line}; not read, so held as if waiting: {}",
            unread.join(", ")
        );
    }
    let mutual: Vec<String> = m
        .waits_for
        .iter()
        .filter(|w| w.check_adding)
        .map(|w| format!("#{}", w.pr))
        .collect();
    if !mutual.is_empty() {
        line = format!(
            "{line}; {} may add a check too: the two hold each other, and the person decides their order",
            mutual.join(", ")
        );
    }
    line
}

/// The gate's reads of GitHub and git: no lock is held while they run.
fn gate_outside(rows: &GateRows, repo: &str, pr: i64, head: &str) -> Found {
    use super::gate;
    let program = super::gh::program();
    let (_url, base_branch, view_head) = match gate::pr_view_read(&program, repo, pr) {
        Ok(v) => v,
        Err(gate::Fault::Refused(why)) => return Found::Before(why),
        Err(gate::Fault::Unavailable(why)) => return Found::Unavailable(why),
    };
    if view_head != head {
        return Found::Before(format!(
            "the PR head is {}, not {}; wait for CI on the current head",
            &view_head[..view_head.len().min(12)],
            &head[..12]
        ));
    }
    if rows
        .latest_ci
        .as_ref()
        .map(|(s, h)| (s.as_str(), h.as_str()))
        != Some(("passed", head))
    {
        let latest = rows
            .latest_ci
            .as_ref()
            .map_or("none".to_string(), |(s, h)| {
                format!("{s} at {}", &h[..h.len().min(12)])
            });
        return Found::Before(format!(
            "no passed ci_completed for {repo}#{pr} at {} (latest: {latest}); record the head with the pr tool and wait for the CI notice. A repository without CI has no passed run: the person merges it in the GitHub web UI",
            &head[..12]
        ));
    }
    let repos = super::prepare::Places::from_env(Path::new("")).repos;
    let Some(clone) = gate::clone_of(&rows.projects, &repos, &repo.to_lowercase()) else {
        return Found::Before(format!(
            "no project's repository in {} is {repo}",
            repos.display()
        ));
    };
    let (base, fetched) = match gate::fetch_read(&clone, repo, pr, &base_branch) {
        Ok(ids) => ids,
        Err(gate::Fault::Refused(why)) => return Found::Before(why),
        Err(gate::Fault::Unavailable(why)) => return Found::Unavailable(why),
    };
    if fetched != head {
        return Found::Before(format!(
            "the PR head moved while fetching ({} -> {}); ask again",
            &head[..12],
            &fetched[..fetched.len().min(12)]
        ));
    }
    let mut v = gate::Verdict {
        base: base.clone(),
        head: head.to_string(),
        ..Default::default()
    };
    let mut ci = gate::Ci::new(&program);
    let failed = |sha: &str| Ok(rows.failed.contains(sha));
    if let Err(e) = gate::evaluate(
        &clone,
        &base,
        head,
        repo,
        rows.pin.as_ref(),
        &mut ci,
        &failed,
        &mut v,
    ) {
        // A read of GitHub that still failed is the cause of the error: not a refusal (Issue 512).
        // The verdict goes on, so that the test commit found is recorded (the pin) and the rules
        // on the content still apply; `judge` answers `unavailable` only if none refuses.
        v.result = if ci.unavailable().is_some() {
            "unavailable"
        } else {
            "rejected"
        }
        .into();
        v.reason = e.0;
    }
    // The tests edited after the test commit (Issue 282). A comparison that cannot be made is
    // never read as "none edited".
    match gate::edited_tests(&clone, &base, head, v.manifest_commit.as_deref(), &v.paths) {
        Ok(edited) => v.edited_tests = edited,
        Err(e) if v.result != "rejected" => {
            v.result = "rejected".into();
            v.reason = format!(
                "the tests at head could not be compared with those before the test commit: {}",
                e.0
            );
        }
        Err(_) => {}
    }
    // Issue 513: helpers changed after the test commit and helpers the base changed after it,
    // with the diff stat of each and of the declared paths the base merged in. A comparison that
    // cannot be made is never read as "none".
    if let Some(m) = v.manifest_commit.clone() {
        let read = gate::helper_changes(&clone, &base, head, &m, &v.paths).and_then(|(c, b)| {
            let own: Vec<&gate::Change> = c.iter().chain(v.merged_tests.iter()).collect();
            let based: Vec<&gate::Change> = b.iter().collect();
            let mut stats = gate::diff_stat(&clone, &m, head, &own)?;
            stats.extend(gate::diff_stat(&clone, &format!("{m}^"), &base, &based)?);
            Ok((c, b, stats))
        });
        match read {
            Ok((c, b, stats)) => {
                v.helpers_changed = c;
                v.base_helpers = b;
                v.helper_stats = stats;
            }
            Err(e) if v.result != "rejected" => {
                v.result = "rejected".into();
                v.reason = format!(
                    "the test helpers at head and at the base could not be compared with those of the test commit: {}",
                    e.0
                );
            }
            Err(_) => {}
        }
    }
    // The merge order (Issue 331): read here, with no lock held, as every fetch of the gate. A
    // read that fails is never read as "not check-adding".
    let others: Vec<(String, i64)> = rows.others.iter().map(|o| (o.task.clone(), o.pr)).collect();
    let mut seen = std::collections::BTreeSet::new();
    let others: Vec<(String, i64)> = others
        .into_iter()
        .filter(|(_, n)| seen.insert(*n))
        .collect();
    match gate::merge_order(&program, &clone, repo, &base, &base_branch, head, &others) {
        // A pull request that could not be read from GitHub (Issue 512) is kept in the order:
        // `judge` decides what it means once every rule on the content has been applied.
        Ok(m) => v.merge_order = m,
        Err(e) if v.result != "rejected" => {
            v.result = "rejected".into();
            v.reason = format!(
                "the gate could not read whether this pull request may add a check over tests/: {}",
                e.0
            );
        }
        Err(_) => {}
    }
    // The language check (Issue 339): a read that fails is never read as "no change".
    match gate::language_change(&clone, &base, head) {
        Ok(l) => v.language_change = l,
        Err(e) if v.result != "rejected" => {
            v.result = "rejected".into();
            v.reason = format!(
                "the gate could not read whether this pull request changes the language check: {}",
                e.0
            );
        }
        Err(_) => {}
    }
    let docs = gate::docs_only(&clone, &base, head);
    let review = gate_review(rows, &clone, &base, head, docs);
    let docs_pass = v.result == "no_manifest" && docs;
    Found::Ran(Box::new(v), docs_pass, review)
}

impl Door {
    /// `gate` (spec section 12): the acceptance gate for one PR at one head. GitHub and git are
    /// read with the door's lock released; the rows it judged from are read again, and its verdict
    /// recorded, in one short transaction.
    fn gate(
        &self,
        w: MutexGuard<'_, StateWriter>,
        actor: &str,
        args: &Value,
        idem: Option<&str>,
    ) -> Response {
        let fail = |w: &StateWriter, r: Response| {
            let _ = record(w, actor, "gate", args, idem, &r);
            r
        };
        let (repo, pr, head) = match gate_args(args) {
            Ok(a) => a,
            Err(e) => return fail(&w, Err(e)),
        };
        let rows = match gate_rows(&w, &repo, pr) {
            Ok(r) => r,
            Err(e) => return fail(&w, Err(internal(e))),
        };
        drop(w);
        let found = gate_outside(&rows, &repo, pr, &head);
        let w = self.writer();
        if let Err(e) = begin_as(&w, "door gate") {
            return Err(internal(e));
        }
        let judged = match self.judge(&w, actor, &repo, pr, &head, &rows, found, "gate") {
            Ok(j) => j,
            Err(e) => return fail(&w, Err(e)),
        };
        let r: Response = Ok(judged.answer);
        if !record(&w, actor, "gate", args, idem, &r) || w.conn().is_autocommit() {
            let _ = w.rollback();
            return Err(internal(
                "the request could not be recorded; nothing was done",
            ));
        }
        if let Err(e) = w.commit() {
            let _ = w.rollback();
            return Err(internal(e));
        }
        r
    }

    /// The gate's judgment (Issue 431: shared by `gate` and `merge`), inside the transaction the
    /// caller began with `BEGIN IMMEDIATE`: the rows it judged from read again, the verdict, the
    /// `acceptance_gate` row (with `via`, the verb that judged), the belt's stop and fill, and the
    /// withdrawal of an older stop. On an error the transaction is rolled back and nothing stands.
    #[allow(clippy::too_many_arguments)]
    fn judge(
        &self,
        w: &StateWriter,
        actor: &str,
        repo: &str,
        pr: i64,
        head: &str,
        rows: &GateRows,
        found: Found,
        via: &str,
    ) -> Result<Judged, DoorError> {
        let (repo, head) = (repo.to_string(), head.to_string());
        let now = match gate_rows(w, &repo, pr) {
            Ok(r) => r,
            Err(e) => {
                let _ = w.rollback();
                return Err(internal(e));
            }
        };
        let mode = super::gate::merge_approval_mode();
        let mut review = None;
        let mut warn = false;
        // Each WARNING the gate adds, as its own line (Issue 432: a pending merge entry shows them).
        let mut warnings: Vec<String> = Vec::new();
        let (mut verdict, reason, mut allow, event) = match found {
            Found::Before(why) => ("rejected".to_string(), why, false, None),
            Found::Unavailable(why) => (
                "unavailable".to_string(),
                format!("GitHub could not be read: {why}"),
                false,
                None,
            ),
            Found::Ran(mut v, docs, rv) => {
                // What changed while the gate ran: a new CI result, or a first pin of another test.
                if now.latest_ci != rows.latest_ci {
                    v.result = "rejected".into();
                    v.reason = "the PR's CI result changed while the gate ran; ask again".into();
                } else if rows.pin.is_none()
                    && let Some(p) = &now.pin
                    && Some(&p.digest) != v.manifest_digest.as_ref()
                {
                    v.result = "rejected".into();
                    v.reason = "another verdict pinned a different test commit while the gate ran - ask the person".into();
                } else if now.task != rows.task || now.review != rows.review {
                    v.result = "rejected".into();
                    v.reason =
                        "the PR's task or its reviews changed while the gate ran; ask again".into();
                } else if now.others != rows.others {
                    v.result = "rejected".into();
                    v.reason = "the open pull requests of the repository changed while the gate ran; ask again".into();
                }
                // A CI read that failed (Issue 512) is held as allowed while the rules on the
                // content run; if none refuses, it becomes `unavailable` below.
                let allow = v.result == "passed"
                    || v.result == "unavailable"
                    || (v.result == "no_manifest" && (mode == "human" || docs));
                let reason = if v.result == "no_manifest" && !allow {
                    format!(
                        "{}: a PR without a declared acceptance test needs the person under merge_approval_mode acceptance (only a docs-only change passes); the person merges it in the GitHub web UI",
                        v.reason
                    )
                } else {
                    v.reason.clone()
                };
                review = Some(rv);
                (v.result.clone(), reason, allow, Some(v))
            }
        };
        // Issue 263: a code PR whose head has no passed, blocker-free diff review. Under
        // acceptance no person is asked, so it is refused; under human the person decides and the
        // hook asks them. An earlier refusal keeps its own verdict; the review line is appended.
        let mut reason = reason;
        if let Some(rv) = &review {
            let line = gate_review_line(rv);
            let state = rv["state"].as_str().unwrap_or_default();
            let bad = ["none", "stale", "not-established", "blockers"].contains(&state);
            // Issue 344: a head the last diff review did not read is refused under either mode.
            if allow && state == "stale" {
                allow = false;
                verdict = "review_missing".into();
                reason = format!(
                    "{line}; an unreviewed head is not merged under either merge_approval_mode: have the worker run a diff review round at this head (a worker out of rounds needs the secretary to raise its allowance with mcp__makiba__review task, allowed and reason) ({reason})"
                );
            } else if allow && bad && mode == "acceptance" {
                allow = false;
                verdict = "review_missing".into();
                reason = format!(
                    "{line}; merge_approval_mode acceptance lets no code pull request land without a passed diff review of this head: the person merges it in the GitHub web UI"
                );
            } else if allow && bad {
                warn = true;
                warnings.push(line.clone());
                reason = format!("{reason}; WARNING {line}: the person decides this landing");
            } else {
                reason = format!("{reason}; {line}");
            }
        }
        // Issue 282: a test that was there before the test commit, is not declared and differs at
        // head (belt.md section 5, line 5). Under acceptance no person is asked, so it is refused;
        // under human the hook asks them. An earlier refusal keeps its verdict and names the files.
        let edited: Vec<String> = event
            .as_ref()
            .map(|v| v.edited_tests.clone())
            .unwrap_or_default();
        if !edited.is_empty() {
            let line = format!(
                "tests edited after the test commit and not declared: {}",
                edited.join(", ")
            );
            if allow && mode == "acceptance" {
                allow = false;
                verdict = "tests_edited".into();
                reason = format!(
                    "{line}; merge_approval_mode acceptance lets no pull request that edits an existing test land without the person: the person merges it in the GitHub web UI ({reason})"
                );
            } else if allow {
                warn = true;
                warnings.push(line.clone());
                reason = format!("{reason}; WARNING {line}: the person decides this landing");
            } else {
                reason = format!("{reason}; {line}");
            }
        }
        // Issue 513 (docs/design/helper-fix.md, option D): a helper changed after the test commit,
        // a declared path the base merged in cleanly, or a helper the base changed after the test
        // commit needs the person's key under either mode. Each line names the object ids it was
        // judged from, so an approval shown other contents is refused (`shown_warnings`).
        let lists = event.as_ref().map(|v| helper_lines(v)).unwrap_or_default();
        for line in &lists {
            if allow {
                warn = true;
                warnings.push(line.clone());
                reason = format!("{reason}; WARNING {line}: the person decides this landing");
            } else {
                reason = format!("{reason}; {line}");
            }
        }
        let stats: Vec<String> = event
            .as_ref()
            .filter(|_| !lists.is_empty())
            .map(|v| v.helper_stats.clone())
            .unwrap_or_default();
        // Issue 331 (docs/design/merge-order.md 3.3): a pull request that may add or tighten a
        // check over tests/ waits while other open pull requests have test commits, whose declared
        // tests cannot change. Under acceptance no person is asked, so it is refused; under human
        // the person decides. An earlier refusal keeps its verdict and has the line appended.
        let mut order = event
            .as_ref()
            .map(|v| v.merge_order.clone())
            .unwrap_or_default();
        // Issue 338: the orders the person set, as read inside this transaction, whose pull
        // request or Issue has not landed.
        order.after = now.orders.iter().filter(|o| !o.landed).cloned().collect();
        // An order not known because GitHub could not be read (Issue 512), decided last.
        let mut order_unread: Option<String> = None;
        if order.holds() {
            let line = merge_order_line(&repo, pr, &order);
            // Issue 512: with nothing known to hold it, an order that could not be read from
            // GitHub is not known; after every rule on the content, that is all that stands.
            if allow && let Some(why) = order.not_known() {
                // Decided after the last rule on the content, below; the line names it meanwhile.
                order_unread = Some(why);
                reason = format!("{reason}; {line}");
            } else if allow && mode == "acceptance" {
                allow = false;
                verdict = "merge_order".into();
                reason = format!(
                    "{line}; it waits until they land or are closed, or the person decides: the person merges it in the GitHub web UI ({reason})"
                );
            } else if allow {
                warn = true;
                warnings.push(line.clone());
                reason = format!("{reason}; WARNING {line}: the person decides this landing");
            } else {
                reason = format!("{reason}; {line}");
            }
        }
        // Issue 338 (docs/design/merge-order.md section 6): an order the person set holds the pull
        // request until its pull request or Issue has landed, under either mode: the person
        // decided already, so neither a later secretary nor an approval with the key passes it.
        // An earlier refusal keeps its verdict and has the line appended.
        if order.person_holds() {
            let clauses: Vec<String> = order
                .after
                .iter()
                .map(|o| {
                    format!(
                        "{repo}#{pr} merges only after {} has landed (order {}, recorded by {}: {})",
                        o.named(),
                        o.event_id,
                        o.by,
                        o.reason
                    )
                })
                .collect();
            let line = format!("the person set a merge order: {}", clauses.join("; "));
            if allow {
                allow = false;
                verdict = "merge_order".into();
                reason = format!(
                    "{line}; it has not landed, so this one waits; no check makes an exception: if the person decides otherwise, they merge it in the GitHub web UI ({reason})"
                );
            } else {
                reason = format!("{reason}; {line}");
            }
        }
        // Issue 339: a change to the language check lands, under either mode, only with the
        // person's own yes to the question naming each file at head, the newest answer of that
        // question on the run deciding; a file that is not a regular one is never approved. An
        // earlier refusal keeps its verdict and has the line appended.
        let language: Vec<super::gate::LanguageEntry> = event
            .as_ref()
            .map(|v| v.language_change.clone())
            .unwrap_or_default();
        let mut language_q: Option<String> = None;
        let mut language_ok: Option<i64> = None;
        if !language.is_empty() {
            let paths: Vec<&str> = language.iter().map(|e| e.path.as_str()).collect();
            let line = if language.iter().all(|e| e.approvable()) {
                let q = language_question(&repo, pr, &language);
                language_ok = match language_approval(w, now.task.as_deref(), &q) {
                    Ok(a) => a,
                    Err(e) => {
                        let _ = w.rollback();
                        return Err(internal(e));
                    }
                };
                let line = format!(
                    "the pull request changes the language check ({}) and the person has not approved it: the person answers yes to the question {q:?} with their own makiba answer <id> yes (a range task's stop asks it; else add it with mcp__makiba__pending for the task and escalate it), then ask merge again",
                    paths.join(", ")
                );
                language_q = Some(q);
                line
            } else {
                format!(
                    "the pull request changes the language check ({}) and one of them at head is not a regular file, which no answer approves: the person lands it in the GitHub web UI",
                    paths.join(", ")
                )
            };
            if language_ok.is_none() {
                if allow {
                    allow = false;
                    verdict = "language_check".into();
                    reason = format!("{line} ({reason})");
                } else {
                    reason = format!("{reason}; {line}");
                }
            }
        }
        // Issue 512: a read of GitHub that still failed (a CI read, or an open pull request this
        // one must be ordered after), with no rule on the content refusing: the acceptance or the
        // order is not known, and that is all that stands.
        if allow && let Some(why) = order_unread {
            allow = false;
            verdict = "unavailable".into();
            reason = format!(
                "GitHub could not be read: the gate could not read an open pull request it must order this one after: {why} ({reason})"
            );
        } else if allow && verdict == "unavailable" {
            allow = false;
            reason = format!("GitHub could not be read: {reason}");
        }
        // Issue 282: a range task's pull request lands without the person only when the range was
        // approved under acceptance and the mode read now is acceptance too. The checks above are
        // those of the mode read now; a pair that is not both only adds the question to the person.
        let range = match now.task.as_deref().map(|t| super::belt::approval_of(w, t)) {
            None => None,
            Some(Ok(r)) => r,
            Some(Err(e)) => {
                let _ = w.rollback();
                return Err(internal(e));
            }
        };
        if let Some((id, range_mode)) = &range {
            let approved = range_mode.as_deref().unwrap_or("human");
            if allow && !(approved == "acceptance" && mode == "acceptance") {
                warn = true;
                warnings.push(format!(
                    "range {id} was approved under merge_approval_mode {approved} and the mode now is {mode}"
                ));
                reason = format!(
                    "{reason}; WARNING range {id} was approved under merge_approval_mode {approved} and the mode now is {mode}: the person decides this landing"
                );
            }
        }
        // Every answer is a record: a refusal decided before the git checks too (no base, no
        // manifest: it is never the PR's pin).
        let mut payload = match &event {
            Some(v) => json!({
                "repo": repo, "pr": pr, "base": v.base, "head": v.head, "result": v.result,
                "reason": v.reason, "mode": mode, "allow": allow,
            }),
            None => json!({
                "repo": repo, "pr": pr, "base": null, "head": head, "result": verdict,
                "reason": reason, "mode": mode, "allow": allow,
            }),
        };
        // The answer's verdict (Issue 512): `unavailable` is no refusal, and the pause of two
        // refusals in a row does not count it; `result` stays the acceptance result.
        payload["verdict"] = json!(verdict);
        if let Some(v) = &event {
            if let Some(m) = &v.manifest_commit {
                payload["manifest_commit"] = json!(m);
                payload["paths"] = json!(v.paths);
            }
            if let Some(d) = &v.manifest_digest {
                payload["manifest_digest"] = json!(d);
            }
        }
        if !edited.is_empty() {
            payload["edited_tests"] = json!(edited);
        }
        if !language.is_empty() {
            payload["language_check"] = json!({
                "paths": language.iter().map(|e| e.path.as_str()).collect::<Vec<_>>(),
                "question": language_q, "approved": language_ok,
            });
        }
        if let Some(v) = &event {
            for (key, list) in [
                ("helpers_changed", &v.helpers_changed),
                ("merged_tests", &v.merged_tests),
                ("base_helpers", &v.base_helpers),
            ] {
                if !list.is_empty() {
                    payload[key] = json!(list.iter().map(|c| &c.path).collect::<Vec<_>>());
                }
            }
        }
        if order.holds() || order.person_holds() {
            payload["merge_order"] = json!({
                "paths": order.paths,
                "waits_for": order.waits_for.iter().map(|w| json!({
                    "pr": w.pr, "task": w.task, "test_commit": w.test_commit,
                    "declared": w.declared, "check_adding": w.check_adding,
                })).collect::<Vec<_>>(),
                "not_read": order.not_read.iter().map(|(n, t, why)| json!({
                    "pr": n, "task": t, "why": why,
                })).collect::<Vec<_>>(),
            });
            if order.person_holds() {
                payload["merge_order"]["after"] = json!(
                    order
                        .after
                        .iter()
                        .map(|o| json!({
                            "event_id": o.event_id, "after": o.after, "number": o.number,
                            "reason": o.reason, "by": o.by,
                        }))
                        .collect::<Vec<_>>()
                );
            }
        }
        if let Some(t) = &now.task {
            payload["task"] = json!(t);
        }
        if let Some((id, range_mode)) = &range {
            payload["range_id"] = json!(id);
            payload["range_mode"] = json!(range_mode);
        }
        if let Some(rv) = &review {
            payload["review"] = rv.clone();
        }
        payload["via"] = json!(via);
        let written = w.append_event(&NewEvent {
            kind: "acceptance_gate",
            actor: Some(actor),
            payload: Some(&payload),
            run_task_id: now.task.as_deref(),
            ..Default::default()
        });
        let event_id = match written {
            Ok(id) => id,
            Err(e) => {
                let _ = w.rollback();
                return Err(internal(format!(
                    "the verdict (this PR's pin) could not be recorded: {e}"
                )));
            }
        };
        // The belt: a refusal, or edited tests, on a range task's pull request is a stop written
        // here, then the pause rule, then the fill (belt.md 3.4), all with the verdict's record.
        let mut pending_id = None;
        if let Some(t) = now.task.as_deref().filter(|_| range.is_some())
            && (!allow || !edited.is_empty() || !lists.is_empty())
        {
            let stop = super::belt::GateStop {
                task: t,
                repo: &repo,
                pr,
                head: &head,
                allow,
                refused: !allow && verdict != "tests_edited",
                unavailable: verdict == "unavailable",
                reason: &reason,
                edited_tests: &edited,
                helpers: &lists,
            };
            // A refusal for the language check asks the gate's own question (Issue 339), so the
            // person's answer to the stop is the approval.
            let asked = match language_q
                .as_deref()
                .filter(|_| verdict == "language_check")
            {
                Some(q) => super::belt::language_stop(w, &stop, q),
                None => super::belt::gate_stop(w, &stop),
            };
            let stopped = asked.and_then(|id| super::belt::fill(w, &self.agent).map(|_| id));
            match stopped {
                Ok(id) => pending_id = id,
                Err(e) => {
                    let _ = w.rollback();
                    return Err(internal(format!("the stop could not be recorded: {e}")));
                }
            }
        }
        // An allowance of a head refused before: its gate stop is withdrawn with this verdict, and
        // its tests stop too when no edited test is reported any more.
        if allow
            && range.is_some()
            && let Err(e) = super::belt::gate_allowed(
                w,
                &repo,
                pr,
                &head,
                edited.is_empty() && lists.is_empty(),
            )
        {
            let _ = w.rollback();
            return Err(internal(format!("the stop could not be withdrawn: {e}")));
        }
        let mut answer = json!({"verdict": verdict, "allow": allow, "reason": reason,
                                "mode": mode, "repo": repo, "pr": pr, "head": head});
        if let Some(rv) = &review {
            answer["review"] = rv.clone();
            answer["warn"] = json!(warn);
        } else if warn {
            answer["warn"] = json!(warn);
        }
        if let Some((_, range_mode)) = &range {
            answer["range_mode"] = json!(range_mode);
        }
        if let Some(id) = pending_id {
            answer["pending_id"] = json!(id);
        }
        Ok(Judged {
            answer,
            allow,
            warn,
            warnings,
            stats,
            verdict,
            reason,
            mode,
            review,
            event: event_id,
        })
    }
}

/// The question the person answers for a change to the language check of `repo`#`pr` (Issue
/// 339), word for word: each file of `language` with its mode and object id at head. The
/// repository is written in lower case, as the gate matches repositories in any case, so that
/// one approval holds however the repository was named.
fn language_question(repo: &str, pr: i64, language: &[super::gate::LanguageEntry]) -> String {
    let words: Vec<String> = language.iter().map(|e| e.words()).collect();
    format!(
        "Approve the change to the language check of {}#{pr}: {}",
        repo.to_lowercase(),
        words.join(", ")
    )
}

/// The pending entry that approves `question` on the run of `task` (Issue 339): the newest
/// answered entry of that question there decides, and it approves only when the person answered
/// it yes with their own key (a relayed answer is the secretary's).
fn language_approval(
    w: &StateWriter,
    task: Option<&str>,
    question: &str,
) -> rusqlite::Result<Option<i64>> {
    let Some(task) = task else {
        return Ok(None);
    };
    let newest: Option<(i64, Option<String>, Option<String>)> = w
        .conn()
        .query_row(
            "SELECT p.id, p.answer, p.answered_by FROM pending p JOIN runs r ON r.id = p.run_id
             WHERE r.task_id = ?1 AND p.kind = 'question' AND p.status = 'answered'
               AND p.question = ?2
             ORDER BY p.answered_at DESC, p.id DESC LIMIT 1",
            params![task, question],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    Ok(newest.and_then(|(id, answer, by)| {
        (answer.as_deref() == Some("yes") && by.as_deref() == Some(crate::spec::AGENT_PERSON))
            .then_some(id)
    }))
}

/// What `Door::judge` decided and recorded (the `acceptance_gate` row `event`), in the open
/// transaction.
struct Judged {
    /// The answer `gate` gives.
    answer: Value,
    allow: bool,
    warn: bool,
    /// Each warning of `warn`, one line each.
    warnings: Vec<String>,
    /// The `git diff --stat` lines of the paths of the Issue 513 warnings, for the entry's text.
    stats: Vec<String>,
    verdict: String,
    reason: String,
    mode: &'static str,
    review: Option<Value>,
    event: i64,
}

// ---- merge (Issue 431, docs/design/merge-act.md) ------------------------------------------------

/// The events that end an attempt of makibad's merge: its outcomes.
pub(super) const MERGE_OUTCOMES: &str =
    "'merge_done', 'merge_enqueued', 'merge_failed', 'merge_unknown'";

/// A merge act of makibad that awaits its landing (merge-act.md section 7): the newest attempt of
/// the pull request is open, or ended `merge_done` (or `merge_unknown` observed `merged`) with no
/// `pr_merged` of the pull request after the attempt; or it ended `merge_enqueued` (or
/// `merge_unknown` observed `enqueued`), a queue act, with no `merge_dequeued`, `pr_merged` or
/// `pr_closed` of the pull request after the attempt. The attempt's id is the boundary, so a
/// landing read before the outcome was written still ends the act.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Awaiting {
    pub attempt: i64,
    pub head: String,
    pub queue: bool,
}

/// The act of `repo#pr` awaiting its landing, if any: the one reader of the `merge` verb's checks
/// and of the `pr` verb's.
pub(super) fn act_awaiting(
    c: &Connection,
    repo: &str,
    pr: i64,
) -> rusqlite::Result<Option<Awaiting>> {
    act_awaiting_for(c, repo, pr, None)
}

/// `act_awaiting` for the run `run` (the PR watch's `followed`, Issue 434): only a `pr_merged` of
/// that run ends the act, since each run that follows the pull request records its own landing;
/// with `None`, a `pr_merged` of any run does.
pub(super) fn act_awaiting_for(
    c: &Connection,
    repo: &str,
    pr: i64,
    run: Option<i64>,
) -> rusqlite::Result<Option<Awaiting>> {
    let of_pr = "lower(json_extract(payload_json, '$.repo')) = lower(?1)
                 AND json_extract(payload_json, '$.pr') = ?2";
    let newest: Option<(i64, Option<String>)> = c
        .query_row(
            &format!(
                "SELECT id, json_extract(payload_json, '$.head') FROM events
                 WHERE kind = 'merge_attempted' AND {of_pr} ORDER BY id DESC LIMIT 1"
            ),
            params![repo, pr],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((attempt, head)) = newest else {
        return Ok(None);
    };
    let outcome: Option<(String, Option<String>)> = c
        .query_row(
            &format!(
                "SELECT kind, json_extract(payload_json, '$.observed') FROM events
                 WHERE kind IN ({MERGE_OUTCOMES}) AND json_extract(payload_json, '$.attempt') = ?1
                 ORDER BY id LIMIT 1"
            ),
            [attempt],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let queue = match outcome.as_ref().map(|(k, o)| (k.as_str(), o.as_deref())) {
        None | Some(("merge_done", _)) | Some(("merge_unknown", Some("merged"))) => false,
        Some(("merge_enqueued", _)) | Some(("merge_unknown", Some("enqueued"))) => true,
        Some(_) => return Ok(None),
    };
    let ends = if queue {
        "'pr_merged', 'pr_closed', 'merge_dequeued'"
    } else {
        "'pr_merged'"
    };
    let ended: bool = c.query_row(
        &format!(
            "SELECT EXISTS (SELECT 1 FROM events WHERE kind IN ({ends}) AND id > ?3 AND {of_pr}
               AND (kind != 'pr_merged' OR ?4 IS NULL OR run_id = ?4))"
        ),
        params![repo, pr, attempt, run],
        |r| r.get(0),
    )?;
    Ok((!ended).then(|| Awaiting {
        attempt,
        head: head.unwrap_or_default(),
        queue,
    }))
}

/// End the queue act `a` of `repo#pr` and tell the secretary, in the caller's transaction. `why`:
/// `head recorded` (another head of the pull request was recorded, by the `pr` verb or the
/// outcome writer), or, from the PR watch (Issue 434), `head moved` (GitHub shows another head),
/// `closed` (read closed) or `left the queue` (read out of the queue with a removal on its
/// timeline).
pub(super) fn merge_dequeued(
    w: &StateWriter,
    task: &str,
    repo: &str,
    pr: i64,
    a: &Awaiting,
    why: &str,
) -> crate::db::Result<()> {
    merge_dequeued_with(w, task, repo, pr, &a.head, Some(a.attempt), why, None)
}

/// End a stay of `repo#pr` in the merge queue at `head` and tell the secretary, in the caller's
/// transaction: makibad's queue act (`attempt` its id) or, with `attempt` `None`, a stay the PR
/// watch recorded for a pull request queued outside makibad (Issue 476, `merge_queue_entered`).
/// `reason` is GitHub's reason for the removal that ended it (`left the queue`), when read.
#[allow(clippy::too_many_arguments)]
pub(super) fn merge_dequeued_with(
    w: &StateWriter,
    task: &str,
    repo: &str,
    pr: i64,
    head: &str,
    attempt: Option<i64>,
    why: &str,
    reason: Option<&str>,
) -> crate::db::Result<()> {
    w.append_event(&NewEvent {
        kind: "merge_dequeued",
        actor: Some("makibad"),
        payload: Some(
            &json!({"repo": repo, "pr": pr, "head": head, "attempt": attempt,
                              "why": why, "reason": reason}),
        ),
        run_task_id: Some(task),
        ..Default::default()
    })?;
    let whose = if attempt.is_some() {
        "makibad's merge queue act for"
    } else {
        "The merge queue entry, queued outside makibad, of"
    };
    notice(
        w,
        crate::spec::AGENT_SECRETARY,
        task,
        &match why {
            "left the queue" => format!(
                "{repo}#{pr} at {head} left the merge queue without landing ({}); it is still open. {}. Read why on the pull request; fix the cause (for a conflict, update the branch and record the new head), then ask again with merge.",
                reason.map_or("GitHub gave no reason".to_string(), |r| format!(
                    "GitHub's reason: {r}"
                )),
                if attempt.is_some() {
                    "makibad had enqueued it"
                } else {
                    "It was queued outside makibad"
                }
            ),
            "head moved" => format!(
                "{whose} {repo}#{pr} at {head} has ended: GitHub shows another head of the pull request, and the queue entry was bound to the old one. Record the new head with makiba pr pushed; once CI passes at it, ask again with merge."
            ),
            "closed" => format!(
                "{whose} {repo}#{pr} at {head} has ended: the pull request was closed without landing."
            ),
            _ => format!(
                "{whose} {repo}#{pr} at {head} has ended: a new head of the pull request was recorded, and the queue entry was bound to the old one. Once CI passes at the new head, ask again with merge."
            ),
        },
    )?;
    Ok(())
}

/// The `merge` verb's check, before the gate and again in its final transaction: the pull request
/// is the current one of a run the PR watch follows (`prwatch::followed`), and it has no act
/// awaiting its landing. The run's task.
fn merge_check(w: &StateWriter, repo: &str, pr: i64) -> Result<String, DoorError> {
    let followed = super::prwatch::followed(w.conn(), None).map_err(internal)?;
    let Some(task) = followed
        .into_iter()
        .find(|f| f.pr == pr && f.repo.eq_ignore_ascii_case(repo))
        .map(|f| f.task)
    else {
        return Err(refuse(
            ErrorKind::Conflict,
            format!(
                "{repo}#{pr} is not a pull request makibad follows (the current pull request of an open run, or of a run closed less than a day ago); nothing was judged or sent"
            ),
            "record the pull request with pr first; a pull request makibad does not follow is merged by the person in the GitHub web UI",
        ));
    };
    if let Some(a) = act_awaiting(w.conn(), repo, pr).map_err(internal)? {
        let what = if a.queue {
            "is in the merge queue by makibad's act"
        } else {
            "has a merge act of makibad that has not landed yet"
        };
        return Err(refuse(
            ErrorKind::Conflict,
            format!(
                "{repo}#{pr} {what} (attempt {}); nothing was judged or sent",
                a.attempt
            ),
            "wait for the act's notice, the landing, or the queue dropping it",
        ));
    }
    // Issue 475 (docs/design/queue-ready.md section 6): no act under an update, nor on a head the
    // PR watch judged not ready for the queue.
    if let Some(command) = super::prwatch::update_running(w.conn(), &task).map_err(internal)? {
        return Err(refuse(
            ErrorKind::Conflict,
            format!(
                "{repo}#{pr} is not ready for the queue: the update command {command} of {task} is running; nothing was judged or sent"
            ),
            "wait for the update's notice and the new head's notices",
        ));
    }
    if let Some(head) = super::prwatch::newest_head(w.conn(), &task).map_err(internal)?
        && let Some(r) =
            super::prwatch::newest_readiness(w.conn(), repo, pr, &head).map_err(internal)?
        && r.verdict == "update"
    {
        return Err(refuse(
            ErrorKind::Conflict,
            format!(
                "{repo}#{pr} at {} is not ready for the queue (makibad's queue_readiness {}: {}); nothing was judged or sent",
                &head[..head.len().min(7)],
                r.id,
                r.reason.as_deref().unwrap_or("update")
            ),
            "wait for its update and the new head's notices; a conflict the update could not resolve goes to the worker",
        ));
    }
    Ok(task)
}

/// The warning lines of Issue 513 for verdict `v`, one per non-empty list, each path with the
/// object ids it was judged from (`none` for a missing entry).
fn helper_lines(v: &super::gate::Verdict) -> Vec<String> {
    let list = |items: &[super::gate::Change], names: &[&str]| -> String {
        items
            .iter()
            .map(|c| {
                let ids: Vec<String> = names
                    .iter()
                    .zip(&c.ids)
                    .map(|(n, id)| format!("{n} {id}"))
                    .collect();
                format!("{} ({})", c.path, ids.join(", "))
            })
            .collect::<Vec<_>>()
            .join("; ")
    };
    let mut lines = Vec::new();
    if !v.helpers_changed.is_empty() {
        lines.push(format!(
            "test helpers changed after the test commit, which no rule can tell a fix from a weakening: {}",
            list(&v.helpers_changed, &["test commit", "head"])
        ));
    }
    if !v.merged_tests.is_empty() {
        lines.push(format!(
            "declared tests that head holds as git's clean merge of the test commit and the base: {}",
            list(&v.merged_tests, &["test commit", "base", "head"])
        ));
    }
    if !v.base_helpers.is_empty() {
        lines.push(format!(
            "test helpers the base changed after the test commit was made, with which the declared test was never shown failing: {}",
            list(&v.base_helpers, &["test commit's parent", "base"])
        ));
    }
    lines
}

/// The text of a pending merge entry (Issue 432, merge-act.md section 5.1): exactly what
/// `makiba merge approve` prints, whose SHA-256 the approval carries as `shown`.
#[allow(clippy::too_many_arguments)]
fn merge_entry_text(
    repo: &str,
    pr: i64,
    task: &str,
    title: &str,
    head: &str,
    method: &str,
    reason: &str,
    warnings: &[String],
    stats: &[String],
    requested_by: &str,
) -> String {
    let mut text = format!(
        "Merge pull request {repo}#{pr}\nTask: {task} ({title})\nHead: {head}\nMethod: {method} (on a branch that requires the merge queue, the queue's own method applies)\nGate: {reason}\n"
    );
    for w in warnings {
        text.push_str(&format!("Warning: {w}\n"));
    }
    for line in stats {
        text.push_str(&format!("Diff: {line}\n"));
    }
    text.push_str(&format!("Requested by: {requested_by}\n"));
    text
}

/// The context of a pending merge entry for the gate's reading `j` (merge-act.md section 5.1).
#[allow(clippy::too_many_arguments)]
fn merge_entry_context(
    w: &StateWriter,
    j: &Judged,
    repo: &str,
    pr: i64,
    task: &str,
    head: &str,
    method: &str,
    requested_by: &str,
) -> rusqlite::Result<Value> {
    let title: String = w
        .conn()
        .query_row("SELECT title FROM runs WHERE task_id = ?1", [task], |r| {
            r.get(0)
        })
        .optional()?
        .unwrap_or_default();
    let text = merge_entry_text(
        repo,
        pr,
        task,
        &title,
        head,
        method,
        &j.reason,
        &j.warnings,
        &j.stats,
        requested_by,
    );
    Ok(json!({
        "repo": repo, "pr": pr, "head": head, "method": method, "mode": j.mode,
        "gate_event": j.event, "reason": j.reason, "warnings": j.warnings,
        "requested_by": requested_by, "text_sum": sha256_hex(&text), "text": text,
    }))
}

/// An open merge entry, as the door reads it.
struct MergeEntry {
    id: i64,
    run_task: Option<String>,
    context: Value,
}

/// The pending entry `id` when it is of kind `merge`, whatever its status, and its status.
fn merge_entry(w: &StateWriter, id: i64) -> rusqlite::Result<Option<(MergeEntry, String)>> {
    w.conn()
        .query_row(
            "SELECT p.id, r.task_id, p.context_json, p.status FROM pending p
             LEFT JOIN runs r ON r.id = p.run_id WHERE p.id = ?1 AND p.kind = 'merge'",
            [id],
            |r| {
                let context: String = r.get(2)?;
                Ok((
                    MergeEntry {
                        id: r.get(0)?,
                        run_task: r.get(1)?,
                        context: serde_json::from_str(&context).unwrap_or_default(),
                    },
                    r.get(3)?,
                ))
            },
        )
        .optional()
}

/// The open merge entries of `repo#pr`, oldest first.
fn open_merge_entries(w: &StateWriter, repo: &str, pr: i64) -> rusqlite::Result<Vec<MergeEntry>> {
    let mut st = w.conn().prepare(
        "SELECT p.id, r.task_id, p.context_json FROM pending p LEFT JOIN runs r ON r.id = p.run_id
         WHERE p.kind = 'merge' AND p.status = 'open'
           AND lower(json_extract(p.context_json, '$.repo')) = lower(?1)
           AND json_extract(p.context_json, '$.pr') = ?2
         ORDER BY p.id",
    )?;
    st.query_map(params![repo, pr], |r| {
        let context: String = r.get(2)?;
        Ok(MergeEntry {
            id: r.get(0)?,
            run_task: r.get(1)?,
            context: serde_json::from_str(&context).unwrap_or_default(),
        })
    })?
    .collect()
}

/// Withdraw the open merge entry `e` because `why`, and tell the secretary, in the caller's
/// transaction. Conditional on `status = 'open'`: an entry answered meanwhile is left as it is.
/// Whether it was withdrawn.
fn withdraw_merge_entry(w: &StateWriter, e: &MergeEntry, why: &str) -> crate::db::Result<bool> {
    let changed = w.conn().execute(
        "UPDATE pending SET status = 'withdrawn' WHERE id = ?1 AND kind = 'merge' AND status = 'open'",
        [e.id],
    )?;
    if changed == 1 {
        let c = &e.context;
        let head = c["head"].as_str().unwrap_or_default();
        let body = format!(
            "makibad withdrew pending merge entry {} ({}#{} at {}): {why}. Nothing was merged; when the pull request is ready again, ask again with merge.",
            e.id,
            c["repo"].as_str().unwrap_or_default(),
            c["pr"],
            &head[..head.len().min(12)]
        );
        match &e.run_task {
            Some(task) => notice(w, crate::spec::AGENT_SECRETARY, task, &body)?,
            None => notice_untasked(w, crate::spec::AGENT_SECRETARY, &body)?,
        };
    }
    Ok(changed == 1)
}

/// makibad's withdrawal of the pending merge entries a record leaves behind (Issue 432,
/// merge-act.md section 5.1), in the caller's transaction: every open entry of `repo#pr` at a head
/// other than `keep_head` (every one when `None`), and with `others`, every open entry of `task`'s
/// run for another pull request (the run's current pull request is now `repo#pr`).
pub(super) fn withdraw_merge_entries(
    w: &StateWriter,
    task: &str,
    repo: &str,
    pr: i64,
    keep_head: Option<&str>,
    others: bool,
    why: &str,
) -> crate::db::Result<()> {
    for e in open_merge_entries(w, repo, pr)? {
        if keep_head.is_none_or(|h| e.context["head"].as_str() != Some(h)) {
            withdraw_merge_entry(w, &e, why)?;
        }
    }
    if others {
        let mut st = w.conn().prepare(
            "SELECT p.id, r.task_id, p.context_json FROM pending p JOIN runs r ON r.id = p.run_id
             WHERE r.task_id = ?1 AND p.kind = 'merge' AND p.status = 'open'
               AND NOT (lower(json_extract(p.context_json, '$.repo')) = lower(?2)
                        AND json_extract(p.context_json, '$.pr') = ?3)
             ORDER BY p.id",
        )?;
        let entries: Vec<MergeEntry> = st
            .query_map(params![task, repo, pr], |r| {
                let context: String = r.get(2)?;
                Ok(MergeEntry {
                    id: r.get(0)?,
                    run_task: r.get(1)?,
                    context: serde_json::from_str(&context).unwrap_or_default(),
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        let replaced = format!("the run's pull request is now {repo}#{pr}");
        for e in entries {
            withdraw_merge_entry(w, &e, &replaced)?;
        }
    }
    Ok(())
}

/// Commit the open transaction with the `merge` command of `status` and its answer `r`.
#[allow(clippy::too_many_arguments)]
fn merge_finish(
    w: &StateWriter,
    actor: &str,
    args: &Value,
    idem: Option<&str>,
    status: &str,
    r: Response,
) -> Response {
    if insert_command(w, actor, "merge", args, idem, status, &r).is_none()
        || w.conn().is_autocommit()
    {
        let _ = w.rollback();
        return Err(internal(
            "the request could not be recorded; nothing was done",
        ));
    }
    if let Err(e) = w.commit() {
        let _ = w.rollback();
        return Err(internal(e));
    }
    r
}

/// The attempt (`merge_attempted`, merge-act.md section 4.1) with its command `accepted`, in the
/// open transaction; the answer. `pending_id`: the entry the person approved.
#[allow(clippy::too_many_arguments)]
fn merge_attempt(
    w: &StateWriter,
    actor: &str,
    args: &Value,
    idem: Option<&str>,
    j: &Judged,
    (repo, pr, head, method): (&str, i64, &str, &str),
    task: &str,
    approved_by: Option<&str>,
    pending_id: Option<i64>,
) -> Result<Value, String> {
    let provisional: Response = Err(internal("the attempt was not recorded"));
    let command = insert_command(w, actor, "merge", args, idem, "accepted", &provisional)
        .ok_or("the command could not be recorded")?;
    let mut payload = json!({
        "repo": repo, "pr": pr, "head": head, "method": method,
        "gate_event": j.event, "command": command, "approved_by": approved_by,
        "boot": super::merge::boot(), "task_id": task,
    });
    if let Some(id) = pending_id {
        payload["pending_id"] = json!(id);
    }
    let attempt = w
        .append_event(&NewEvent {
            kind: "merge_attempted",
            actor: Some(actor),
            payload: Some(&payload),
            run_task_id: Some(task),
            ..Default::default()
        })
        .map_err(|e| e.to_string())?;
    let mut answer = json!({"status": "accepted", "attempt": attempt, "repo": repo, "pr": pr,
                            "head": head, "method": method, "mode": j.mode});
    if let Some(id) = pending_id {
        answer["pending_id"] = json!(id);
    }
    w.conn()
        .execute(
            // The provisional line was an error: its kind goes with it.
            "UPDATE commands SET result_json = ?1, error_kind = NULL WHERE id = ?2",
            params![line(&Ok(answer.clone())).to_string(), command],
        )
        .map_err(|e| e.to_string())?;
    Ok(answer)
}

/// The answer's `status` and the command's status of a `merge` the gate did not allow: a read of
/// GitHub that still failed is `unavailable`, recorded `failed` (Issue 512); any other verdict is
/// `refused`, recorded `rejected`.
fn merge_refusal(verdict: &str) -> (&'static str, &'static str) {
    if verdict == "unavailable" {
        ("unavailable", "failed")
    } else {
        ("refused", "rejected")
    }
}

/// A refusal of a merge entry that is no longer the one shown, or no longer open.
fn merge_stale(id: i64, why: &str) -> DoorError {
    refuse(
        ErrorKind::Stale,
        format!("pending merge entry {id} {why}; nothing was merged"),
        "run makiba merge approve <id> again: it prints the entry as it is now",
    )
}

impl Door {
    /// `merge` (Issue 431, merge-act.md section 3): `request` runs the gate at `head` and, on an
    /// allowance that needs no person, commits the attempt (`merge_attempted`) with the gate's row
    /// and the command `accepted`; makibad's merge pass runs the act afterwards. A refusal answers
    /// the verdict and sends nothing. An allowance that needs the person (under human from the
    /// secretary, or with a warning) writes a pending entry of kind `merge` instead (Issue 432,
    /// section 5), which only the person's `approve` turns into an attempt; `refuse` and
    /// `withdraw` end it.
    fn merge(
        &self,
        w: MutexGuard<'_, StateWriter>,
        actor: &str,
        role: Role,
        args: &Value,
        idem: Option<&str>,
    ) -> Response {
        let fail = |w: &StateWriter, r: Response| {
            let _ = record(w, actor, "merge", args, idem, &r);
            r
        };
        let action = match args.get("action") {
            None | Some(Value::Null) => "request",
            Some(a) => match a.as_str() {
                Some(a @ ("request" | "order" | "approve" | "refuse" | "withdraw")) => a,
                _ => {
                    return fail(
                        &w,
                        Err(usage(
                            "action is request, order, approve, refuse or withdraw",
                        )),
                    );
                }
            },
        };
        if ["approve", "refuse"].contains(&action) && role != Role::Person {
            return fail(
                &w,
                Err(refuse(
                    ErrorKind::Denied,
                    format!("only the person may {action} a pending merge"),
                    "the person approves it in their shell with makiba merge approve <id>",
                )),
            );
        }
        match action {
            "approve" => return self.merge_approve(w, actor, args, idem),
            "refuse" | "withdraw" => return merge_end(&w, actor, role, action, args, idem),
            "order" => return merge_order_set(&w, actor, args, idem),
            _ => {}
        }
        let parsed = (|| -> Result<_, DoorError> {
            let (repo, pr, head) = gate_args(args)?;
            let method = match args.get("method") {
                None | Some(Value::Null) => "squash",
                Some(m) => m
                    .as_str()
                    .filter(|m| ["squash", "merge", "rebase"].contains(m))
                    .ok_or_else(|| usage("method is squash, merge or rebase"))?,
            };
            Ok((repo, pr, head, method.to_string()))
        })();
        let (repo, pr, head, method) = match parsed {
            Ok(p) => p,
            Err(e) => return fail(&w, Err(e)),
        };
        if let Err(e) = merge_check(&w, &repo, pr) {
            return fail(&w, Err(e));
        }
        let rows = match gate_rows(&w, &repo, pr) {
            Ok(r) => r,
            Err(e) => return fail(&w, Err(internal(e))),
        };
        drop(w);
        let found = gate_outside(&rows, &repo, pr, &head);
        let w = self.writer();
        // The same request may have been recorded while the lock was released.
        if let Some(idem) = idem
            && let Some(first) = replay(&w, actor, idem).map_err(internal)?
        {
            return first;
        }
        if let Err(e) = begin_as(&w, "door merge") {
            return Err(internal(e));
        }
        let j = match self.judge(&w, actor, &repo, pr, &head, &rows, found, "merge") {
            Ok(j) => j,
            Err(e) => return fail(&w, Err(e)),
        };
        // Checked again with the lock: an act or a replacement recorded while the gate ran makes
        // the whole request nothing, the gate's row too.
        let task = match merge_check(&w, &repo, pr) {
            Ok(t) => t,
            Err(e) => {
                let _ = w.rollback();
                return fail(&w, Err(e));
            }
        };
        if !j.allow {
            let (status, recorded) = merge_refusal(&j.verdict);
            let mut answer = json!({"status": status, "verdict": j.verdict, "allow": false, "reason": j.reason,
                                    "mode": j.mode, "repo": repo, "pr": pr, "head": head});
            if let Some(rv) = &j.review {
                answer["review"] = rv.clone();
            }
            // The stop a read that failed made for the person (Issue 512).
            if j.verdict == "unavailable"
                && let Some(id) = j.answer.get("pending_id")
            {
                answer["pending_id"] = id.clone();
            }
            return merge_finish(&w, actor, args, idem, recorded, Ok(answer));
        }
        if j.warn || (j.mode != "acceptance" && role != Role::Person) {
            // Issue 432: the person's key decides. The same request again is the same entry; one
            // at another head or with another method replaces it.
            let written = (|| -> crate::db::Result<i64> {
                for e in open_merge_entries(&w, &repo, pr)? {
                    let c = &e.context;
                    if c["head"].as_str() == Some(head.as_str())
                        && c["method"].as_str() == Some(method.as_str())
                    {
                        return Ok(e.id);
                    }
                    withdraw_merge_entry(
                        &w,
                        &e,
                        "the secretary asked again at another head or with another method",
                    )?;
                }
                let context = merge_entry_context(&w, &j, &repo, pr, &task, &head, &method, actor)?;
                w.conn().execute(
                    "INSERT INTO pending (run_id, kind, question, context_json)
                     VALUES ((SELECT id FROM runs WHERE task_id = ?1), 'merge', ?2, ?3)",
                    params![
                        task,
                        format!(
                            "Merge {repo}#{pr} at {} by {method}?",
                            &head[..head.len().min(12)]
                        ),
                        context.to_string()
                    ],
                )?;
                Ok(w.conn().last_insert_rowid())
            })();
            let id = match written {
                Ok(id) => id,
                Err(e) => {
                    let _ = w.rollback();
                    return Err(internal(format!(
                        "the pending merge could not be recorded; nothing was done: {e}"
                    )));
                }
            };
            let answer = json!({"status": "pending", "pending_id": id, "repo": repo, "pr": pr,
                                "head": head, "method": method, "mode": j.mode,
                                "warnings": j.warnings});
            return merge_finish(&w, actor, args, idem, "done", Ok(answer));
        }
        let approved_by = (role == Role::Person).then_some("person");
        // A pending merge of this pull request is superseded by the act that runs now: left open,
        // it could be approved after this attempt ended, for a request nobody makes any more.
        let attempted = withdraw_merge_entries(
            &w,
            &task,
            &repo,
            pr,
            None,
            false,
            "makibad's merge of the pull request was asked again and runs now without the entry",
        )
        .map_err(|e| e.to_string())
        .and_then(|()| {
            merge_attempt(
                &w,
                actor,
                args,
                idem,
                &j,
                (&repo, pr, &head, &method),
                &task,
                approved_by,
                None,
            )
        });
        let answer = match attempted {
            Ok(a) => a,
            Err(e) => {
                let _ = w.rollback();
                return Err(internal(format!(
                    "the attempt could not be recorded; nothing was done: {e}"
                )));
            }
        };
        if w.conn().is_autocommit() {
            return Err(internal(
                "the request could not be recorded; nothing was done",
            ));
        }
        if let Err(e) = w.commit() {
            let _ = w.rollback();
            return Err(internal(e));
        }
        Ok(answer)
    }

    /// `merge approve` (Issue 432, merge-act.md section 5.2), the person only: the gate runs again
    /// at the entry's head, and in the final transaction one conditional update consumes the entry
    /// only while it is open and holds the text the person was shown; the attempt is written only
    /// when that update changed the row. A refusal, a withdrawal or another approval committed
    /// while the gate ran leaves it unchanged: `stale`, and nothing is written.
    fn merge_approve(
        &self,
        w: MutexGuard<'_, StateWriter>,
        actor: &str,
        args: &Value,
        idem: Option<&str>,
    ) -> Response {
        let fail = |w: &StateWriter, r: Response| {
            let _ = record(w, actor, "merge", args, idem, &r);
            r
        };
        let parsed = (|| -> Result<(i64, String), DoorError> {
            Ok((id_arg(args)?, str_arg(args, "shown")?.to_string()))
        })();
        let (id, shown) = match parsed {
            Ok(p) => p,
            Err(e) => return fail(&w, Err(e)),
        };
        let first = match merge_entry(&w, id) {
            Ok(e) => e,
            Err(e) => return fail(&w, Err(internal(e))),
        };
        let Some((entry, status)) = first else {
            return fail(
                &w,
                Err(refuse(
                    ErrorKind::NotFound,
                    format!("no pending merge entry {id}"),
                    "",
                )),
            );
        };
        // A first check: it only saves a gate run; the final transaction decides.
        if status != "open" {
            return fail(&w, Err(merge_stale(id, &format!("is {status}"))));
        }
        if entry.context["text_sum"].as_str() != Some(shown.as_str()) {
            return fail(&w, Err(merge_stale(id, "is not the text that was printed")));
        }
        let c = &entry.context;
        let (Some(repo), Some(pr), Some(head), Some(method)) = (
            c["repo"].as_str().map(String::from),
            c["pr"].as_i64(),
            c["head"].as_str().map(String::from),
            c["method"].as_str().map(String::from),
        ) else {
            return fail(
                &w,
                Err(internal(format!(
                    "pending merge entry {id} names no pull request"
                ))),
            );
        };
        let shown_warnings: Vec<String> = c["warnings"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
        let requested_by = c["requested_by"].as_str().unwrap_or(actor).to_string();
        if let Err(e) = merge_check(&w, &repo, pr) {
            return fail(&w, Err(e));
        }
        let rows = match gate_rows(&w, &repo, pr) {
            Ok(r) => r,
            Err(e) => return fail(&w, Err(internal(e))),
        };
        drop(w);
        let found = gate_outside(&rows, &repo, pr, &head);
        let w = self.writer();
        if let Some(idem) = idem
            && let Some(first) = replay(&w, actor, idem).map_err(internal)?
        {
            return first;
        }
        if let Err(e) = begin_as(&w, "door merge_approve") {
            return Err(internal(e));
        }
        let j = match self.judge(&w, actor, &repo, pr, &head, &rows, found, "merge") {
            Ok(j) => j,
            Err(e) => return fail(&w, Err(e)),
        };
        let task = match merge_check(&w, &repo, pr) {
            Ok(t) => t,
            Err(e) => {
                let _ = w.rollback();
                return fail(&w, Err(e));
            }
        };
        // The entry read again with the lock: refused, withdrawn, approved or changed while the gate
        // ran, it is no longer the one this approval names, whatever the gate said; nothing stands.
        let now: rusqlite::Result<Option<(String, Option<String>)>> = w
            .conn()
            .query_row(
                "SELECT status, json_extract(context_json, '$.text_sum') FROM pending
                 WHERE id = ?1 AND kind = 'merge'",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional();
        match now {
            Ok(Some((status, sum))) if status == "open" && sum.as_deref() == Some(&shown) => {}
            Ok(_) => {
                let _ = w.rollback();
                return fail(
                    &w,
                    Err(merge_stale(
                        id,
                        "was answered or changed while the gate ran",
                    )),
                );
            }
            Err(e) => {
                let _ = w.rollback();
                return Err(internal(e));
            }
        }
        if !j.allow {
            // The entry stays open: the person may approve it again once the cause is dealt with.
            let what = if j.verdict == "unavailable" {
                "GitHub could not be read, so the gate could not judge"
            } else {
                "The gate refused"
            };
            let told = notice(
                &w,
                crate::spec::AGENT_SECRETARY,
                &task,
                &format!(
                    "{what} the person's approval of pending merge entry {id} ({repo}#{pr} at {}): {}. The entry stays open; deal with the cause, then the person approves it again.",
                    &head[..head.len().min(12)],
                    j.reason
                ),
            );
            if let Err(e) = told {
                let _ = w.rollback();
                return Err(internal(e));
            }
            let (status, recorded) = merge_refusal(&j.verdict);
            let mut answer = json!({"status": status, "verdict": j.verdict, "allow": false, "reason": j.reason,
                                    "mode": j.mode, "repo": repo, "pr": pr, "head": head,
                                    "pending_id": id});
            if let Some(rv) = &j.review {
                answer["review"] = rv.clone();
            }
            return merge_finish(&w, actor, args, idem, recorded, Ok(answer));
        }
        if j.warnings.iter().any(|x| !shown_warnings.contains(x)) {
            // A warning the person was not shown: the entry takes the new reading, and the person
            // approves what they are shown next, nothing else.
            let replaced =
                merge_entry_context(&w, &j, &repo, pr, &task, &head, &method, &requested_by)
                    .map_err(crate::db::Error::from)
                    .and_then(|context| {
                        Ok(w.conn().execute(
                            "UPDATE pending SET context_json = ?1
                     WHERE id = ?2 AND kind = 'merge' AND status = 'open'
                       AND json_extract(context_json, '$.text_sum') = ?3",
                            params![context.to_string(), id, shown],
                        )?)
                    });
            if let Err(e) = replaced {
                let _ = w.rollback();
                return Err(internal(e));
            }
            return merge_finish(
                &w,
                actor,
                args,
                idem,
                "rejected",
                Err(merge_stale(
                    id,
                    "now has a warning the printed text did not show",
                )),
            );
        }
        let consumed = w.conn().execute(
            "UPDATE pending SET status = 'answered', answer = 'approved', answered_by = 'person',
               answered_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
             WHERE id = ?1 AND kind = 'merge' AND status = 'open'
               AND json_extract(context_json, '$.text_sum') = ?2",
            params![id, shown],
        );
        match consumed {
            Ok(1) => {}
            Ok(_) => {
                // Refused, withdrawn, approved or changed while the gate ran: nothing stands.
                let _ = w.rollback();
                return fail(
                    &w,
                    Err(merge_stale(
                        id,
                        "was answered or changed while the gate ran",
                    )),
                );
            }
            Err(e) => {
                let _ = w.rollback();
                return Err(internal(e));
            }
        }
        let attempted = merge_attempt(
            &w,
            actor,
            args,
            idem,
            &j,
            (&repo, pr, &head, &method),
            &task,
            Some("person"),
            Some(id),
        );
        let answer = match attempted {
            Ok(a) => a,
            Err(e) => {
                let _ = w.rollback();
                return Err(internal(format!(
                    "the attempt could not be recorded; nothing was done: {e}"
                )));
            }
        };
        if w.conn().is_autocommit() {
            return Err(internal(
                "the request could not be recorded; nothing was done",
            ));
        }
        if let Err(e) = w.commit() {
            let _ = w.rollback();
            return Err(internal(e));
        }
        Ok(answer)
    }
}

/// `merge` action `order` (Issue 338, docs/design/merge-order.md section 6): the person's order
/// "`repo#pr` merges only after `after_pr` or `after_issue` has landed", relayed by the secretary
/// or given by the person, recorded as `merge_order_set` in one transaction with its command. The
/// same order again is the first one. Refused (`conflict`) while the pull request has a merge act
/// of makibad awaiting its landing: that act runs without a new judgement, so the order could not
/// hold it.
fn merge_order_set(w: &StateWriter, actor: &str, args: &Value, idem: Option<&str>) -> Response {
    let fail = |r: Response| {
        let _ = record(w, actor, "merge", args, idem, &r);
        r
    };
    let parsed = (|| -> Result<(String, i64, &'static str, i64, String), DoorError> {
        let repo = repo_arg(args)?;
        let number = |k: &str| -> Result<Option<i64>, DoorError> {
            match args.get(k) {
                None | Some(Value::Null) => Ok(None),
                Some(v) => v
                    .as_i64()
                    .filter(|n| *n > 0)
                    .map(Some)
                    .ok_or_else(|| usage(format!("{k} is a positive number"))),
            }
        };
        let pr = number("pr")?
            .ok_or_else(|| usage("the argument pr (a pull request number) is missing"))?;
        let (after, n) = match (number("after_pr")?, number("after_issue")?) {
            (Some(n), None) => ("pr", n),
            (None, Some(n)) => ("issue", n),
            _ => {
                return Err(usage(
                    "an order names exactly one of after_pr (a pull request) and after_issue (an Issue) of the same repository",
                ));
            }
        };
        if after == "pr" && n == pr {
            return Err(usage("a pull request cannot wait for itself"));
        }
        let reason = args
            .get("reason")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .ok_or_else(|| {
                usage("the argument reason (the person's decision, as text) is missing")
            })?;
        Ok((repo.to_string(), pr, after, n, reason.to_string()))
    })();
    let (repo, pr, after, number, reason) = match parsed {
        Ok(p) => p,
        Err(e) => return fail(Err(e)),
    };
    if let Err(e) = begin_as(w, "door merge_order_set") {
        return Err(internal(e));
    }
    let done = (|| -> Result<Response, rusqlite::Error> {
        if let Some(a) = act_awaiting(w.conn(), &repo, pr)? {
            return Ok(Err(refuse(
                ErrorKind::Conflict,
                format!(
                    "{repo}#{pr} has a merge act of makibad awaiting its landing (attempt {}); an order cannot hold it",
                    a.attempt
                ),
                "tell the person: the merge was already sent",
            )));
        }
        let project = if after == "issue" {
            let projects: Vec<(String, Option<String>)> = w
                .conn()
                .prepare(
                    "SELECT slug, origin_url FROM projects WHERE status = 'active' ORDER BY slug",
                )?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            let found: Vec<String> = projects
                .into_iter()
                .filter(|(_, url)| {
                    url.as_deref()
                        .and_then(super::triage::owner_repo_from_url)
                        .is_some_and(|r| r.eq_ignore_ascii_case(&repo))
                })
                .map(|(slug, _)| slug)
                .collect();
            match found.as_slice() {
                [slug] => Some(slug.clone()),
                [_, _, ..] => {
                    return Ok(Err(refuse(
                        ErrorKind::Conflict,
                        format!(
                            "more than one active project records {repo} ({}): the Issue's project cannot be told",
                            found.join(", ")
                        ),
                        "tell the person; an order after a pull request (after_pr) needs no project",
                    )));
                }
                [] => {
                    return Ok(Err(refuse(
                        ErrorKind::NotFound,
                        format!("no active project's recorded repository is {repo}"),
                        "an Issue is matched through the runs of its project",
                    )));
                }
            }
        } else {
            None
        };
        let existing = super::gate::person_orders(w.conn(), &repo, pr)?
            .into_iter()
            .find(|o| o.after == after && o.number == number);
        let (event_id, landed) = match existing {
            Some(o) => (o.event_id, o.landed),
            None => {
                let task: Option<String> = w
                    .conn()
                    .query_row(
                        "SELECT r.task_id FROM pr_watches p JOIN runs r ON r.id = p.run_id
                         WHERE lower(p.repo) = lower(?1) AND p.pr = ?2 ORDER BY p.id DESC LIMIT 1",
                        params![repo, pr],
                        |r| r.get(0),
                    )
                    .optional()?;
                let payload = json!({"repo": repo, "pr": pr, "after": after, "number": number,
                                     "project": project, "reason": reason, "by": actor});
                let id = w
                    .append_event(&NewEvent {
                        kind: "merge_order_set",
                        actor: Some(actor),
                        payload: Some(&payload),
                        run_task_id: task.as_deref(),
                        ..Default::default()
                    })
                    .map_err(|e| rusqlite::Error::InvalidParameterName(e.to_string()))?;
                let landed =
                    super::gate::order_landed(w.conn(), &repo, after, number, project.as_deref())?;
                (id, landed)
            }
        };
        Ok(Ok(
            json!({"status": "recorded", "event_id": event_id, "repo": repo, "pr": pr,
                     "after": after, "number": number, "landed": landed}),
        ))
    })();
    match done {
        Ok(Ok(v)) => merge_finish(w, actor, args, idem, "done", Ok(v)),
        Ok(Err(e)) => {
            let _ = w.rollback();
            fail(Err(e))
        }
        Err(e) => {
            let _ = w.rollback();
            Err(internal(e))
        }
    }
}

/// `merge refuse` (the person) and `merge withdraw` (the secretary or the person), Issue 432: the
/// entry ends by one conditional update on `status = 'open'`; an entry no longer open is `stale`.
/// Neither needs the pull request to be a current one (merge-act.md, the last review's follow-up 1).
fn merge_end(
    w: &StateWriter,
    actor: &str,
    role: Role,
    action: &str,
    args: &Value,
    idem: Option<&str>,
) -> Response {
    let fail = |r: Response| {
        let _ = record(w, actor, "merge", args, idem, &r);
        r
    };
    let id = match id_arg(args) {
        Ok(id) => id,
        Err(e) => return fail(Err(e)),
    };
    let reason = args
        .get("reason")
        .and_then(Value::as_str)
        .filter(|r| !r.trim().is_empty());
    if let Err(e) = begin_as(w, "door merge_end") {
        return Err(internal(e));
    }
    let ended = (|| -> Result<Response, crate::db::Error> {
        let Some((entry, _)) = merge_entry(w, id)? else {
            return Ok(Err(refuse(
                ErrorKind::NotFound,
                format!("no pending merge entry {id}"),
                "",
            )));
        };
        let (to, answer) = if action == "refuse" {
            ("answered", Some("refused"))
        } else {
            ("withdrawn", None)
        };
        let changed = w.conn().execute(
            "UPDATE pending SET status = ?1, answer = ?2,
               answered_by = CASE WHEN ?2 IS NULL THEN answered_by ELSE 'person' END,
               answered_at = CASE WHEN ?2 IS NULL THEN answered_at
                                  ELSE strftime('%Y-%m-%dT%H:%M:%fZ','now') END
             WHERE id = ?3 AND kind = 'merge' AND status = 'open'",
            params![to, answer, id],
        )?;
        if changed != 1 {
            return Ok(Err(merge_stale(id, "is no longer open")));
        }
        let c = &entry.context;
        let head = c["head"].as_str().unwrap_or_default();
        let what = format!(
            "{}#{} at {}",
            c["repo"].as_str().unwrap_or_default(),
            c["pr"],
            &head[..head.len().min(12)]
        );
        let because = reason.map(|r| format!(": {r}")).unwrap_or_default();
        let body = if action == "refuse" {
            Some(format!(
                "The person refused the merge of {what} (pending merge entry {id}){because}. Do not ask again for this head unless the person says so."
            ))
        } else if role == Role::Person {
            Some(format!(
                "The person withdrew pending merge entry {id} ({what}){because}. Nothing was merged."
            ))
        } else {
            None
        };
        if let Some(body) = body {
            match &entry.run_task {
                Some(task) => notice(w, crate::spec::AGENT_SECRETARY, task, &body)?,
                None => notice_untasked(w, crate::spec::AGENT_SECRETARY, &body)?,
            };
        }
        let mut v = json!({"id": id, "status": to});
        if let Some(a) = answer {
            v["answer"] = json!(a);
        }
        Ok(Ok(v))
    })();
    match ended {
        Ok(r @ Ok(_)) => merge_finish(w, actor, args, idem, "done", r),
        Ok(r @ Err(_)) => {
            let _ = w.rollback();
            fail(r)
        }
        Err(e) => {
            let _ = w.rollback();
            Err(internal(e))
        }
    }
}

// ---- review (Issue 135) -------------------------------------------------------------------------

/// The rounds of `mode` (`diff` or `design`) a task has started; a row without a mode is `diff`.
fn rounds_used(w: &StateWriter, task: &str, mode: &str) -> rusqlite::Result<i64> {
    w.conn().query_row(
        "SELECT COUNT(*) FROM events WHERE kind = 'codex_round_started'
           AND json_extract(payload_json, '$.task') = ?1
           AND COALESCE(json_extract(payload_json, '$.mode'), 'diff') = ?2",
        params![task, mode],
        |r| r.get(0),
    )
}

/// A task's diff allowance: the last `codex_round_cap_changed`, else the default; `None` is
/// unlimited.
fn rounds_allowed(w: &StateWriter, task: &str) -> rusqlite::Result<Option<i64>> {
    let last: Option<Value> = w
        .conn()
        .query_row(
            "SELECT json_extract(payload_json, '$.allowed') FROM events
             WHERE kind = 'codex_round_cap_changed' AND json_extract(payload_json, '$.task') = ?1
             ORDER BY id DESC LIMIT 1",
            [task],
            |r| r.get::<_, Option<rusqlite::types::Value>>(0),
        )
        .optional()?
        .flatten()
        .map(|v| match v {
            rusqlite::types::Value::Integer(n) => json!(n),
            rusqlite::types::Value::Text(t) => json!(t),
            _ => Value::Null,
        });
    Ok(match last {
        None => Some(super::review::DIFF_ALLOWED),
        Some(v) => v.as_i64(),
    })
}

fn allowed_json(a: Option<i64>) -> Value {
    a.map_or(json!("unlimited"), |n| json!(n))
}

/// `review` by the secretary or the person: set a task's diff allowance, with a reason.
fn review_allowance(w: &StateWriter, actor: &str, args: &Value) -> Response {
    let task = str_arg(args, "task")
        .map_err(|_| usage("the secretary sets a task's allowance: task, allowed and reason"))?;
    let allowed = match &args["allowed"] {
        Value::String(s) if s == "unlimited" => None,
        v => Some(
            v.as_i64()
                .filter(|n| *n >= 0)
                .ok_or_else(|| usage("allowed is a whole number from 0, or \"unlimited\""))?,
        ),
    };
    let reason = args["reason"].as_str().map(str::trim).unwrap_or_default();
    if reason.is_empty() {
        return Err(usage("a reason is required"));
    }
    if !run_exists(w, task)? {
        return Err(refuse(ErrorKind::NotFound, format!("no task {task:?}"), ""));
    }
    let previous = rounds_allowed(w, task).map_err(internal)?;
    let payload = json!({"task": task, "allowed": allowed_json(allowed),
                         "previous": allowed_json(previous), "reason": reason});
    w.append_event(&NewEvent {
        kind: "codex_round_cap_changed",
        actor: Some(actor),
        payload: Some(&payload),
        run_task_id: Some(task),
        ..Default::default()
    })
    .map_err(internal)?;
    Ok(json!({
        "task": task,
        "allowed": allowed_json(allowed),
        "used": rounds_used(w, task, "diff").map_err(internal)?,
        "design_used": rounds_used(w, task, "design").map_err(internal)?,
    }))
}

/// `report done` is refused while the worker's last diff review found a blocker (Issue 135).
fn last_review_clear(w: &StateWriter, actor: &str) -> Result<(), DoorError> {
    let blockers: Option<i64> = w
        .conn()
        .query_row(
            "SELECT json_extract(result_json, '$.result.findings.blocker') FROM commands
             WHERE verb = 'review' AND actor = ?1 AND status = 'done'
               AND json_extract(result_json, '$.result.kind') = 'diff'
             ORDER BY id DESC LIMIT 1",
            [actor],
            |r| r.get::<_, Option<i64>>(0),
        )
        .optional()
        .map_err(internal)?
        .flatten();
    match blockers {
        Some(n) if n > 0 => Err(refuse(
            ErrorKind::Conflict,
            format!("your last diff review found {n} blocker(s)"),
            "fix them, commit, and call review again; or report blocked with the findings",
        )),
        _ => Ok(()),
    }
}

/// The review state of a code task's reported head: `passed`, `not-established`, `none` (no diff
/// review) or `stale` (the last one read an older head), with the reader and the heads. `None` for
/// a task that is not a code run with a repository of its own.
fn review_state(w: &StateWriter, actor: &str, task: &str) -> Result<Option<Value>, DoorError> {
    let Ok(run) = review_run(w, task) else {
        return Ok(None);
    };
    let state_dir = w
        .conn()
        .path()
        .and_then(|p| Path::new(p).parent().map(Path::to_path_buf))
        .unwrap_or_default();
    let places = super::prepare::Places::from_env(&state_dir);
    let gitdir = super::run_repo::dir(&places.settings, task);
    if run.kind != "code" || !super::run_repo::is_run(&gitdir, &run.workplace) {
        return Ok(None);
    }
    let head = super::run_repo::commit(&gitdir, &run.workplace, "HEAD");
    let last: Option<String> = w
        .conn()
        .query_row(
            "SELECT json_extract(result_json, '$.result') FROM commands
             WHERE verb = 'review' AND actor = ?1 AND status = 'done'
               AND json_extract(result_json, '$.result.kind') = 'diff'
             ORDER BY id DESC LIMIT 1",
            [actor],
            |r| r.get(0),
        )
        .optional()
        .map_err(internal)?
        .flatten();
    let last: Option<Value> = last.and_then(|t| serde_json::from_str(&t).ok());
    let state = match &last {
        None => "none",
        // Stale only when the review names a head and it is another; a review that ran on no
        // head (no reader could start) keeps its own verdict and reason.
        Some(l) if l["head"].is_string() && l["head"].as_str() != head.as_deref() => "stale",
        Some(l) => l["verdict"].as_str().unwrap_or("not-established"),
    };
    let mut r = json!({"state": state, "head": head});
    if let Some(l) = &last {
        for k in ["reader", "round", "allowed", "findings", "reason"] {
            if !l[k].is_null() {
                r[k] = l[k].clone();
            }
        }
        r["reviewed_head"] = l["head"].clone();
    }
    Ok(Some(r))
}

/// The line the secretary reads under a done report.
fn review_line(r: &Value) -> String {
    let short = |v: &Value| {
        v.as_str()
            .map(|s| s[..s.len().min(12)].to_string())
            .unwrap_or_default()
    };
    let head = short(&r["head"]);
    match r["state"].as_str().unwrap_or_default() {
        "passed" => format!(
            "review of {head}: passed by {} (round {} of {}), findings {}",
            r["reader"].as_str().unwrap_or("?"),
            r["round"],
            r["allowed"],
            r["findings"]
        ),
        "none" => format!("review of {head}: none (no diff review was run)"),
        "stale" => format!(
            "review of {head}: stale (the last diff review read {})",
            short(&r["reviewed_head"])
        ),
        _ => format!(
            "review of {head}: not established ({})",
            r["reason"].as_str().unwrap_or("no reason")
        ),
    }
}

/// What a worker's review needs of its run.
struct ReviewRun {
    project: String,
    branch: String,
    kind: String,
    workplace: PathBuf,
}

fn review_run(w: &StateWriter, task: &str) -> Result<ReviewRun, DoorError> {
    w.conn()
        .query_row(
            "SELECT p.slug, COALESCE(p.default_branch, 'main'), l.kind, l.worktree
             FROM runs r JOIN projects p ON p.id = r.project_id
             JOIN run_launch l ON l.run_id = r.id
             WHERE r.task_id = ?1 ORDER BY l.id DESC LIMIT 1",
            [task],
            |r| {
                Ok(ReviewRun {
                    project: r.get(0)?,
                    branch: r.get(1)?,
                    kind: r.get(2)?,
                    workplace: PathBuf::from(r.get::<_, String>(3)?),
                })
            },
        )
        .optional()
        .map_err(internal)?
        .ok_or_else(|| {
            refuse(
                ErrorKind::NotFound,
                format!("no launched task {task:?}"),
                "",
            )
        })
}

/// Removes the task from the door's set of running reviews when the round ends, however.
struct Reviewing<'a>(&'a Mutex<std::collections::HashSet<String>>, String);

impl Drop for Reviewing<'_> {
    fn drop(&mut self) {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&self.1);
    }
}

impl Door {
    /// `review` by a worker (Issue 135): one round of `kind` (`diff` or `design`) on its own task.
    /// Checked and counted under the door's lock; the reader runs with the lock released.
    fn review(
        &self,
        w: MutexGuard<'_, StateWriter>,
        actor: &str,
        args: &Value,
        idem: Option<&str>,
    ) -> Response {
        let fail = |w: &StateWriter, r: Response| {
            let _ = record(w, actor, "review", args, idem, &r);
            r
        };
        if ["task", "allowed", "reason"]
            .iter()
            .any(|k| args.get(*k).is_some())
        {
            return fail(
                &w,
                Err(refuse(
                    ErrorKind::Denied,
                    "a worker reviews its own task and cannot set its allowance",
                    "only the secretary changes the allowance; report the remaining findings to it",
                )),
            );
        }
        let task = actor.strip_prefix("w-").unwrap_or(actor).to_string();
        let design = match args["kind"].as_str() {
            Some("design") => true,
            Some("diff") => false,
            _ => return Err(usage("kind is diff or design")),
        };
        let note = args["note"].as_str().filter(|n| !n.trim().is_empty());
        if design && note.is_none() {
            return Err(usage("a design review needs the design as note"));
        }
        let mode = if design { "design" } else { "diff" };
        let run = match review_run(&w, &task) {
            Ok(r) => r,
            Err(e) => return fail(&w, Err(e)),
        };
        if run.kind != "code" {
            return Err(usage(
                "review is for code tasks; a document task has no repository",
            ));
        }
        if !self
            .reviewing
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(task.clone())
        {
            return fail(
                &w,
                Err(refuse(
                    ErrorKind::Conflict,
                    "a review of this task is running",
                    "wait for its answer",
                )),
            );
        }
        let _running = Reviewing(&self.reviewing, task.clone());
        drop(w);
        // Before a round is spent: which reader can run, and the snapshot it will read.
        let places = super::prepare::Places::from_env(&self.state);
        let plan = super::review::Plan::find(&self.review);
        let base_answer = |verdict: &str, reader: &str, reason: &str| json!({"verdict": verdict, "kind": mode, "reader": reader, "reason": reason});
        if let Some(why) = plan.none() {
            let w = self.writer();
            let why = super::review::scrub(why);
            return fail(&w, Ok(base_answer("not-established", "none", &why)));
        }
        let snap = match super::review::snapshot(
            &places,
            &task,
            &run.project,
            &run.branch,
            &run.workplace,
            note.filter(|_| design),
        ) {
            Ok(s) => s,
            Err(why) => {
                let w = self.writer();
                return fail(
                    &w,
                    Err(refuse(
                        ErrorKind::Conflict,
                        format!("the review could not be prepared: {why}"),
                        "no round was spent",
                    )),
                );
            }
        };
        // Admission: count and record the round, or refuse it, in one short transaction.
        let (round, allowed) = {
            let w = self.writer();
            if let Err(e) = begin_as(&w, "door review") {
                return Err(internal(e));
            }
            let counted = (|| -> rusqlite::Result<(i64, Option<i64>)> {
                let used = rounds_used(&w, &task, mode)?;
                let allowed = if design {
                    Some(super::review::DESIGN_ALLOWED)
                } else {
                    rounds_allowed(&w, &task)?
                };
                Ok((used, allowed))
            })();
            let (used, allowed) = match counted {
                Ok(c) => c,
                Err(e) => {
                    let _ = w.rollback();
                    return Err(internal(e));
                }
            };
            let over = allowed.is_some_and(|a| used >= a);
            let mut payload = json!({"task": task, "mode": mode, "allowed": allowed_json(allowed),
                                     "base": snap.base, "head": snap.head});
            if over {
                payload["used"] = json!(used);
            } else {
                payload["round"] = json!(used + 1);
            }
            if let Some(n) = note.filter(|_| design) {
                payload["note_sha256"] = json!(sha256_hex(n));
            }
            let written = w.append_event(&NewEvent {
                kind: if over {
                    "codex_round_denied"
                } else {
                    "codex_round_started"
                },
                actor: Some(actor),
                payload: Some(&payload),
                run_task_id: Some(&task),
                ..Default::default()
            });
            let r: Response = if over {
                Err(refuse(
                    ErrorKind::Conflict,
                    format!(
                        "REVIEW_ROUND_DENIED (kind={mode} used={used} allowed={}): do not call review again; report the remaining findings and your judgment to the secretary and stop",
                        allowed_json(allowed)
                    ),
                    "only the secretary raises the allowance",
                ))
            } else {
                Ok(Value::Null)
            };
            let recorded = r.is_ok() || record(&w, actor, "review", args, idem, &r);
            if written.is_err() || !recorded || w.commit().is_err() {
                let _ = w.rollback();
                return Err(internal("the round could not be recorded; nothing was run"));
            }
            r?;
            (used + 1, allowed)
        };
        let a = super::review::run(&plan, &places, &snap, design);
        let findings = a
            .findings
            .map(|[b, ma, mi]| json!({"blocker": b, "major": ma, "minor": mi}));
        let mut answer = json!({
            "verdict": if a.established { "passed" } else { "not-established" },
            "kind": mode,
            "reader": a.reader,
            "round": round,
            "allowed": allowed_json(allowed),
            "base": snap.base,
            "head": snap.head,
            "findings": findings,
            "text": a.text,
        });
        if let Some(f) = a.fallback_reason {
            answer["fallback_reason"] = json!(f);
        }
        if let Some(r) = a.reason {
            answer["reason"] = json!(r);
        }
        let w = self.writer();
        fail(&w, Ok(answer))
    }
}

// ---- the entrances ----------------------------------------------------------------------------

/// Where the door listens (spec section 2).
pub struct Config {
    /// `None` with `MAKIBA_DOOR=none`: no socket is opened.
    pub socket: Option<PathBuf>,
    pub port: u16,
    pub person_key: PathBuf,
    /// `MAKIBA_LAUNCH_AGENT`: the agent module of the launches `dispatch` makes.
    pub agent: String,
}

/// The test seam of `dispatch` (spec section 2): `claude_code` unless the variable names `fake`.
pub const LAUNCH_AGENT_ENV: &str = "MAKIBA_LAUNCH_AGENT";

impl Config {
    pub fn from_env() -> Result<Config, String> {
        Ok(Config {
            socket: if crate::cli::door_off() {
                None
            } else {
                Some(crate::cli::door_path().ok_or(
                    "neither MAKIBA_DOOR nor XDG_RUNTIME_DIR is set: no place for the door",
                )?)
            },
            port: crate::cli::port()?,
            person_key: crate::cli::person_key_path(),
            agent: match std::env::var(LAUNCH_AGENT_ENV)
                .ok()
                .filter(|v| !v.is_empty())
            {
                None => "claude_code".into(),
                Some(a) if ["claude_code", "fake"].contains(&a.as_str()) => a,
                Some(a) => {
                    return Err(format!(
                        "{LAUNCH_AGENT_ENV} is claude_code or fake, not {a:?}"
                    ));
                }
            },
        })
    }
}

/// A door that listens. Dropping it removes the socket file; the listening threads end with the
/// process.
pub struct Running {
    pub socket: Option<PathBuf>,
    pub addr: SocketAddr,
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(socket) = &self.socket {
            let _ = std::fs::remove_file(socket);
        }
    }
}

fn euid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

/// Bind the socket. A file already there is removed only when it is a socket of this user that
/// nothing answers on; anything else stops the start.
fn bind_socket(path: &Path) -> Result<UnixListener, String> {
    let shown = path.display();
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    match std::fs::symlink_metadata(path) {
        Ok(m) => {
            if !m.file_type().is_socket() || m.uid() != euid() {
                return Err(format!("{shown} exists and is not this user's socket"));
            }
            match UnixStream::connect(path) {
                Ok(_) => return Err(format!("something already answers on {shown}")),
                Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                    std::fs::remove_file(path)
                        .map_err(|e| format!("cannot remove the stale {shown}: {e}"))?;
                }
                Err(e) => return Err(format!("cannot tell whether {shown} is in use: {e}")),
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("cannot look at {shown}: {e}")),
    }
    let l = UnixListener::bind(path).map_err(|e| format!("cannot bind {shown}: {e}"))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("cannot chmod {shown}: {e}"))?;
    Ok(l)
}

/// Start both entrances, after the person key exists (spec section 2).
pub fn start(cfg: &Config, db: &Path, base: Arc<dyn Base>, hub: Hub) -> Result<Running, String> {
    let w = StateWriter::open(db).map_err(|e| format!("cannot open {}: {e}", db.display()))?;
    super::person_key::ensure(&w, &cfg.person_key)?;
    let tcp = TcpListener::bind(("127.0.0.1", cfg.port))
        .map_err(|e| format!("cannot listen on 127.0.0.1:{}: {e}", cfg.port))?;
    let addr = tcp.local_addr().map_err(|e| e.to_string())?;
    let unix = match &cfg.socket {
        Some(path) => Some(bind_socket(path)?),
        None => {
            eprintln!("makibad: MAKIBA_DOOR=none: the door's socket is off");
            None
        }
    };
    let running = Running {
        socket: cfg.socket.clone(),
        addr,
    };
    let door = Arc::new(Door::new(w, base, cfg.agent.clone()));
    let d = Arc::clone(&door);
    if let Some(unix) = unix {
        std::thread::spawn(move || {
            for s in unix.incoming().flatten() {
                let d = Arc::clone(&d);
                std::thread::spawn(move || serve_socket(&d, s));
            }
        });
    }
    std::thread::spawn(move || {
        for s in tcp.incoming().flatten() {
            let (d, hub) = (Arc::clone(&door), hub.clone());
            std::thread::spawn(move || serve_http(&d, &hub, s));
        }
    });
    Ok(running)
}

#[cfg(target_os = "linux")]
fn peer_uid(s: &UnixStream) -> Option<u32> {
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: the buffer is a ucred of the length given, owned by this frame.
    let rc = unsafe {
        libc::getsockopt(
            s.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    (rc == 0).then_some(cred.uid)
}

#[cfg(not(target_os = "linux"))]
fn peer_uid(s: &UnixStream) -> Option<u32> {
    let (mut uid, mut gid) = (0, 0);
    // SAFETY: both out-parameters are owned by this frame.
    let rc = unsafe { libc::getpeereid(s.as_raw_fd(), &mut uid, &mut gid) };
    (rc == 0).then_some(uid)
}

/// The person's socket: refuse another user id before reading, then one line in, one out.
fn serve_socket(door: &Door, mut s: UnixStream) {
    if peer_uid(&s) != Some(euid()) {
        return;
    }
    let _ = s.set_read_timeout(Some(IO_LIMIT));
    let _ = s.set_write_timeout(Some(IO_LIMIT));
    let mut buf = Vec::new();
    let read = BufReader::new(&s)
        .take(LINE_MAX as u64 + 1)
        .read_until(b'\n', &mut buf);
    let r = match read {
        Err(e) => Err(usage(format!("cannot read the request: {e}"))),
        Ok(_) if buf.len() > LINE_MAX => Err(usage("the request is longer than 1 MiB")),
        Ok(_) => match serde_json::from_slice::<Value>(&buf) {
            Err(e) => Err(usage(format!("the request is not JSON: {e}"))),
            Ok(req) if req["v"] != json!(1) => Err(usage("the request's v is not 1")),
            Ok(req) => door.handle(
                req["key"].as_str(),
                req["verb"].as_str().unwrap_or_default(),
                req.get("args").cloned().unwrap_or_else(|| json!({})),
                req["idem"].as_str(),
                false,
            ),
        },
    };
    // Issue 340: the build makibad runs, so that a client of another build can say so.
    let mut answer = line(&r);
    if let Some(build) = super::exe::running_digest() {
        answer["build"] = build.into();
    }
    let _ = writeln!(s, "{answer}");
}

/// Does the connection ask for the channel path? Looks at its first bytes without taking them.
fn asks_channel(s: &TcpStream) -> bool {
    let want = format!("GET {} ", crate::channel::PATH);
    let want = want.as_bytes();
    let mut b = [0u8; 64];
    let until = Instant::now() + IO_LIMIT;
    loop {
        let n = match s.peek(&mut b) {
            Ok(0) | Err(_) => return false,
            Ok(n) => n,
        };
        if n >= want.len() || !want.starts_with(&b[..n]) {
            return b[..n].starts_with(want);
        }
        if Instant::now() > until {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn respond(s: &mut TcpStream, status: &str, body: &str) {
    let kind = if body.is_empty() {
        ""
    } else {
        "Content-Type: application/json\r\n"
    };
    let _ = write!(
        s,
        "HTTP/1.1 {status}\r\n{kind}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = s.flush();
}

fn rpc_error(id: &Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// The agents' entrance: one HTTP request per connection, `POST /mcp` with one JSON-RPC message.
fn serve_http(door: &Door, hub: &Hub, mut s: TcpStream) {
    let _ = s.set_read_timeout(Some(IO_LIMIT));
    let _ = s.set_write_timeout(Some(IO_LIMIT));
    if asks_channel(&s) {
        hub.serve(s);
        return;
    }
    let Ok((mut buf, n)) = crate::channel::read_head(&mut s) else {
        return;
    };
    let mut headers = [httparse::EMPTY_HEADER; 32];
    let mut req = httparse::Request::new(&mut headers);
    if !matches!(req.parse(&buf[..n]), Ok(httparse::Status::Complete(_))) {
        return respond(&mut s, "400 Bad Request", "");
    }
    if req.path != Some(MCP_PATH) {
        return respond(&mut s, "404 Not Found", "");
    }
    if req.method != Some("POST") {
        return respond(&mut s, "405 Method Not Allowed", "");
    }
    let header = |name: &str| {
        req.headers
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case(name))
            .and_then(|h| std::str::from_utf8(h.value).ok())
    };
    let Some(len) = header("content-length").and_then(|v| v.trim().parse::<usize>().ok()) else {
        return respond(&mut s, "411 Length Required", "");
    };
    if len > LINE_MAX {
        return respond(&mut s, "413 Content Too Large", "");
    }
    let token = header("authorization")
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_string);
    let mut body = buf.split_off(n);
    while body.len() < len {
        let mut chunk = [0u8; 8192];
        match s.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(k) => body.extend_from_slice(&chunk[..k]),
        }
    }
    body.truncate(len);
    let Ok(msg) = serde_json::from_slice::<Value>(&body) else {
        let e = rpc_error(&Value::Null, -32700, "parse error");
        return respond(&mut s, "200 OK", &e.to_string());
    };
    let Some(id) = msg.get("id").cloned() else {
        // A notification or a response: nothing to answer.
        return respond(&mut s, "202 Accepted", "");
    };
    let answer = mcp(door, token.as_deref(), &id, &msg);
    respond(&mut s, "200 OK", &answer.to_string());
}

fn mcp(door: &Door, token: Option<&str>, id: &Value, msg: &Value) -> Value {
    let method = msg["method"].as_str().unwrap_or_default();
    let params = &msg["params"];
    let ok = |result: Value| json!({"jsonrpc": "2.0", "id": id, "result": result});
    if method == "tools/call" {
        let name = params["name"].as_str().unwrap_or_default();
        let mut args = params["arguments"].as_object().cloned().unwrap_or_default();
        let idem = args
            .remove("idem")
            .and_then(|v| v.as_str().map(str::to_string));
        let r = door.handle(token, name, Value::Object(args), idem.as_deref(), true);
        let (text, is_error) = match &r {
            Ok(v) => (v.to_string(), false),
            Err(e) => (error_json(e).to_string(), true),
        };
        return ok(json!({"content": [{"type": "text", "text": text}], "isError": is_error}));
    }
    let Some((_, role)) = door.caller(token) else {
        return rpc_error(id, -32001, "denied: no valid token");
    };
    match method {
        "initialize" => {
            let want = params["protocolVersion"].as_str().unwrap_or_default();
            let version = PROTOCOLS
                .iter()
                .find(|p| **p == want)
                .unwrap_or(&PROTOCOLS[0]);
            ok(json!({
                "protocolVersion": version,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "makiba", "version": env!("CARGO_PKG_VERSION")},
            }))
        }
        "ping" => ok(json!({})),
        "tools/list" => {
            let tools: Vec<Value> = tools_of(role).into_iter().map(tool).collect();
            ok(json!({"tools": tools}))
        }
        _ => rpc_error(id, -32601, "method not found"),
    }
}

/// A tool's definition: its name, what it does, and its arguments.
fn tool(v: Verb) -> Value {
    let text = json!({"type": "string"});
    let (description, mut props, required): (&str, Map<String, Value>, &[&str]) = match v {
        Verb::Send => (
            "Send a message to another agent. typed: type it into a worker's or the curator's terminal instead of the channel.",
            Map::from_iter([
                ("to".into(), text.clone()),
                ("text".into(), text.clone()),
                ("typed".into(), json!({"type": "boolean"})),
            ]),
            &["to", "text"],
        ),
        Verb::Report => (
            "Report on a task to the secretary: start, done or blocked, with text.",
            Map::from_iter([
                (
                    "state".into(),
                    json!({"type": "string", "enum": ["start", "done", "blocked"]}),
                ),
                ("task".into(), text.clone()),
                ("text".into(), text.clone()),
            ]),
            &["state", "task", "text"],
        ),
        Verb::Close => (
            "Close a task's run. Refused (conflict) before its done report unless abandon is true, which marks the run abandoned (give its reason). A run whose current pull request merged, with a known limit the person accepted, closes as completed with limit (the review finding, one line), reason (one line) and pending: the entry the person answered yes whose question is \"Accept as a known limit of <task_id> and close it as completed: <limit>\" and whose why is the reason.",
            Map::from_iter([
                ("task_id".into(), text.clone()),
                ("abandon".into(), json!({"type": "boolean"})),
                ("reason".into(), text.clone()),
                ("limit".into(), text.clone()),
                ("pending".into(), json!({"type": "integer"})),
            ]),
            &["task_id"],
        ),
        Verb::Show => (
            "The full body of a message you sent or received, by its id.",
            Map::from_iter([("id".into(), json!({"type": "integer"}))]),
            &["id"],
        ),
        Verb::Pr => (
            "Record that a pull request was opened or a fix pushed, and have makibad watch the CI of that head. A new head replaces the watch of the older one. rerun: after `gh run rerun <run> --failed`, watch the same head again and get a notice of the rerun's result.",
            Map::from_iter([
                (
                    "state".into(),
                    json!({"type": "string", "enum": ["opened", "pushed", "rerun"]}),
                ),
                ("repo".into(), text.clone()),
                ("pr".into(), json!({"type": "integer"})),
                ("head".into(), text.clone()),
                ("task".into(), text.clone()),
            ]),
            &["state", "repo", "pr", "head", "task"],
        ),
        Verb::Push => (
            "Have makibad push a worker's branch: the run's worktree, on its branch, clean, pushed as its head to the same branch at origin, never forced. With head (the full id of the branch's test commit, the one with the Acceptance-Test trailer), that commit alone is pushed. A head past the test commit is refused until a failed CI run is recorded at the test commit on one of the task's pull requests. Answers accepted; a notice tells you the head pushed (then record it with pr) or why it failed.",
            Map::from_iter([("task".into(), text.clone()), ("head".into(), text.clone())]),
            &["task"],
        ),
        Verb::Update => (
            "Have makibad bring a pushed worker branch up to date with its base: fetch, then merge origin/<base> into the run's worktree (a merge commit, never a rebase) and push it, never forced. Refused when the worktree has uncommitted changes, is on another branch, the branch is not at origin or moved there; a conflict is undone and its files named. Answers accepted; a notice tells you the new head (then record it with pr pushed) or why it failed.",
            Map::from_iter([("task".into(), text.clone()), ("base".into(), text.clone())]),
            &["task", "base"],
        ),
        Verb::Inbox => (
            "Your unconfirmed messages, each in its wrapper, oldest first. Reading them confirms them.",
            Map::new(),
            &[],
        ),
        Verb::Dispatch => (
            "Dispatch a task: a queued run and its first launch. kind is code or document; project is required for code. The brief is the task text itself. issue: the Issue number the task works on, so that a range does not dispatch it again. design_first: true when the person must approve the task's design first (a trust boundary: git, the sandbox, keys, the push line, the merge gate): makibad then refuses the task's test commit (Acceptance-Test), at the push and in the worker's commit hook, until a pending entry of the task with stop_kind design is answered yes by the person.",
            Map::from_iter([
                (
                    "kind".into(),
                    json!({"type": "string", "enum": ["code", "document"]}),
                ),
                ("task_id".into(), text.clone()),
                ("title".into(), text.clone()),
                ("brief".into(), text.clone()),
                ("project".into(), text.clone()),
                ("model".into(), text.clone()),
                ("branch".into(), text.clone()),
                ("issue".into(), json!({"type": "integer"})),
                ("design_first".into(), json!({"type": "boolean"})),
            ]),
            &["kind", "task_id", "title", "brief"],
        ),
        Verb::Status => (
            "The dispatcher's state, as the person's makiba status --json shows it.",
            Map::new(),
            &[],
        ),
        Verb::Peek => (
            "The last lines of a worker's screen, read by makibad (default 40, at most 1000).",
            Map::from_iter([
                ("task_id".into(), text.clone()),
                ("lines".into(), json!({"type": "integer"})),
            ]),
            &["task_id"],
        ),
        Verb::Pending => (
            "Keep the pending table of questions: add (task, question, and its context: background, if_yes, if_no, recommend, why), escalate or withdraw (id), list. An entry is escalated only with all five parts; recommend may say there is none, with the reason in why. add of the same open question fills in the parts given. For a range task's stop, add also stop_kind (design, scope, order, naming, other) and recommend_answer (yes, no or none), the recommendation as an answer.",
            Map::from_iter(
                [
                    (
                        "action".into(),
                        json!({"type": "string", "enum": ["add", "escalate", "withdraw", "list"]}),
                    ),
                    ("id".into(), json!({"type": "integer"})),
                    ("task".into(), text.clone()),
                    ("question".into(), text.clone()),
                    (
                        "stop_kind".into(),
                        json!({"type": "string", "enum": ["design", "scope", "order", "naming", "other"]}),
                    ),
                    (
                        "recommend_answer".into(),
                        json!({"type": "string", "enum": ["yes", "no", "none"]}),
                    ),
                ]
                .into_iter()
                .chain(CONTEXT_PARTS.map(|p| (p.into(), text.clone()))),
            ),
            &["action"],
        ),
        Verb::Answer => (
            "Answer a pending entry yes or no. A question's answer goes to its worker (typed: typed into its terminal); a permission prompt gets one key (yes: the one-time yes, no: escape), only while the recorded prompt is still on the screen.",
            Map::from_iter([
                ("id".into(), json!({"type": "integer"})),
                (
                    "answer".into(),
                    json!({"type": "string", "enum": ["yes", "no"]}),
                ),
                ("note".into(), text.clone()),
                ("typed".into(), json!({"type": "boolean"})),
            ]),
            &["id", "answer"],
        ),
        Verb::Renew => (
            "Ask to be replaced by a fresh secretary that resumes from the handover text. Wired in M6-5: refused until then.",
            Map::from_iter([("handover".into(), text.clone())]),
            &["handover"],
        ),
        Verb::Handover => (
            "Store a handover without being replaced: what was decided by word and the judgments in progress, for the secretary that makiba up launches next. Not empty, at most 32 KiB; refused while a renewal is under way. Call it when the person says the organization goes down.",
            Map::from_iter([("handover".into(), text.clone())]),
            &["handover"],
        ),
        Verb::Note => (
            "Save a raw note for the curator: makibad writes it to <knowledge>/raw/<date>-<topic>[-n].md with the line Task: <task> first. topic is English kebab-case; text must have the four headings ## Facts, ## Judgment, ## Grounds, ## Where it applies. Returns the file.",
            Map::from_iter([
                ("task".into(), text.clone()),
                ("topic".into(), text.clone()),
                ("text".into(), text.clone()),
            ]),
            &["task", "topic", "text"],
        ),
        Verb::Project => (
            "Register a project and have makibad clone its GitHub repository, or fetch it and fast-forward its default branch when the clone exists. repo is OWNER/NAME only. Answers accepted; a notice says when it is done or why it failed. Dispatch into the project after that notice.",
            Map::from_iter([("slug".into(), text.clone()), ("repo".into(), text.clone())]),
            &["slug", "repo"],
        ),
        Verb::Candidate => (
            "Record the person's decision on a skill candidate: makibad writes status (approved, rejected, deferred, or merged-into-<skill> for into-<skill>), decided (today) and reason into the open entry of pattern (pending or deferred) in <knowledge>/skill-candidates.md, changing nothing else. deferred holds a pending entry; it is decided later with one of the other three. reason is one line. An unknown pattern is not_found; a pattern with no open entry, or a deferred entry deferred again, is conflict.",
            Map::from_iter([
                ("pattern".into(), text.clone()),
                ("decision".into(), text.clone()),
                ("reason".into(), text.clone()),
            ]),
            &["pattern", "decision", "reason"],
        ),
        Verb::Goal => (
            "Read, check or place a project's goal ledger (docs/goals.md). action show: the placed text, its clauses, who placed it and when, or unset, or the reason it cannot be read. action check: validate a draft in text with the ledger's own validator, writing nothing. action place: validate text, and only a valid one is written to the ledger folder (the previous ledger is kept beside it as <repo>.md.prev, one generation), then read back; an invalid draft is usage and nothing is written. repo is OWNER/NAME. Place only on the person's word.",
            Map::from_iter([
                (
                    "action".into(),
                    json!({"type": "string", "enum": ["show", "check", "place"]}),
                ),
                ("repo".into(), text.clone()),
                ("text".into(), text.clone()),
            ]),
            &["action", "repo"],
        ),
        Verb::Range => (
            "Ranges of work the person approves once; makibad then dispatches each range's next Issue itself when a slot frees. propose (project, issues and/or takes, at_once, budget_tasks, brief_head, replaces; budget_hours: hours from approval after which nothing new goes in; budget_fixes: fixes pushed to a task's pull request after its first head before it is a stop, default 3 (a head whose only new commit is a clean merge of the base is no fix); follow_ups: inside (default: an Issue created after the approval with a line Follow-up of #<n> naming an Issue of the range is in it, unless a line Needs host: names a host the project does not list or a line Stop line: names one) or ask; design_ahead: true lets a candidate whose open dependencies are range tasks with a pull request go in for its design only, parked until they land; design_first: true when the Issues touch a trust boundary: each task's test commit is refused, at the push and in the worker's commit hook, until a pending entry of the task with stop_kind design is answered yes by the person): a draft, read by makibad. pause, end (id), skip (id, issue, reason). approve and resume are the person's alone (approve: id, read and approval_mode, the read and the merge approval mode shown; one that is no longer the newest read or makibad's mode is stale). makibad pauses a range itself after two failed launches or two refused pull requests in a row, and writes the stops of the gate and the budgets as pending entries for the person.",
            Map::from_iter([
                (
                    "action".into(),
                    json!({"type": "string", "enum": ["propose", "approve", "pause", "resume", "end", "skip"]}),
                ),
                ("id".into(), json!({"type": "integer"})),
                ("project".into(), text.clone()),
                (
                    "issues".into(),
                    json!({"type": "array", "items": {"type": "integer"}}),
                ),
                ("takes".into(), text.clone()),
                ("at_once".into(), json!({"type": "integer"})),
                ("budget_tasks".into(), json!({"type": "integer"})),
                ("budget_hours".into(), json!({"type": "number"})),
                ("budget_fixes".into(), json!({"type": "integer"})),
                (
                    "follow_ups".into(),
                    json!({"type": "string", "enum": ["inside", "ask"]}),
                ),
                ("design_ahead".into(), json!({"type": "boolean"})),
                ("design_first".into(), json!({"type": "boolean"})),
                ("brief_head".into(), text.clone()),
                ("replaces".into(), json!({"type": "integer"})),
                ("issue".into(), json!({"type": "integer"})),
                ("reason".into(), text.clone()),
                ("read".into(), json!({"type": "integer"})),
                (
                    "approval_mode".into(),
                    json!({"type": "string", "enum": ["human", "acceptance"]}),
                ),
            ]),
            &["action"],
        ),
        Verb::Review => (
            "A worker: have a second reader review your task, once with kind design (the design as note) before code, and with kind diff after your last commit, before report done. makibad runs the reader (another model's program when usable, else the same model in a fresh session) on your committed head and answers with verdict (passed or not-established), reader, round, allowed, findings (blocker, major, minor) and text. Rounds are capped: design 1, diff 3; over the cap it is refused with REVIEW_ROUND_DENIED. The secretary: set a task's diff allowance with task, allowed (a number or unlimited) and reason.",
            Map::from_iter([
                (
                    "kind".into(),
                    json!({"type": "string", "enum": ["diff", "design"]}),
                ),
                ("note".into(), text.clone()),
                ("task".into(), text.clone()),
                ("allowed".into(), json!({"type": ["integer", "string"]})),
                ("reason".into(), text.clone()),
            ]),
            &[],
        ),
        Verb::Merge => (
            "Ask makibad to merge a pull request: action request with repo (owner/name), pr, head (the full 40-character head of the CI notice) and method (squash, merge or rebase; default squash). makibad runs the acceptance gate at that head; refused: the verdict, nothing runs. Allowed under merge_approval_mode acceptance with no warning: accepted with the attempt's number, and makibad itself lands the pull request bound to that head (or adds it to the merge queue where the branch requires one); a notice says merged, enqueued, refused by GitHub or not known. Nothing is retried: ask again once the cause is dealt with. Under merge_approval_mode human, or when the gate warns, the answer is pending with a pending_id: tell the person the pull request, the warnings and that they approve it in their shell with makiba merge approve <id>; only their key lets it land, a word in the conversation does not, and answer cannot. A new head, a replacement pull request or a landing withdraws the entry. action withdraw with id (and an optional reason) withdraws a pending merge. Refused (conflict) for a pull request makibad does not follow, or one with an act awaiting its landing. action order records a merge order the person decided: repo, pr (the pull request that waits), exactly one of after_pr or after_issue (what must land first, in the same repository) and reason (the person's decision); the gate then refuses that pull request under either mode until the other has landed. The same order again is the first one; no action lifts it.",
            Map::from_iter([
                (
                    "action".into(),
                    json!({"type": "string", "enum": ["request", "order", "withdraw"]}),
                ),
                ("after_pr".into(), json!({"type": "integer"})),
                ("after_issue".into(), json!({"type": "integer"})),
                ("repo".into(), text.clone()),
                ("pr".into(), json!({"type": "integer"})),
                ("head".into(), text.clone()),
                (
                    "method".into(),
                    json!({"type": "string", "enum": ["squash", "merge", "rebase"]}),
                ),
                ("id".into(), json!({"type": "integer"})),
                ("reason".into(), text.clone()),
            ]),
            // Each action checks its own (request: repo, pr, head; order: repo, pr, after_pr or
            // after_issue, reason; withdraw: id).
            &[],
        ),
        _ => ("Not built yet.", Map::new(), &[]),
    };
    props.insert(
        "idem".into(),
        json!({"type": "string", "description": "A token of your own: the same idem twice returns the first answer again."}),
    );
    json!({
        "name": v.as_str(),
        "description": description,
        "inputSchema": {"type": "object", "properties": props, "required": required},
    })
}

#[cfg(test)]
mod tests {
    use super::{CONTEXT_PARTS, Verb, curator_at_work, dispatch_issue, json, save_note, tool};

    /// Issue 317: the project becomes a path in the read, so a dispatch that `dispatch` would
    /// refuse for its project or kind reads nothing, neither git nor GitHub.
    #[test]
    fn only_a_dispatch_with_a_valid_project_and_kind_reads_its_issue() {
        let a = |kind: &str, project: Option<&str>| {
            let mut v = json!({"kind": kind, "issue": 7});
            if let Some(p) = project {
                v["project"] = json!(p);
            }
            dispatch_issue(&v)
        };
        assert_eq!(a("code", Some("p")), Some(7));
        assert_eq!(a("document", None), Some(7));
        for p in ["../x", "/abs", ".hidden", "-x", "a/b"] {
            assert_eq!(a("code", Some(p)), None, "{p}");
        }
        assert_eq!(a("other", Some("p")), None);
        assert_eq!(
            dispatch_issue(&json!({"kind": "code", "project": "p", "issue": 0})),
            None
        );
    }

    #[test]
    fn a_curator_key_not_revoked_means_the_curator_is_at_work() {
        let dir = tempfile::tempdir().unwrap();
        let w = crate::db::writer::StateWriter::open(&dir.path().join("state.db")).unwrap();
        assert!(!curator_at_work(&w).unwrap());
        w.conn()
            .execute(
                "INSERT INTO agents (agent_id, role, key_sha256) VALUES ('curator', 'curator', 'k')",
                [],
            )
            .unwrap();
        assert!(curator_at_work(&w).unwrap());
        w.conn()
            .execute("UPDATE agents SET revoked_at = 'now'", [])
            .unwrap();
        assert!(!curator_at_work(&w).unwrap());
    }

    #[test]
    fn a_note_takes_the_first_name_free_in_raw_and_in_the_archive() {
        let dir = tempfile::tempdir().unwrap();
        let raw = dir.path().join("raw");
        std::fs::create_dir_all(raw.join("archive")).unwrap();
        std::fs::write(raw.join("archive/d-t.md"), "old").unwrap();
        std::fs::write(raw.join("d-t-2.md"), "kept").unwrap();
        assert_eq!(save_note(&raw, "d-t", "new\n").unwrap(), "d-t-3.md");
        assert_eq!(
            std::fs::read_to_string(raw.join("d-t-2.md")).unwrap(),
            "kept"
        );
        assert_eq!(
            std::fs::read_to_string(raw.join("d-t-3.md")).unwrap(),
            "new\n"
        );
        // The staging file is gone.
        let names: Vec<_> = std::fs::read_dir(&raw)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert!(names.iter().all(|n| !n.starts_with('.')), "{names:?}");
    }

    /// A design-only run's `report done` is refused while it waits, whoever reports it (Issue
    /// 284): a curator's or the person's done would let `close` end it before its dependency
    /// lands.
    #[test]
    fn a_design_only_runs_done_is_refused_for_every_role_while_it_waits() {
        use crate::db::writer::{NewEvent, RunUpsert};
        use crate::spec::{ErrorKind, Role};
        let dir = tempfile::tempdir().unwrap();
        let w = crate::db::writer::StateWriter::open(&dir.path().join("state.db")).unwrap();
        w.upsert_run(&RunUpsert {
            task_id: "r1-2",
            project_slug: "p",
            status: Some("in_use"),
            ..Default::default()
        })
        .unwrap();
        w.append_event(&NewEvent {
            kind: "belt_dispatched",
            actor: Some("makibad"),
            payload: Some(&json!({"range_id": 1, "issue": 2, "task_id": "r1-2",
                                  "design_only": true, "waits_for": [1],
                                  "waits_tasks": ["r1-1"]})),
            run_task_id: Some("r1-2"),
            ..Default::default()
        })
        .unwrap();
        let done = json!({"state": "done", "task": "r1-2", "text": "Done."});
        for (actor, role) in [
            ("curator", Role::Curator),
            ("person", Role::Person),
            ("w-r1-2", Role::Worker),
        ] {
            let e = super::report(&w, actor, role, &done)
                .expect_err("a done report of a design-only run waiting for #1");
            assert_eq!(e.kind, ErrorKind::Conflict, "{actor}: {e:?}");
        }
        let reports: i64 = w
            .conn()
            .query_row(
                "SELECT count(*) FROM messages WHERE kind = 'report_done'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(reports, 0);
    }

    /// The `pending` tool offers the five context parts as optional text (Issue 255).
    #[test]
    fn the_pending_tool_takes_the_context_parts() {
        let t = tool(Verb::Pending);
        for part in CONTEXT_PARTS {
            assert_eq!(
                t["inputSchema"]["properties"][part],
                json!({"type": "string"}),
                "{t}"
            );
            assert!(
                !t["inputSchema"]["required"]
                    .as_array()
                    .unwrap()
                    .contains(&json!(part)),
                "{t}"
            );
        }
    }
}

#[cfg(test)]
mod merge_order_tests {
    use super::{ErrorKind, json, merge_order_set};

    /// Issue 338, diff review round 1: when two active projects record the order's repository, the
    /// Issue's project is not chosen silently; the order is refused and nothing is written.
    #[test]
    fn an_issue_order_whose_repository_two_projects_record_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let w = crate::db::writer::StateWriter::open(&dir.path().join("state.db")).unwrap();
        for slug in ["a", "b"] {
            w.ensure_project(slug, None, Some("https://github.com/o/r"))
                .unwrap();
        }
        let args = json!({"action": "order", "repo": "o/r", "pr": 2, "after_issue": 1,
                          "reason": "the person's condition"});
        let e = merge_order_set(&w, "secretary", &args, None).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Conflict, "{e:?}");
        let n: i64 = w
            .conn()
            .query_row(
                "SELECT count(*) FROM events WHERE kind = 'merge_order_set'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0);
        // An order after a pull request needs no project.
        let args = json!({"action": "order", "repo": "o/r", "pr": 2, "after_pr": 1,
                          "reason": "the person's condition"});
        let v = merge_order_set(&w, "secretary", &args, None).unwrap();
        assert_eq!(v["status"], json!("recorded"), "{v}");
    }

    /// Issue 338, diff review round 3: a repository the gate would refuse as malformed is refused
    /// for an order too, so no order is recorded that never matches the pull request it names.
    #[test]
    fn an_order_with_a_malformed_repository_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let w = crate::db::writer::StateWriter::open(&dir.path().join("state.db")).unwrap();
        for repo in ["o/r ", " o/r", "o/r/x", "o r/r", "o/", "/r", "o/r\n"] {
            let args = json!({"action": "order", "repo": repo, "pr": 2, "after_pr": 1,
                              "reason": "the person's condition"});
            let e = merge_order_set(&w, "secretary", &args, None).unwrap_err();
            assert_eq!(e.kind, ErrorKind::Usage, "{repo:?}: {e:?}");
        }
        let n: i64 = w
            .conn()
            .query_row(
                "SELECT count(*) FROM events WHERE kind = 'merge_order_set'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0);
    }
}
