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
use std::sync::{Arc, Mutex};
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
const CONFIG_KEYS: [&str; 4] = [
    "max_concurrent_workers",
    "merge_approval_mode",
    "conversation_language",
    "artifact_language",
];

/// What a request comes to: the response's `result`, or its `error`.
pub type Response = Result<Value, DoorError>;

pub fn sha256_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes())
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

fn refuse(kind: ErrorKind, message: impl Into<String>, hint: impl Into<String>) -> DoorError {
    DoorError {
        kind,
        message: message.into(),
        hint: hint.into(),
    }
}

fn internal(e: impl std::fmt::Display) -> DoorError {
    refuse(
        ErrorKind::Internal,
        e.to_string(),
        "makibad's log has the detail",
    )
}

fn usage(message: impl Into<String>) -> DoorError {
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

/// A verb that has a tool for agents (spec section 12: all but `up`, `down`, `config`).
pub fn has_tool(v: Verb) -> bool {
    !matches!(v, Verb::Up | Verb::Down | Verb::Config)
}

/// The tools a role is listed: its verbs (spec section 4) that have a tool.
pub fn tools_of(role: Role) -> Vec<Verb> {
    role.verbs()
        .iter()
        .copied()
        .filter(|v| has_tool(*v))
        .collect()
}

fn str_arg<'a>(args: &'a Value, name: &str) -> Result<&'a str, DoorError> {
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
}

impl Door {
    pub fn new(w: StateWriter, base: Arc<dyn Base>, agent: String) -> Door {
        Door {
            w: Mutex::new(w),
            base,
            agent,
        }
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
        if v == Verb::Send && args.get("typed") == Some(&json!(true)) {
            return self.typed(&w, &actor, role, &args, idem);
        }
        // The verb's writes and its record are one transaction (a verb's own transaction joins it).
        // IMMEDIATE takes the write lock first: a deferred one that reads, then writes after
        // makibad's loop has written, fails at once with `database is locked` (WAL snapshot).
        let begin = || w.conn().execute_batch("BEGIN IMMEDIATE");
        if let Err(e) = begin() {
            return Err(internal(e));
        }
        let r = run(&w, &self.agent, &actor, role, v, &args);
        if r.is_err() && w.rollback().is_ok() {
            let _ = begin();
        }
        // A verb's writes stand only with their record: without it a retry would repeat them.
        // A transaction SQLite rolled back by itself (a full disk) is gone: nothing was done.
        if !record(&w, &actor, verb, &args, idem, &r) || w.conn().is_autocommit() {
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

    /// `send` with `typed` (spec section 7). The command is recorded before anything is typed, as
    /// not finished, so a retry of the same `idem` after a crash answers that the outcome is not
    /// known instead of typing again. `send_typed` records the message before it types.
    fn typed(
        &self,
        w: &StateWriter,
        actor: &str,
        role: Role,
        args: &Value,
        idem: Option<&str>,
    ) -> Response {
        let (to, text) = match (str_arg(args, "to"), str_arg(args, "text")) {
            (Ok(to), Ok(text)) => (to, text),
            (Err(e), _) | (_, Err(e)) => return Err(e),
        };
        if let Err(e) = recipient(w, role, to) {
            let r = Err(e);
            let _ = record(w, actor, "send", args, idem, &r);
            return r;
        }
        let unknown: Response = Err(internal(
            "the outcome of an earlier typed send with this idem is not known; it is not sent again",
        ));
        let Some(id) = insert_command(w, actor, "send", args, idem, "accepted", &unknown) else {
            return Err(internal("cannot record the request"));
        };
        let r = super::delivery::send_typed(w, &*self.base, actor, to, "message", text)
            .map(|id| json!({"id": id}))
            .map_err(|e| match e {
                super::delivery::TypedError::Usage(m) => usage(m),
                super::delivery::TypedError::Conflict(m) => refuse(
                    ErrorKind::Conflict,
                    m,
                    "nothing was typed or stored; send again later or without typed",
                ),
                super::delivery::TypedError::Failed(m) => internal(m),
            });
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
        r
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

fn record(
    w: &StateWriter,
    actor: &str,
    verb: &str,
    args: &Value,
    idem: Option<&str>,
    r: &Response,
) -> bool {
    // `dispatch` and `close` return at `accepted` and finish later (spec section 5.2); a close
    // of a run closed already is `done` again.
    let status = match (verb, r) {
        ("dispatch", Ok(_)) => Some("accepted"),
        ("close", Ok(v)) if v.get("closed").is_none() => Some("accepted"),
        _ => status_of(r),
    };
    match status {
        Some(status) => insert_command(w, actor, verb, args, idem, status, r).is_some(),
        None => true,
    }
}

fn run(w: &StateWriter, agent: &str, actor: &str, role: Role, v: Verb, args: &Value) -> Response {
    match v {
        // The tool for the secretary is #42's; until then only the person's command is built.
        Verb::Dispatch if role == Role::Person => dispatch(w, agent, actor, args),
        Verb::Send => {
            let to = str_arg(args, "to")?;
            let text = str_arg(args, "text")?;
            recipient(w, role, to)?;
            let id = store_message(w, actor, to, "message", None, text)?;
            Ok(json!({"id": id}))
        }
        Verb::Report => report(w, actor, role, args),
        Verb::Close => close(w, actor, args),
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
        Verb::Config => config(args),
        Verb::Pr => pr(w, args),
        other => Err(internal(format!("{} is not built yet", other.as_str()))),
    }
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

/// `dispatch` (spec section 12): a `queued` run, its first `run_launch` row and `delegate_sent`.
/// The caller's transaction holds them together with the command's record.
fn dispatch(w: &StateWriter, agent: &str, actor: &str, args: &Value) -> Response {
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
    if run_exists(w, task)? {
        return Err(refuse(
            ErrorKind::Conflict,
            format!("task {task:?} was dispatched already"),
            "a task id is used once; dispatch under another id",
        ));
    }
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
            ..Default::default()
        })?;
        w.conn().execute(
            "INSERT INTO run_launch (run_id, spawn_id, kind, agent, model, brief, worktree)
             VALUES ((SELECT id FROM runs WHERE task_id = ?1), ?2, ?3, ?4, ?5, ?6, ?7)",
            params![task, spawn, kind, agent, opt("model"), brief, worktree],
        )?;
        w.append_event(&NewEvent {
            kind: "delegate_sent",
            actor: Some(actor),
            payload: Some(&json!({"task_id": task, "spawn_id": spawn, "kind": kind})),
            run_task_id: Some(task),
            ..Default::default()
        })?;
        Ok(())
    };
    store().map_err(internal)?;
    Ok(json!({"task_id": task, "spawn_id": spawn, "status": "queued"}))
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
    let kind = format!("report_{state}");
    let id = store_message(
        w,
        actor,
        crate::spec::AGENT_SECRETARY,
        &kind,
        Some(task),
        text,
    )?;
    if state == "done" {
        w.conn()
            .execute(
                "UPDATE runs SET status = 'review' WHERE task_id = ?1 AND status IN ('queued','in_use','suspended')",
                [task],
            )
            .map_err(internal)?;
    }
    Ok(json!({"id": id}))
}

/// `close` (spec section 12), the gate of Issue 36: refused without a `report_done` unless
/// `abandon`; a run closed already is `done` again. A launch under way is failed. Ending the
/// terminal and removing the working folder are Issue 37's.
fn close(w: &StateWriter, actor: &str, args: &Value) -> Response {
    let task = str_arg(args, "task_id")?;
    let abandon = args.get("abandon") == Some(&json!(true));
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
    if !closed && !done && !abandon {
        return Err(refuse(
            ErrorKind::Conflict,
            format!("task {task:?} has no done report"),
            "wait for the worker's report done, or close with --abandon",
        ));
    }
    if !abandon {
        unsaved_work(w, run)?;
    }
    let status = if done && !abandon {
        "completed"
    } else {
        "abandoned"
    };
    let store = || -> crate::db::Result<()> {
        if retry && !abandon {
            return Ok(());
        }
        w.conn().execute(
            "UPDATE runs SET status = ?1, completed_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
             WHERE id = ?2",
            params![status, run],
        )?;
        if retry {
            return Ok(());
        }
        // A launch under way would start a worker for a closed run (as `down` fails them).
        w.conn().execute(
            "UPDATE run_launch SET stage = 'failed', failed_reason = 'makiba close'
             WHERE run_id = ?1 AND stage NOT IN ('verified', 'failed')",
            [run],
        )?;
        w.append_event(&NewEvent {
            kind: "run_closed",
            actor: Some(actor),
            payload: Some(&json!({"task_id": task})),
            run_task_id: Some(task),
            ..Default::default()
        })?;
        Ok(())
    };
    store().map_err(internal)?;
    let status: String = w
        .conn()
        .query_row("SELECT status FROM runs WHERE id = ?1", [run], |r| r.get(0))
        .map_err(internal)?;
    Ok(json!({"task_id": task, "status": status}))
}

/// Refuse a close (without `--abandon`) while a code worktree of the run holds unsaved work.
fn unsaved_work(w: &StateWriter, run: i64) -> Result<(), DoorError> {
    let paths: Vec<String> = w
        .conn()
        .prepare("SELECT DISTINCT worktree FROM run_launch WHERE run_id = ?1 AND kind = 'code'")
        .and_then(|mut st| {
            st.query_map([run], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()
        })
        .map_err(internal)?;
    for p in paths.iter().map(Path::new).filter(|p| p.exists()) {
        if let Some(why) = super::close::unsaved(p) {
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

/// `pr opened|pushed` (spec section 12): the event, the older watches of the PR `superseded`
/// and the watch of the new head, as `prwatch::register` does in one transaction.
fn pr(w: &StateWriter, args: &Value) -> Response {
    let state = str_arg(args, "state")?;
    let kind = match state {
        "opened" => "pr_opened",
        "pushed" => "fix_pushed",
        _ => return Err(usage(format!("state is opened or pushed, not {state:?}"))),
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
    let r = super::prwatch::register(w, kind, task, repo, number, head).map_err(internal)?;
    Ok(json!({"watch_id": r.watch_id, "status": r.status}))
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

fn config(args: &Value) -> Response {
    let key = str_arg(args, "key")?;
    if !CONFIG_KEYS.contains(&key) {
        return Err(usage(format!(
            "no setting {key:?}; the settings are {}",
            CONFIG_KEYS.join(", ")
        )));
    }
    // ponytail: no store yet; M6-7 builds it and fills `value`.
    match str_arg(args, "action")? {
        "get" => Ok(json!({"key": key, "value": null})),
        "set" => Err(internal("config set is not built yet (M6-7)")),
        other => Err(usage(format!("action is get or set, not {other:?}"))),
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
    let _ = writeln!(s, "{}", line(&r));
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
            "Close a task's run. Refused (conflict) before its done report unless abandon is true, which marks the run abandoned.",
            Map::from_iter([
                ("task_id".into(), text.clone()),
                ("abandon".into(), json!({"type": "boolean"})),
            ]),
            &["task_id"],
        ),
        Verb::Show => (
            "The full body of a message you sent or received, by its id.",
            Map::from_iter([("id".into(), json!({"type": "integer"}))]),
            &["id"],
        ),
        Verb::Pr => (
            "Record that a pull request was opened or a fix pushed, and have makibad watch the CI of that head. A new head replaces the watch of the older one.",
            Map::from_iter([
                (
                    "state".into(),
                    json!({"type": "string", "enum": ["opened", "pushed"]}),
                ),
                ("repo".into(), text.clone()),
                ("pr".into(), json!({"type": "integer"})),
                ("head".into(), text.clone()),
                ("task".into(), text.clone()),
            ]),
            &["state", "repo", "pr", "head", "task"],
        ),
        Verb::Inbox => (
            "Your unconfirmed messages, each in its wrapper, oldest first. Reading them confirms them.",
            Map::new(),
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
