//! `makiba status` run by the person (spec section 12): reads `state.db` read-only, by itself,
//! with no door, so that it works when `makibad` is dead. This is the only code of the person's
//! command that opens the database. `--json` prints `read`'s object, which the secretary's `status`
//! tool returns through the door.

use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

/// A heartbeat older than this means `makibad` is not running.
pub const STALE: Duration = Duration::from_secs(30);

pub fn main(as_json: bool) -> ExitCode {
    let path = crate::daemon::state_dir().join("state.db");
    if as_json {
        return match read(&path) {
            Ok(v) => {
                println!("{v}");
                if v["makibad"]["running"] == json!(true) {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::FAILURE
                }
            }
            Err(e) => {
                eprintln!("makiba status: {e}");
                ExitCode::FAILURE
            }
        };
    }
    match report(&path) {
        Ok((text, fresh)) => {
            print!("{text}");
            if fresh {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(e) => {
            eprintln!("makiba status: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `read` of this organization's `state.db` (the person's `range approve` shows a range from it).
pub fn read_here() -> Result<Value, String> {
    read(&crate::daemon::state_dir().join("state.db"))
}

/// Who reads the status, which decides where `merge_approval_mode` comes from (Issue 383).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reader {
    /// The person's `makiba`: the mode the running `makibad` recorded (`recorded_mode`).
    Person,
    /// `makibad` itself, through the door: its own environment, read at each call as the gate
    /// reads it (Issue 188), which the secretary reads before every merge.
    Makibad,
}

/// The person's read (`Reader::Person`).
pub fn read(path: &Path) -> Result<Value, String> {
    read_as(path, Reader::Person)
}

/// The same read as an object, with no age in it, so two reads of one heartbeat agree:
/// `{"makibad": {"pid", "heartbeat_at", "subscribed", "running"}, "merge_approval_mode",
/// "merge_approval_mode_here"}`. `merge_approval_mode` is `makibad`'s mode as `reader` can know it
/// (null when the person's read finds no running `makibad` that recorded one);
/// `merge_approval_mode_here` is this process's environment: the person's shell, or through the
/// door `makibad`'s own.
pub fn read_as(path: &Path, reader: Reader) -> Result<Value, String> {
    let conn = open(path)?;
    let row = conn.query_row(
        "SELECT pid, subscribed, heartbeat_at,
                (julianday('now') - julianday(heartbeat_at)) * 86400.0
         FROM dispatcher WHERE id = 1",
        [],
        |r| {
            Ok((
                r.get::<_, Option<i64>>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<f64>>(3)?,
            ))
        },
    );
    let (pid, subscribed, beat, age) = row.unwrap_or((None, 0, None, None));
    let running = age.is_some_and(|a| a < STALE.as_secs_f64());
    let here = mode_here();
    let mode = match reader {
        Reader::Makibad => Some(here.to_string()),
        Reader::Person => recorded_mode(&conn, running),
    };
    let projects: Vec<Value> = projects(&conn)
        .into_iter()
        .map(|(slug, repo, branch)| {
            let hosts = crate::daemon::hosts_of(&conn, &slug).unwrap_or_default();
            let goal = ledger(&conn, repo.as_deref());
            json!({"slug": slug, "repo": repo, "default_branch": branch, "network_hosts": hosts.join(","),
                   "goal_ledger": goal})
        })
        .collect();
    let sql = |e: rusqlite::Error| format!("cannot read {}: {e}", path.display());
    let mut v = json!({"makibad": {"pid": pid, "heartbeat_at": beat, "subscribed": subscribed == 1,
                          "running": running}, "merge_approval_mode": mode,
        "merge_approval_mode_here": here, "projects": projects,
        "runs": runs(&conn).map_err(sql)?, "terminals": terminals(&conn).map_err(sql)?,
        "anomalies": anomalies(&conn).map_err(sql)?, "pending": pending(&conn).map_err(sql)?,
        "pr_watches": pr_watches(&conn).map_err(sql)?});
    v["ranges"] = Value::Array(crate::daemon::belt::read_ranges(&conn).map_err(sql)?);
    v["next_work"] = next_work(&conn).map_err(sql)?;
    Mask::new(&conn).value(&mut v);
    Ok(v)
}

/// The mode of this process's environment, as the gate reads it.
fn mode_here() -> &'static str {
    crate::daemon::gate::mode_of(std::env::var(crate::daemon::gate::MODE_ENV).ok().as_deref()).0
}

/// The mode the running `makibad` recorded in its `dispatcher_started` (Issue 383): the newest
/// such event of the pid in `dispatcher`, written at or after that row's start, so a record of an
/// earlier `makibad` (even one with the same pid) is not read. None when `makibad` is not
/// `running`, or has not recorded one (before its first Resync, or a build before Issue 383).
fn recorded_mode(conn: &Connection, running: bool) -> Option<String> {
    if !running {
        return None;
    }
    let value: Option<String> = conn
        .query_row(
            "SELECT json_extract(e.payload_json, '$.merge_approval_mode')
             FROM events e JOIN dispatcher d ON d.id = 1
             WHERE e.kind = 'dispatcher_started'
               AND json_extract(e.payload_json, '$.pid') = d.pid
               AND e.occurred_at >= d.started_at
             ORDER BY e.id DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .ok()
        .flatten();
    value.filter(|m| m == "human" || m == "acceptance")
}

/// The text's line of the merge approval mode: `makibad`'s, and this shell's when it differs or
/// `makibad`'s is unknown.
fn mode_line(makibad: Option<&str>, running: bool, here: &str) -> String {
    match makibad {
        Some(m) if m == here => format!("merge approval mode: {m} (makibad's)\n"),
        Some(m) => format!(
            "merge approval mode: {m} (makibad's); this shell says {here}, which the gate does not read\n"
        ),
        None if running => format!(
            "merge approval mode: unknown (makibad has not recorded it); this shell says {here}, which the gate does not read\n"
        ),
        None => format!(
            "merge approval mode: unknown (makibad is not running); this shell says {here}, which makiba up passes on\n"
        ),
    }
}

/// Open `state.db` read-only, in one read transaction, so every query of a read sees one state.
fn open(path: &Path) -> Result<Connection, String> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    conn.execute_batch("BEGIN")
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    Ok(conn)
}

/// Every row of `sql` as one JSON object of its columns, by name.
fn rows(conn: &Connection, sql: &str, p: impl rusqlite::Params) -> rusqlite::Result<Vec<Value>> {
    use rusqlite::types::ValueRef;
    let mut q = conn.prepare(sql)?;
    let names: Vec<String> = q.column_names().map(String::from).collect();
    q.query_map(p, |r| {
        let mut m = serde_json::Map::new();
        for (i, n) in names.iter().enumerate() {
            let v = match r.get_ref(i)? {
                ValueRef::Integer(n) => json!(n),
                ValueRef::Real(f) => json!(f),
                ValueRef::Text(t) => json!(String::from_utf8_lossy(t)),
                ValueRef::Null | ValueRef::Blob(_) => Value::Null,
            };
            m.insert(n.clone(), v);
        }
        Ok(Value::Object(m))
    })?
    .collect()
}

/// The runs not closed (Issue 168): a `failed` run stays until it is closed. A run waiting for
/// the person carries `waiting`, and one woken from parking `unparked` (Issue 283).
fn runs(conn: &Connection) -> rusqlite::Result<Vec<Value>> {
    let mut all = rows(
        conn,
        "SELECT r.task_id, l.kind, p.slug AS project, r.status, l.stage FROM runs r
         JOIN projects p ON p.id = r.project_id
         LEFT JOIN run_launch l ON l.id = (SELECT max(id) FROM run_launch WHERE run_id = r.id)
         WHERE r.status NOT IN ('completed', 'abandoned') ORDER BY r.id",
        [],
    )?;
    for run in &mut all {
        let task = run["task_id"].as_str().unwrap_or_default().to_string();
        let status = run["status"].as_str().unwrap_or_default().to_string();
        if let Value::Object(more) = crate::daemon::park::read_fields(conn, &task, &status)? {
            run.as_object_mut()
                .expect("a row is an object")
                .extend(more);
        }
        // Who is on it (Issue 406): the notice of a failed run's failure, and the secretary's
        // newest request about its task.
        let notice = match status.as_str() {
            "failed" => match failure_event(conn, &task)? {
                Some(source) => notice_after(conn, source)?,
                None => Value::Null,
            },
            _ => Value::Null,
        };
        let (acted_at, acted_verb) = secretary_act(conn, &task)?;
        let m = run.as_object_mut().expect("a row is an object");
        m.insert("notice".into(), notice);
        m.insert("secretary_acted_at".into(), acted_at);
        m.insert("secretary_acted_verb".into(), acted_verb);
    }
    Ok(all)
}

/// The event that failed the run of `task` (Issue 406): the newest of its `worker_spawn_failed`
/// and its `gone` raise told to the secretary, the only two records that fail a run.
fn failure_event(conn: &Connection, task: &str) -> rusqlite::Result<Option<i64>> {
    conn.query_row(
        "SELECT max(e.id) FROM events e
         WHERE e.run_id = (SELECT id FROM runs WHERE task_id = ?1)
           AND (e.kind = 'worker_spawn_failed'
                OR (e.kind = 'anomaly_raised'
                    AND json_extract(e.payload_json, '$.rule') = 'gone'
                    AND json_extract(e.payload_json, '$.suppressed') IS NULL))",
        [task],
        |r| r.get(0),
    )
}

/// The notice that told the secretary of the event `source` (Issue 406): the first `message_sent`
/// of `makibad` after it, about the same run (or none), whose message is a `notice` to the
/// secretary, with no other raise or launch failure of that run between them. Every writer of
/// such an event writes its notice in the same transaction. Null when there is none.
/// `{"to", "state": "queued"|"delivered", "written_at", "delivered_at"}`; never the body.
fn notice_after(conn: &Connection, source: i64) -> rusqlite::Result<Value> {
    let found = rows(
        conn,
        "SELECT m.recipient AS \"to\", e.occurred_at AS written_at,
                CASE WHEN d.status = 'delivered' THEN 'delivered' ELSE 'queued' END AS state,
                CASE WHEN d.status = 'delivered' THEN d.delivered_at END AS delivered_at
         FROM events s
         JOIN events e ON e.id > s.id AND e.kind = 'message_sent' AND e.actor = 'makibad'
                      AND e.run_id IS s.run_id
         JOIN messages m ON m.event_id = e.id AND m.kind = 'notice' AND m.recipient = 'secretary'
         LEFT JOIN event_deliveries d ON d.source_event_id = e.id AND d.recipient = m.recipient
         WHERE s.id = ?1
           AND NOT EXISTS (SELECT 1 FROM events x WHERE x.id > s.id AND x.id < e.id
                             AND x.kind IN ('anomaly_raised', 'worker_spawn_failed')
                             AND x.run_id IS s.run_id)
         ORDER BY e.id LIMIT 1",
        [source],
    )?;
    Ok(found.into_iter().next().unwrap_or(Value::Null))
}

/// The secretary's verbs that name a task by `task_id` (Issue 406).
const ACT_BY_TASK_ID: &[&str] = &["dispatch", "close", "peek"];
/// The secretary's verbs that name a task by `task` (`update` reads it through `push`); `pending`
/// does with `action` `add` only.
const ACT_BY_TASK: &[&str] = &["pr", "note", "push", "update", "review"];

/// The secretary's newest request about `task` (Issue 406): `received_at` and `verb` of the newest
/// `commands` row of `secretary` whose verb reads the task from its arguments (`ACT_BY_TASK_ID`,
/// `ACT_BY_TASK`, `pending add`, `send` to its worker, `answer` and `pending escalate|withdraw`
/// of one of its entries), whatever its status. An argument a verb does not read never counts.
fn secretary_act(conn: &Connection, task: &str) -> rusqlite::Result<(Value, Value)> {
    let list = |verbs: &[&str]| {
        verbs
            .iter()
            .map(|v| format!("'{v}'"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let sql = format!(
        "SELECT c.received_at, c.verb FROM commands c
         WHERE c.actor = ?1
           AND ((c.verb IN ({by_id}) AND json_extract(c.args_json, '$.task_id') = ?2)
             OR ((c.verb IN ({by_task})
                  OR (c.verb = 'pending' AND json_extract(c.args_json, '$.action') = 'add'))
                 AND json_extract(c.args_json, '$.task') = ?2)
             OR (c.verb = 'send' AND json_extract(c.args_json, '$.to') = ?3)
             OR ((c.verb = 'answer'
                  OR (c.verb = 'pending'
                      AND json_extract(c.args_json, '$.action') IN ('escalate', 'withdraw')))
                 AND json_extract(c.args_json, '$.id') IN
                     (SELECT p.id FROM pending p JOIN runs r ON r.id = p.run_id
                      WHERE r.task_id = ?2)))
         ORDER BY c.id DESC LIMIT 1",
        by_id = list(ACT_BY_TASK_ID),
        by_task = list(ACT_BY_TASK),
    );
    let worker = crate::spec::worker_agent_id(task);
    let found = rows(
        conn,
        &sql,
        rusqlite::params![crate::spec::AGENT_SECRETARY, task, worker],
    )?;
    Ok(match found.into_iter().next() {
        Some(r) => (r["received_at"].clone(), r["verb"].clone()),
        None => (Value::Null, Value::Null),
    })
}

/// The live terminals: the newest launch of each role and run whose key is not revoked (a key
/// dies with its terminal, and `down` revokes the keys of what it ends), unless its newest
/// record is a `terminal_gone`. `state` is its newest status (null before the first), `since`
/// when that state began (the launch, before the first status).
fn terminals(conn: &Connection) -> rusqlite::Result<Vec<Value>> {
    let launches = rows(
        conn,
        "SELECT l.role, r.task_id, l.spawn_id, l.created_at FROM run_launch l
         LEFT JOIN runs r ON r.id = l.run_id
         WHERE l.id IN (SELECT max(id) FROM run_launch GROUP BY role, run_id)
           AND EXISTS (SELECT 1 FROM agents a WHERE a.spawn_id = l.spawn_id
                         AND a.revoked_at IS NULL)
         ORDER BY l.id",
        [],
    )?;
    let mut out = Vec::new();
    for l in launches {
        let records = rows(
            conn,
            "SELECT kind, json_extract(payload_json, '$.state') AS state, occurred_at FROM events
             WHERE kind IN ('terminal_status', 'terminal_gone')
               AND json_extract(payload_json, '$.spawn_id') = ?1 ORDER BY id",
            [l["spawn_id"].as_str().unwrap_or_default()],
        )?;
        if records.last().is_some_and(|r| r["kind"] == "terminal_gone") {
            continue;
        }
        let (mut state, mut since) = (Value::Null, l["created_at"].clone());
        for (i, r) in records.iter().enumerate() {
            if i == 0 || r["state"] != state {
                (state, since) = (r["state"].clone(), r["occurred_at"].clone());
            }
        }
        let role = l["role"].as_str().unwrap_or_default();
        let name = match l["task_id"].as_str() {
            Some(task) if role == "worker" => crate::spec::worker_agent_id(task),
            _ => role.to_string(),
        };
        out.push(json!({"role": role, "name": name, "state": state, "since": since}));
    }
    Ok(out)
}

/// The open anomalies: each raise with no later clear of the same subject (its payload without
/// `suppressed`), in the order they were raised.
fn anomalies(conn: &Connection) -> rusqlite::Result<Vec<Value>> {
    // Each open subject, its entry, and its raise when that raise told the secretary (Issue 406).
    let mut open: Vec<(Value, Value, Option<i64>)> = Vec::new();
    for e in rows(
        conn,
        "SELECT id, kind, payload_json, occurred_at FROM events
         WHERE kind IN ('anomaly_raised', 'anomaly_cleared') ORDER BY id",
        [],
    )? {
        let mut subject: Value =
            serde_json::from_str(e["payload_json"].as_str().unwrap_or("{}")).unwrap_or_default();
        let suppressed = subject
            .as_object_mut()
            .and_then(|m| m.remove("suppressed"))
            .is_some();
        open.retain(|(s, _, _)| *s != subject);
        if e["kind"] == "anomaly_raised" {
            let entry = json!({"task_id": subject["task_id"], "rule": subject["rule"],
                               "since": e["occurred_at"]});
            let told = e["id"].as_i64().filter(|_| !suppressed);
            open.push((subject, entry, told));
        }
    }
    let mut out = Vec::new();
    for (_, mut entry, told) in open {
        entry["notice"] = match told {
            Some(raise) => notice_after(conn, raise)?,
            None => Value::Null,
        };
        out.push(entry);
    }
    Ok(out)
}

/// The open and escalated entries. An entry made from a screen (`prompt_text`) shows the first
/// line of its question only: the screen may hold a message between two agents.
fn pending(conn: &Connection) -> rusqlite::Result<Vec<Value>> {
    let mut out = rows(
        conn,
        "SELECT p.id, r.task_id, p.kind, p.question, p.status, p.created_at,
                p.prompt_text IS NOT NULL AS screen, p.context_json
         FROM pending p LEFT JOIN runs r ON r.id = p.run_id
         WHERE p.status IN ('open', 'escalated') ORDER BY p.id",
        [],
    )?;
    for p in &mut out {
        let m = p.as_object_mut().expect("a row is an object");
        let screen = m.remove("screen") == Some(json!(1));
        if screen {
            let first = m["question"].as_str().unwrap_or_default();
            m["question"] = json!(first.lines().next().unwrap_or_default());
        }
        let stored = m.remove("context_json").unwrap_or_default();
        m.insert("context".into(), context(&stored, screen));
    }
    Ok(out)
}

/// The context of a pending entry (Issue 255): each of its five parts, null when not set, and
/// all null for an entry made from a screen, which shows its first line only.
fn context(stored: &Value, screen: bool) -> Value {
    let stored: Value = serde_json::from_str(stored.as_str().unwrap_or("{}")).unwrap_or_default();
    let parts = crate::daemon::door::CONTEXT_PARTS.map(|part| {
        let v = match &stored[part] {
            Value::String(t) if !screen => json!(t),
            _ => Value::Null,
        };
        (part.to_string(), v)
    });
    Value::Object(parts.into_iter().collect())
}

/// The text lines of the open and escalated entries (Issue 255): each with its context in short
/// form, the first line of each part, masked as `--json` is.
fn pending_text(conn: &Connection) -> String {
    let rows = pending(conn).unwrap_or_default();
    if rows.is_empty() {
        return "pending: none\n".into();
    }
    let first = |v: &Value| {
        v.as_str()
            .unwrap_or("-")
            .lines()
            .next()
            .unwrap_or("")
            .to_string()
    };
    let mut text = String::from("pending:\n");
    for p in &rows {
        text.push_str(&format!(
            "  #{} {} (task {}): {}\n",
            p["id"],
            first(&p["status"]),
            first(&p["task_id"]),
            first(&p["question"])
        ));
        let c = &p["context"];
        for (part, label) in [
            ("background", "background"),
            ("if_yes", "if yes"),
            ("if_no", "if no"),
        ] {
            if !c[part].is_null() {
                text.push_str(&format!("    {label}: {}\n", first(&c[part])));
            }
        }
        if !c["recommend"].is_null() || !c["why"].is_null() {
            text.push_str(&format!(
                "    recommend: {} - {}\n",
                first(&c["recommend"]),
                first(&c["why"])
            ));
        }
    }
    Mask::new(conn).text(&text)
}

/// The CI watches under way.
fn pr_watches(conn: &Connection) -> rusqlite::Result<Vec<Value>> {
    rows(
        conn,
        "SELECT w.repo, w.pr, w.head_oid, r.task_id, w.started_at FROM pr_watches w
         LEFT JOIN runs r ON r.id = w.run_id WHERE w.status = 'watching' ORDER BY w.id",
        [],
    )
}

/// The newest triage scan (Issue 256), null before the first: what the secretary would propose
/// next outside any range. A scan recorded before Issue 256 has null `candidates` and `left_out`.
fn next_work(conn: &Connection) -> rusqlite::Result<Value> {
    let Some(e) = rows(
        conn,
        "SELECT id, occurred_at, payload_json FROM events
         WHERE kind = 'work_discovery_scanned' ORDER BY id DESC LIMIT 1",
        [],
    )?
    .pop() else {
        return Ok(Value::Null);
    };
    let p: Value =
        serde_json::from_str(e["payload_json"].as_str().unwrap_or("{}")).unwrap_or_default();
    // Every candidate found ready: those the notice listed and those its cut left.
    let all =
        p["candidate_count"].as_i64().unwrap_or(0) + p["truncated_count"].as_i64().unwrap_or(0);
    let mut v = json!({"scan_id": e["id"], "scanned_at": e["occurred_at"], "candidate_count": all});
    for k in [
        "trigger",
        "status",
        "error",
        "input_truncated",
        "candidates",
        "candidates_not_recorded",
        "left_out",
        "left_out_not_recorded",
    ] {
        v[k] = p[k].clone();
    }
    Ok(v)
}

/// The `next work:` line of the text, from the masked `next_work`, and a second line with the
/// reasons when there is no candidate.
fn next_text(conn: &Connection) -> String {
    let mut v = next_work(conn).unwrap_or_default();
    Mask::new(conn).value(&mut v);
    let s = |v: &Value| v.as_str().unwrap_or("").to_string();
    if v.is_null() {
        return "next work: no scan yet (makibad scans after a run is closed)\n".into();
    }
    let at = s(&v["scanned_at"]);
    if v["status"] == "error" {
        return format!("next work: the scan of {at} failed: {}\n", s(&v["error"]));
    }
    let n = v["candidate_count"].as_u64().unwrap_or(0);
    let mut line = match n {
        0 => format!("next work: no candidates (scan of {at})"),
        1 => format!("next work: 1 candidate (scan of {at})"),
        n => format!("next work: {n} candidates (scan of {at})"),
    };
    let forwarded = v["candidates"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["forwarded"] == json!(true))
        .count();
    if n > 0 && v["candidates"].is_array() {
        line.push_str(&format!(", {forwarded} forwarded to the secretary"));
    }
    if !s(&v["input_truncated"]).is_empty() {
        line.push_str(&format!(", input truncated: {}", s(&v["input_truncated"])));
    }
    let Some(candidates) = v["candidates"].as_array() else {
        return format!("{line}; details not recorded by this makibad\n");
    };
    if let Some(c) = candidates.iter().find(|c| c["recommended"] == json!(true)) {
        let clause = match c["clause_id"].as_str() {
            Some(id) => format!("{id} {}", s(&c["clause_heading"])),
            None => "names no clause".into(),
        };
        line.push_str(&format!(
            "; recommended {}#{} {} ({clause})",
            s(&c["repo"]),
            c["issue"],
            s(&c["title"])
        ));
    }
    line.push('\n');
    if n == 0 {
        let why: Vec<String> = v["left_out"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|l| {
                let at = match (l["project"].as_str(), l["issue"].as_i64()) {
                    (Some(p), Some(i)) => format!("project {p} #{i}"),
                    (Some(p), None) => format!("project {p}"),
                    (None, Some(i)) => format!("{}#{i}", s(&l["repo"])),
                    (None, None) => s(&l["repo"]),
                };
                format!("{at}: {}", s(&l["reason"]))
            })
            .collect();
        line.push_str(&match why.len() {
            0 => "  why: no open Issue is ready\n".to_string(),
            1..=3 => format!("  why: {}\n", why.join("; ")),
            k => format!("  why: {}; +{} more\n", why[..3].join("; "), k - 3),
        });
    }
    line
}

/// How many events `events` returns when not told, and at most.
const EVENTS_LIMIT: u32 = 100;
const EVENTS_MAX: u32 = 1000;

/// The events after `after`, in order, at most `limit` (Issue 168): `{"events": [{"id",
/// "occurred_at", "actor", "kind", "task_id", "payload"}]}`. A launch's screen is not shown.
pub fn events(path: &Path, after: i64, limit: Option<u32>) -> Result<Value, String> {
    let conn = open(path)?;
    let limit = limit.unwrap_or(EVENTS_LIMIT).min(EVENTS_MAX);
    let mut events = rows(
        &conn,
        "SELECT e.id, e.occurred_at, e.actor, e.kind, r.task_id, e.payload_json AS payload
         FROM events e LEFT JOIN runs r ON r.id = e.run_id
         WHERE e.id > ?1 ORDER BY e.id LIMIT ?2",
        rusqlite::params![after, limit],
    )
    .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    for e in &mut events {
        let mut p: Value =
            serde_json::from_str(e["payload"].as_str().unwrap_or("{}")).unwrap_or_default();
        if let Some(screen) = p.get_mut("screen") {
            *screen = json!("<screen>");
        }
        e["payload"] = p;
    }
    let mut v = json!({"events": events});
    Mask::new(&conn).value(&mut v);
    Ok(v)
}

/// `makiba events`: the read above, as one object (`--json`) or one line per event.
pub fn events_main(after: i64, limit: Option<u32>, as_json: bool) -> ExitCode {
    let path = crate::daemon::state_dir().join("state.db");
    match events(&path, after, limit) {
        Ok(v) if as_json => println!("{v}"),
        Ok(v) => {
            for e in v["events"].as_array().into_iter().flatten() {
                let text = |k: &str| e[k].as_str().unwrap_or("-").to_string();
                println!(
                    "{} {} {} {} {} {}",
                    e["id"],
                    text("occurred_at"),
                    text("kind"),
                    text("actor"),
                    text("task_id"),
                    e["payload"]
                );
            }
        }
        Err(e) => {
            eprintln!("makiba events: {e}");
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}

/// What no output of the read surface shows: the person key and the merge gate's key, as their
/// files hold them, and every run of 64 hex characters that is a key whose SHA-256 `agents`
/// stores. Each becomes `<key>`.
struct Mask {
    literals: Vec<String>,
    hashes: std::collections::HashSet<String>,
}

impl Mask {
    fn new(conn: &Connection) -> Mask {
        let literals = [crate::cli::person_key_path(), crate::cli::gate_key_path()]
            .iter()
            .filter_map(|f| std::fs::read_to_string(f).ok())
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty())
            .collect();
        let hashes = conn
            .prepare("SELECT key_sha256 FROM agents")
            .and_then(|mut q| q.query_map([], |r| r.get(0))?.collect())
            .unwrap_or_default();
        Mask { literals, hashes }
    }

    fn text(&self, s: &str) -> String {
        let mut s = s.to_string();
        for k in &self.literals {
            s = s.replace(k.as_str(), "<key>");
        }
        let b = s.as_bytes();
        let mut out = String::new();
        let (mut i, mut done) = (0, 0);
        while i < b.len() {
            let n = b[i..].iter().take_while(|c| c.is_ascii_hexdigit()).count();
            // Every 64-character window of the run: a key may touch other hex characters.
            let (end, mut j) = (i + n, i);
            while j + 64 <= end {
                if self
                    .hashes
                    .contains(&crate::daemon::door::sha256_hex(&s[j..j + 64]))
                {
                    out.push_str(&s[done..j]);
                    out.push_str("<key>");
                    j += 64;
                    done = j;
                } else {
                    j += 1;
                }
            }
            i = end.max(i + 1);
        }
        out.push_str(&s[done..]);
        out
    }

    fn value(&self, v: &mut Value) {
        match v {
            Value::String(s) => *s = self.text(s),
            Value::Array(a) => a.iter_mut().for_each(|x| self.value(x)),
            Value::Object(m) => m.values_mut().for_each(|x| self.value(x)),
            _ => {}
        }
    }
}

/// The active projects by slug (Issue 227): slug, `OWNER/NAME` (`None` when no `origin_url` is
/// recorded; another URL as it is) and the branch a dispatch starts from. A database without the
/// table reads as no project.
fn projects(conn: &Connection) -> Vec<(String, Option<String>, String)> {
    let Ok(mut q) = conn.prepare(
        "SELECT slug, origin_url, COALESCE(default_branch, 'main') FROM projects
         WHERE status = 'active' ORDER BY slug",
    ) else {
        return Vec::new();
    };
    q.query_map([], |r| {
        let url: Option<String> = r.get(1)?;
        let repo = url.map(|u| {
            u.strip_prefix("https://github.com/")
                .map_or(u.clone(), str::to_string)
        });
        Ok((r.get(0)?, repo, r.get(2)?))
    })
    .map(|rows| rows.filter_map(|r| r.ok()).collect())
    .unwrap_or_default()
}

/// A project's goal ledger as the scan would read it (`docs/goals.md`), and the newest placement
/// through the verb `goal` (Issue 271): `state` is `none`, `ok` or `error`; `clauses` the number of
/// clauses when `ok`; `placed_at` and `placed_by` null when no placement was recorded.
fn ledger(conn: &Connection, repo: Option<&str>) -> Value {
    use crate::work_discovery::goals::{GoalStatus, load_goals};
    let Some(repo) = repo else {
        return json!({"state": "none", "clauses": null, "placed_at": null, "placed_by": null});
    };
    let g = load_goals(&crate::cli::goals_dir(), repo);
    let (state, clauses) = match g.status {
        GoalStatus::Ok => ("ok", json!(g.clauses.len())),
        GoalStatus::Unset => ("none", Value::Null),
        GoalStatus::Error => ("error", Value::Null),
    };
    let placed: Option<(String, String)> = conn
        .query_row(
            "SELECT occurred_at, actor FROM events WHERE kind = 'goal_ledger_placed'
             AND json_extract(payload_json, '$.repo') = ?1 ORDER BY id DESC LIMIT 1",
            [repo.to_lowercase()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    json!({"state": state, "clauses": clauses,
           "placed_at": placed.as_ref().map(|p| &p.0), "placed_by": placed.as_ref().map(|p| &p.1)})
}

/// The ledger's words for a project's text line.
fn ledger_text(l: &Value) -> String {
    let when = match (l["placed_at"].as_str(), l["placed_by"].as_str()) {
        (Some(at), Some(by)) => format!(", placed {at} by {by}"),
        _ => String::new(),
    };
    match l["state"].as_str() {
        Some("ok") => format!("goal ledger: ok, {} clauses{when}", l["clauses"]),
        Some("error") => format!("goal ledger: cannot be read{when}"),
        _ => "goal ledger: none".into(),
    }
}

/// The text lines of the projects: one line when there is none.
fn projects_text(conn: &Connection) -> String {
    let rows = projects(conn);
    if rows.is_empty() {
        return "projects: none registered\n".into();
    }
    let mut text = String::from("projects:\n");
    for (slug, repo, branch) in rows {
        let hosts = crate::daemon::hosts_of(conn, &slug).unwrap_or_default();
        let hosts = if hosts.is_empty() {
            "none".to_string()
        } else {
            hosts.join(", ")
        };
        let goal = ledger_text(&ledger(conn, repo.as_deref()));
        match repo {
            Some(repo) => text.push_str(&format!(
                "  {slug}: {repo} (default branch {branch}; network hosts: {hosts}; {goal})\n"
            )),
            None => text.push_str(&format!(
                "  {slug}: repository not recorded (network hosts: {hosts}; {goal})\n"
            )),
        }
    }
    text
}

/// The text to print and whether the heartbeat is fresh.
fn report(path: &std::path::Path) -> Result<(String, bool), String> {
    let conn =
        Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| {
            format!(
                "cannot open {}: {e}; makibad has never run here",
                path.display()
            )
        })?;
    // The age is worked out by SQLite, from the same clock that wrote the heartbeat.
    let row = conn.query_row(
        "SELECT pid, subscribed, heartbeat_at,
                (julianday('now') - julianday(heartbeat_at)) * 86400.0
         FROM dispatcher WHERE id = 1",
        [],
        |r| {
            Ok((
                r.get::<_, Option<i64>>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<f64>>(3)?,
            ))
        },
    );
    let (pid, subscribed, beat, age) = row.unwrap_or((None, 0, None, None));
    let (Some(beat), Some(age)) = (beat, age) else {
        return Ok((
            format!(
                "makibad: no heartbeat has ever been written\n{}{}{}{}",
                mode_line(None, false, mode_here()),
                projects_text(&conn),
                pending_text(&conn),
                next_text(&conn)
            ),
            false,
        ));
    };
    let fresh = age < STALE.as_secs_f64();
    let pid = pid.map_or("unknown".to_string(), |p| p.to_string());
    let mut text = format!(
        "makibad: pid {pid}\nheartbeat: {beat} ({age:.0} s ago)\nsubscribed: {}\n",
        if subscribed == 1 { "yes" } else { "no" }
    );
    text.push_str(&mode_line(
        recorded_mode(&conn, fresh).as_deref(),
        fresh,
        mode_here(),
    ));
    if !fresh {
        text.push_str("makibad is not running: the heartbeat is older than 30 s\n");
    }
    // The secretary (Issue 43): its newest launch, and how the person attaches to it.
    let stage: Option<String> = conn
        .query_row(
            "SELECT stage FROM run_launch WHERE role = 'secretary' ORDER BY id DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .ok();
    let session = std::env::var("MAKIBA_BASE_SESSION").unwrap_or_else(|_| "makiba".into());
    text.push_str(&format!(
        "secretary: {}; attach with {}\n",
        secretary_state(&conn, stage.as_deref()),
        crate::base::attach_command(&session, crate::spec::AGENT_SECRETARY)
    ));
    text.push_str(&projects_text(&conn));
    text.push_str(&pending_text(&conn));
    text.push_str(&next_text(&conn));
    Ok((text, fresh))
}

/// What the `secretary:` line says (Issue 46): a renewal under way, a secretary that is gone (and
/// whether `makibad` still relaunches it), the last renewal's failure, or the newest launch's stage.
fn secretary_state(conn: &Connection, stage: Option<&str>) -> String {
    let one = |sql: &str| -> Option<String> { conn.query_row(sql, [], |r| r.get(0)).ok() };
    if one("SELECT 'y' FROM commands WHERE verb = 'renew' AND status = 'accepted' LIMIT 1")
        .is_some()
    {
        return format!("renewing (newest launch {})", stage.unwrap_or("none"));
    }
    // The same reads as makibad's secretary pass (`daemon::secretary::stopped`, `down`).
    if one("SELECT 'y' FROM commands c
         WHERE c.actor = 'makibad' AND c.verb = 'renew' AND c.status = 'rejected'
           AND NOT EXISTS (SELECT 1 FROM run_launch l WHERE l.role = 'secretary'
                             AND julianday(l.created_at) > julianday(c.received_at))")
    .is_some()
    {
        return "gone: relaunched twice in 10 minutes and stopped; run makiba up".into();
    }
    let gone = one(
        "SELECT kind FROM events WHERE kind IN ('anomaly_raised', 'anomaly_cleared')
           AND json_extract(payload_json, '$.rule') = 'secretary_gone'
           AND json_extract(payload_json, '$.generation') =
               (SELECT max(id) FROM agents WHERE agent_id = 'secretary')
         ORDER BY id DESC LIMIT 1",
    );
    // The newest renewal's failure, while no secretary was launched after it.
    let failed = one(
        "SELECT json_extract(c.result_json, '$.error.message') FROM commands c
         WHERE c.id = (SELECT max(id) FROM commands WHERE verb = 'renew')
           AND c.status = 'failed'
           AND NOT EXISTS (SELECT 1 FROM run_launch l WHERE l.role = 'secretary'
                             AND julianday(l.created_at) > julianday(c.finished_at))",
    );
    let failed = failed.map_or_else(String::new, |why| format!(" (last renewal failed: {why})"));
    // As makibad's `down`: a launch `makiba up` failed after a stop of the base is not relaunched.
    let failed_launch = one("SELECT 'y' FROM run_launch
         WHERE id = (SELECT max(id) FROM run_launch WHERE role = 'secretary')
           AND stage = 'failed' AND failed_reason IS NOT 'the base was stopped'");
    if gone.as_deref() == Some("anomaly_raised") || failed_launch.is_some() {
        return format!("gone{failed}; makibad relaunches it");
    }
    format!("{}{failed}", stage.unwrap_or("not launched"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A key is masked alone, inside a longer run of hex and twice; other hex stays.
    #[test]
    fn a_key_is_masked_wherever_it_sits_in_hex() {
        let key = "ab".repeat(32);
        let m = Mask {
            literals: vec!["lit-key-123".into()],
            hashes: [crate::daemon::door::sha256_hex(&key)].into(),
        };
        let other = "cd".repeat(32);
        assert_eq!(m.text(&format!("a{key}f {key}")), "a<key>f <key>");
        assert_eq!(m.text(&format!("{key}{key}")), "<key><key>");
        assert_eq!(m.text(&other), other);
        assert_eq!(m.text("x lit-key-123 y"), "x <key> y");
    }
}
