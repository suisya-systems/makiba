//! `makiba status` run by the person (spec section 12): reads `state.db` read-only, by itself,
//! with no door, so that it works when `makibad` is dead. This is the only code of the person's
//! command that opens the database. `--json` prints `read`'s object, which the secretary's `status`
//! tool returns through the door.

use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
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
pub fn read(path: &std::path::Path) -> Result<Value, String> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
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
    Ok(
        json!({"makibad": {"pid": pid, "heartbeat_at": beat, "subscribed": subscribed == 1,
                          "running": running}, "merge_approval_mode": mode, "projects": projects}),
    )
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
