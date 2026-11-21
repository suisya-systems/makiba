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

/// The same read as an object, with no age in it, so two reads of one heartbeat agree:
/// `{"makibad": {"pid", "heartbeat_at", "subscribed", "running"}, "merge_approval_mode"}`. The mode
/// is read from this process's environment at each call, as the gate reads it (Issue 188): through
/// the door that is `makibad`'s, which the secretary reads before every merge; run by the person,
/// it is the person's shell's.
pub fn read(path: &Path) -> Result<Value, String> {
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
    let mode =
        crate::daemon::gate::mode_of(std::env::var(crate::daemon::gate::MODE_ENV).ok().as_deref())
            .0;
    let projects: Vec<Value> = projects(&conn)
        .into_iter()
        .map(|(slug, repo, branch)| json!({"slug": slug, "repo": repo, "default_branch": branch}))
        .collect();
    let sql = |e: rusqlite::Error| format!("cannot read {}: {e}", path.display());
    let mut v = json!({"makibad": {"pid": pid, "heartbeat_at": beat, "subscribed": subscribed == 1,
                          "running": running}, "merge_approval_mode": mode, "projects": projects,
        "runs": runs(&conn).map_err(sql)?, "terminals": terminals(&conn).map_err(sql)?,
        "anomalies": anomalies(&conn).map_err(sql)?, "pending": pending(&conn).map_err(sql)?,
        "pr_watches": pr_watches(&conn).map_err(sql)?});
    Mask::new(&conn).value(&mut v);
    Ok(v)
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

/// The runs not closed (Issue 168): a `failed` run stays until it is closed.
fn runs(conn: &Connection) -> rusqlite::Result<Vec<Value>> {
    rows(
        conn,
        "SELECT r.task_id, l.kind, p.slug AS project, r.status, l.stage FROM runs r
         JOIN projects p ON p.id = r.project_id
         LEFT JOIN run_launch l ON l.id = (SELECT max(id) FROM run_launch WHERE run_id = r.id)
         WHERE r.status NOT IN ('completed', 'abandoned') ORDER BY r.id",
        [],
    )
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
    let mut open: Vec<(Value, Value)> = Vec::new();
    for e in rows(
        conn,
        "SELECT kind, payload_json, occurred_at FROM events
         WHERE kind IN ('anomaly_raised', 'anomaly_cleared') ORDER BY id",
        [],
    )? {
        let mut subject: Value =
            serde_json::from_str(e["payload_json"].as_str().unwrap_or("{}")).unwrap_or_default();
        if let Some(m) = subject.as_object_mut() {
            m.remove("suppressed");
        }
        open.retain(|(s, _)| *s != subject);
        if e["kind"] == "anomaly_raised" {
            let entry = json!({"task_id": subject["task_id"], "rule": subject["rule"],
                               "since": e["occurred_at"]});
            open.push((subject, entry));
        }
    }
    Ok(open.into_iter().map(|(_, e)| e).collect())
}

/// The open and escalated entries. An entry made from a screen (`prompt_text`) shows the first
/// line of its question only: the screen may hold a message between two agents.
fn pending(conn: &Connection) -> rusqlite::Result<Vec<Value>> {
    let mut out = rows(
        conn,
        "SELECT p.id, r.task_id, p.kind, p.question, p.status, p.created_at,
                p.prompt_text IS NOT NULL AS screen
         FROM pending p LEFT JOIN runs r ON r.id = p.run_id
         WHERE p.status IN ('open', 'escalated') ORDER BY p.id",
        [],
    )?;
    for p in &mut out {
        let m = p.as_object_mut().expect("a row is an object");
        if m.remove("screen") == Some(json!(1)) {
            let first = m["question"].as_str().unwrap_or_default();
            m["question"] = json!(first.lines().next().unwrap_or_default());
        }
    }
    Ok(out)
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

/// The text lines of the projects: one line when there is none.
fn projects_text(conn: &Connection) -> String {
    let rows = projects(conn);
    if rows.is_empty() {
        return "projects: none registered\n".into();
    }
    let mut text = String::from("projects:\n");
    for (slug, repo, branch) in rows {
        match repo {
            Some(repo) => text.push_str(&format!("  {slug}: {repo} (default branch {branch})\n")),
            None => text.push_str(&format!("  {slug}: repository not recorded\n")),
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
                "makibad: no heartbeat has ever been written\n{}",
                projects_text(&conn)
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
