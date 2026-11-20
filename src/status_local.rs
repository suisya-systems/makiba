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
/// `{"makibad": {"pid", "heartbeat_at", "subscribed", "running"}}`.
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
    Ok(
        json!({"makibad": {"pid": pid, "heartbeat_at": beat, "subscribed": subscribed == 1,
                          "running": running}}),
    )
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
    let (pid, subscribed, beat, age) = match row {
        Ok(r) => r,
        Err(_) => {
            return Ok((
                "makibad: no heartbeat has ever been written\n".into(),
                false,
            ));
        }
    };
    let (Some(beat), Some(age)) = (beat, age) else {
        return Ok((
            "makibad: no heartbeat has ever been written\n".into(),
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
    Ok((text, fresh))
}
