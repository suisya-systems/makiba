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
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
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
const CONFIG_KEYS: [&str; 6] = [
    "max_concurrent_workers",
    "merge_approval_mode",
    "conversation_language",
    "artifact_language",
    super::project::OWNERS_KEY,
    super::project::HOSTS_KEY,
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

/// A verb that has a tool for agents (spec section 12: all but `up`, `down`, `config`, `gate`).
pub fn has_tool(v: Verb) -> bool {
    !matches!(
        v,
        Verb::Up | Verb::Down | Verb::Config | Verb::Gate | Verb::Events
    )
}

/// The actor of a request that carries the merge gate's key (spec section 3).
pub const GATE_ACTOR: &str = "merge-gate";

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
    /// The SHA-256 of the merge gate's key, once it is written (spec section 3).
    gate_key: OnceLock<String>,
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
            gate_key: OnceLock::new(),
            state,
            review: super::review::Programs::from_env(),
            reviewing: Mutex::new(Default::default()),
        }
    }

    /// The readers of `review` (Issue 135): the other model's program and the same model's.
    pub fn with_review_programs(mut self, other: PathBuf, same: PathBuf) -> Door {
        self.review = super::review::Programs { other, same };
        self
    }

    /// The merge gate's key: its holder may ask `gate` and nothing else.
    pub fn set_gate_key(&self, key: &str) {
        let _ = self.gate_key.set(sha256_hex(key));
    }

    fn is_gate_key(&self, key: Option<&str>) -> bool {
        matches!((key, self.gate_key.get()), (Some(k), Some(sha)) if !k.is_empty() && sha256_hex(k) == *sha)
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
        if self.is_gate_key(key) {
            if verb != Verb::Gate.as_str() {
                let r = Err(refuse(
                    ErrorKind::Denied,
                    format!("{GATE_ACTOR} may only use gate"),
                    "",
                ));
                let _ = record(&w, GATE_ACTOR, verb, &args, idem, &r);
                return r;
            }
            if let Some(idem) = idem
                && let Some(first) = replay(&w, GATE_ACTOR, idem).map_err(internal)?
            {
                return first;
            }
            return self.gate(w, GATE_ACTOR, &args, idem);
        }
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
        if v == Verb::Review && role == Role::Worker {
            return self.review(w, &actor, &args, idem);
        }
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
                        Ok(Some((to.to_string(), text.to_string(), None)))
                    }),
                Verb::Answer => {
                    question(&w, &args).map(|q| q.map(|q| (q.to.clone(), q.body.clone(), Some(q))))
                }
                _ => Ok(None),
            };
            match target {
                Err(e) => {
                    let r = Err(e);
                    let _ = record(&w, &actor, verb, &args, idem, &r);
                    return r;
                }
                Ok(Some((to, text, q))) => {
                    return self.typed(
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
                    );
                }
                // An answer to a prompt types no text: the verb's own path.
                Ok(None) => {}
            }
        }
        // The verb's writes and its record are one transaction (a verb's own transaction joins it).
        // IMMEDIATE takes the write lock first: a deferred one that reads, then writes after
        // makibad's loop has written, fails at once with `database is locked` (WAL snapshot).
        let begin = || w.conn().execute_batch("BEGIN IMMEDIATE");
        if let Err(e) = begin() {
            return Err(internal(e));
        }
        // A decided candidate's file is replaced outside the transaction: its old bytes come back
        // when its record does not stand.
        let mut undo = None;
        let r = match v {
            Verb::Candidate => candidate(&w, &actor, &args, &mut undo),
            _ => run(&w, &*self.base, &self.agent, &actor, role, v, &args),
        };
        if r.is_err() && w.rollback().is_ok() {
            let _ = begin();
        }
        // A saved note is a file outside the transaction: it goes when its record does.
        let unsave = || {
            if let (Verb::Note, Ok(saved)) = (v, &r)
                && let Some(file) = saved["file"].as_str()
            {
                let _ = std::fs::remove_file(super::curator::knowledge_dir().join(file));
            }
            if let Some((path, old)) = &undo {
                let _ = replace_file(path, old);
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
    /// answer marks its entry answered there).
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
    ) -> Response {
        let unknown: Response = Err(internal(
            "the outcome of an earlier typed send with this idem is not known; it is not sent again",
        ));
        let Some(id) = insert_command(w, actor, verb, args, idem, "accepted", &unknown) else {
            return Err(internal("cannot record the request"));
        };
        let r = super::delivery::send_typed(w, &*self.base, actor, to, "message", text)
            .map_err(|e| match e {
                super::delivery::TypedError::Usage(m) => usage(m),
                super::delivery::TypedError::Conflict(m) => refuse(
                    ErrorKind::Conflict,
                    m,
                    "nothing was typed or stored; send again later or without typed",
                ),
                super::delivery::TypedError::Failed(m) => internal(m),
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

fn run(
    w: &StateWriter,
    base: &dyn Base,
    agent: &str,
    actor: &str,
    role: Role,
    v: Verb,
    args: &Value,
) -> Response {
    let r = match v {
        Verb::Dispatch => dispatch(w, agent, actor, args),
        Verb::Status => status(w),
        Verb::Peek => peek(w, base, args),
        Verb::Pending => pending(w, args),
        Verb::Answer => answer(w, base, actor, args),
        Verb::Renew => renew(w, args),
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
        Verb::Config => config(w, actor, args),
        Verb::Pr => pr(w, args),
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
    // A done report, a close and an escalation free a slot: the belt fills it in the same
    // transaction (belt.md 3.4, 4.1). Any other answer of these verbs fills nothing new.
    if matches!(v, Verb::Report | Verb::Close | Verb::Pending) {
        super::belt::fill(w, agent).map_err(internal)?;
    }
    Ok(r)
}

/// `renew` (spec section 12, Issue 46): the handover is stored in the command's own row, which
/// `makibad`'s secretary pass takes up. One renewal at a time.
fn renew(w: &StateWriter, args: &Value) -> Response {
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
    Ok(json!({"renewal": "accepted"}))
}

/// `status` (spec section 12): what the person's local `makiba status --json` prints, read from
/// the door's own database file. Read in `makibad`, its `merge_approval_mode` is the gate's.
fn status(w: &StateWriter) -> Response {
    crate::status_local::read(&db_file(w)?).map_err(internal)
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
    crate::status_local::events(&db_file(w)?, after, limit).map_err(internal)
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
            if action == "escalate" {
                missing_context(c, id)?;
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
    task: String,
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
    let row: Option<(String, String, Option<String>, String)> = w
        .conn()
        .query_row(
            "SELECT p.kind, p.status, r.task_id, p.question FROM pending p
             LEFT JOIN runs r ON r.id = p.run_id WHERE p.id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .map_err(internal)?;
    let Some((kind, status, task, question)) = row else {
        return Err(refuse(
            ErrorKind::NotFound,
            format!("no pending entry {id}"),
            "",
        ));
    };
    if !["open", "escalated"].contains(&status.as_str()) {
        return Err(refuse(
            ErrorKind::Conflict,
            format!("pending entry {id} is {status} already"),
            "",
        ));
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
    // The question goes with the answer: two answers of one task are told apart.
    let mut body = format!("Answer to pending entry {id} ({question}): {answer}");
    if let Some(note) = note {
        body.push_str(&format!("\nNote: {note}"));
    }
    Ok(Some(Question {
        id,
        answer: answer.to_string(),
        to: worker_agent_id(&task),
        task,
        body,
    }))
}

/// `answer` (spec sections 7 and 12). A question: its answer is queued to the worker and the
/// entry is answered, in the caller's transaction. A prompt (M6-8): exactly one key, and only
/// while the same episode's prompt is on a fresh read of the screen; any difference is `stale`
/// and nothing is pressed.
fn answer(w: &StateWriter, base: &dyn Base, actor: &str, args: &Value) -> Response {
    if let Some(q) = question(w, args)? {
        let id = store_message(w, actor, &q.to, "message", Some(&q.task), &q.body)?;
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
    // A code task's done report carries the state of its review, recorded by the door (Issue 135).
    let review = if state == "done" && role == Role::Worker {
        last_review_clear(w, actor)?;
        review_state(w, actor, task)?
    } else {
        None
    };
    let body = match &review {
        Some(r) => format!("{text}\n\n[makiba] {}", review_line(r)),
        None => text.to_string(),
    };
    let kind = format!("report_{state}");
    let id = store_message(
        w,
        actor,
        crate::spec::AGENT_SECRETARY,
        &kind,
        Some(task),
        &body,
    )?;
    if let Some(r) = &review {
        let payload = json!({"task": task, "state": state, "review": r});
        w.append_event(&NewEvent {
            kind: "worker_reported",
            actor: Some(actor),
            payload: Some(&payload),
            run_task_id: Some(task),
            ..Default::default()
        })
        .map_err(internal)?;
    }
    if state == "done" {
        w.conn()
            .execute(
                "UPDATE runs SET status = 'review' WHERE task_id = ?1 AND status IN ('queued','in_use','suspended')",
                [task],
            )
            .map_err(internal)?;
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
    undo: &mut Option<(PathBuf, Vec<u8>)>,
) -> Response {
    let pattern = str_arg(args, "pattern")?;
    let decision = str_arg(args, "decision")?;
    let reason = str_arg(args, "reason")?.trim();
    kebab("pattern", pattern)?;
    // The words of skills/curate.
    let status = match decision {
        "approved" | "rejected" => decision.to_string(),
        d => match d.strip_prefix("into-") {
            Some(skill) => {
                kebab("the skill of into-<skill>", skill)?;
                format!("merged-into-{skill}")
            }
            None => {
                return Err(usage(format!(
                    "decision is approved, rejected or into-<skill>, not {d:?}"
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
    *undo = Some((path, text.into_bytes()));
    Ok(payload)
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
/// replaced; every other byte as it was.
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
    let (from, to) = match ours[..] {
        [] => {
            return Err(refuse(
                ErrorKind::NotFound,
                format!("no entry of pattern {pattern:?} in {CANDIDATES_FILE}"),
                "",
            ));
        }
        [one] => one,
        _ => {
            return Err(refuse(
                ErrorKind::Conflict,
                format!(
                    "{} entries of pattern {pattern:?} in {CANDIDATES_FILE}; nothing was written",
                    ours.len()
                ),
                "the curator must merge them into one",
            ));
        }
    };
    let mut set = Vec::new();
    for (key, new) in [("status", status), ("decided", decided), ("reason", reason)] {
        let Some((i, at)) = (from..to).find_map(|i| {
            candidate_field(&lines[i])
                .filter(|(k, _)| *k == key)
                .map(|(_, at)| (i, at))
        }) else {
            return Err(refuse(
                ErrorKind::Conflict,
                format!("the entry of pattern {pattern:?} has no {key} line; nothing was written"),
                "",
            ));
        };
        if key == "status" && value(&lines[i], at) != "pending" {
            return Err(refuse(
                ErrorKind::Conflict,
                format!(
                    "the entry of pattern {pattern:?} is decided already: {}",
                    value(&lines[i], at)
                ),
                "",
            ));
        }
        set.push((i, at, new));
    }
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
}

fn same_pr(p: &Value, repo: &str, pr: i64) -> bool {
    p["pr"].as_i64() == Some(pr)
        && p["repo"]
            .as_str()
            .is_some_and(|r| r.eq_ignore_ascii_case(repo))
}

fn gate_rows(w: &StateWriter, repo: &str, pr: i64) -> rusqlite::Result<GateRows> {
    let c = w.conn();
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
    let ci = payloads("ci_completed")?;
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
    let task = c
        .query_row(
            "SELECT r.task_id FROM pr_watches p JOIN runs r ON r.id = p.run_id
             WHERE lower(p.repo) = lower(?1) AND p.pr = ?2 ORDER BY p.id DESC LIMIT 1",
            params![repo, pr],
            |r| r.get(0),
        )
        .optional()?;
    Ok(GateRows {
        latest_ci: ci
            .last()
            .map(|(_, v)| (text(v, "status"), text(v, "head_oid"))),
        failed: ci
            .iter()
            .filter(|(_, v)| v["status"] == json!("failed"))
            .map(|(_, v)| text(v, "head_oid"))
            .collect(),
        pin,
        projects,
        task,
    })
}

/// `(repo, pr, head)` of a `gate` request.
fn gate_args(args: &Value) -> Result<(String, i64, String), DoorError> {
    let repo = str_arg(args, "repo")?;
    let ok = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    };
    if !repo.split_once('/').is_some_and(|(o, n)| ok(o) && ok(n)) {
        return Err(usage(format!("repo is owner/name, not {repo:?}")));
    }
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

/// What the gate found outside the door's lock: refused before the git checks, or their verdict.
enum Found {
    Before(String),
    Ran(super::gate::Verdict, bool),
}

/// The gate's reads of GitHub and git: no lock is held while they run.
fn gate_outside(rows: &GateRows, repo: &str, pr: i64, head: &str) -> Found {
    use super::gate;
    let program = super::gh::program();
    let (_url, base_branch, view_head) = match gate::pr_view(&program, repo, pr) {
        Ok(v) => v,
        Err(e) => return Found::Before(e.0),
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
    let (base, fetched) = match gate::fetch(&clone, repo, pr, &base_branch) {
        Ok(ids) => ids,
        Err(e) => return Found::Before(e.0),
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
        v.result = "rejected".into();
        v.reason = e.0;
    }
    let docs = v.result == "no_manifest" && gate::docs_only(&clone, &base, head);
    Found::Ran(v, docs)
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
        if let Err(e) = w.conn().execute_batch("BEGIN IMMEDIATE") {
            return Err(internal(e));
        }
        let now = match gate_rows(&w, &repo, pr) {
            Ok(r) => r,
            Err(e) => {
                let _ = w.rollback();
                return fail(&w, Err(internal(e)));
            }
        };
        let mode = super::gate::merge_approval_mode();
        let (verdict, reason, allow, event) = match found {
            Found::Before(why) => ("rejected".to_string(), why, false, None),
            Found::Ran(mut v, docs) => {
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
                }
                let allow = v.result == "passed"
                    || (v.result == "no_manifest" && (mode == "human" || docs));
                let reason = if v.result == "no_manifest" && !allow {
                    format!(
                        "{}: a PR without a declared acceptance test needs the person under merge_approval_mode acceptance (only a docs-only change passes); the person merges it in the GitHub web UI",
                        v.reason
                    )
                } else {
                    v.reason.clone()
                };
                (v.result.clone(), reason, allow, Some(v))
            }
        };
        if let Some(v) = &event {
            let mut payload = json!({
                "repo": repo, "pr": pr, "base": v.base, "head": v.head, "result": v.result,
                "reason": v.reason, "mode": mode, "allow": allow,
            });
            if let Some(m) = &v.manifest_commit {
                payload["manifest_commit"] = json!(m);
                payload["paths"] = json!(v.paths);
            }
            if let Some(d) = &v.manifest_digest {
                payload["manifest_digest"] = json!(d);
            }
            if let Some(t) = &now.task {
                payload["task"] = json!(t);
            }
            let written = w.append_event(&NewEvent {
                kind: "acceptance_gate",
                actor: Some(actor),
                payload: Some(&payload),
                run_task_id: now.task.as_deref(),
                ..Default::default()
            });
            if let Err(e) = written {
                let _ = w.rollback();
                return fail(
                    &w,
                    Err(internal(format!(
                        "the verdict (this PR's pin) could not be recorded: {e}"
                    ))),
                );
            }
        }
        let r: Response = Ok(json!({"verdict": verdict, "allow": allow, "reason": reason,
                                    "mode": mode, "repo": repo, "pr": pr, "head": head}));
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
            if let Err(e) = w.conn().execute_batch("BEGIN IMMEDIATE") {
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
    /// `$MAKIBA_GATE_KEY`: where the merge gate's key is written at start.
    pub gate_key: PathBuf,
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
            gate_key: crate::cli::gate_key_path(),
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
    door.set_gate_key(&super::person_key::rotate_gate_key(&cfg.gate_key)?);
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
        Verb::Push => (
            "Have makibad push a worker's branch: the run's worktree, on its branch, clean, pushed as its head to the same branch at origin, never forced. Answers accepted; a notice tells you the head pushed (then record it with pr) or why it failed.",
            Map::from_iter([("task".into(), text.clone())]),
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
            "Dispatch a task: a queued run and its first launch. kind is code or document; project is required for code. The brief is the task text itself. issue: the Issue number the task works on, so that a range does not dispatch it again.",
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
            "Record the person's decision on a pending skill candidate: makibad writes status (approved, rejected, or merged-into-<skill> for into-<skill>), decided (today) and reason into the entry of pattern in <knowledge>/skill-candidates.md, changing nothing else. reason is one line. An unknown pattern is not_found; an entry already decided is conflict.",
            Map::from_iter([
                ("pattern".into(), text.clone()),
                ("decision".into(), text.clone()),
                ("reason".into(), text.clone()),
            ]),
            &["pattern", "decision", "reason"],
        ),
        Verb::Range => (
            "Ranges of work the person approves once; makibad then dispatches each range's next Issue itself when a slot frees. propose (project, issues and/or takes, at_once, budget_tasks, brief_head, replaces): a draft, read by makibad. pause, end (id), skip (id, issue, reason). approve and resume are the person's alone.",
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
                ("brief_head".into(), text.clone()),
                ("replaces".into(), json!({"type": "integer"})),
                ("issue".into(), json!({"type": "integer"})),
                ("reason".into(), text.clone()),
                ("read".into(), json!({"type": "integer"})),
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
    use super::{CONTEXT_PARTS, Verb, curator_at_work, json, save_note, tool};

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
