//! `makiba status` run by the person (spec section 12): reads `state.db` read-only, by itself,
//! with no door, so that it works when `makibad` is dead. This is the only code of the person's
//! command that opens the database.

use rusqlite::{Connection, OpenFlags};
use std::process::ExitCode;
use std::time::Duration;

/// A heartbeat older than this means `makibad` is not running.
pub const STALE: Duration = Duration::from_secs(30);

pub fn main() -> ExitCode {
    let path = crate::daemon::state_dir().join("state.db");
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
        stage.as_deref().unwrap_or("not launched"),
        crate::base::attach_command(&session, crate::spec::AGENT_SECRETARY)
    ));
    Ok((text, fresh))
}
