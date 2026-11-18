//! M3: the launch identity of a terminal (`agents.spawn_id`, the tag `makiba.spawn`) tells
//! terminals of one role apart in the anomaly pass and in delivery confirmation (spec sections
//! 5.1, 6.3, 8 and 10).
//!
//! No daemon and no base: the records are written into a fresh database, the anomaly pass is
//! called at an explicit time, and delivery gets a channel that accepts every push.

use makiba::daemon::anomalies::Anomalies;
use makiba::daemon::delivery::{Channel, Delivery};
use makiba::db::writer::{NewEvent, StateWriter};
use serde_json::{Value, json};

struct Accepting;

impl Channel for Accepting {
    fn reaches(&self, _recipient: &str) -> bool {
        true
    }
    fn push(&self, _recipient: &str, _text: &str) -> Result<(), String> {
        Ok(())
    }
}

fn ts(secs: i64) -> String {
    let s = 10 * 3600 + secs;
    format!(
        "2026-11-18T{:02}:{:02}:{:02}.000Z",
        s / 3600,
        s / 60 % 60,
        s % 60
    )
}

/// A key row of the secretary: one row is one launch, and `spawn` is the launch's identity.
fn secretary_launch(w: &StateWriter, at: i64, spawn: &str) {
    w.conn()
        .execute(
            "INSERT INTO agents (agent_id, role, key_sha256, created_at)
             VALUES ('secretary', 'secretary', ?1, ?2)",
            rusqlite::params![format!("key-{spawn}"), ts(at)],
        )
        .unwrap();
    // The column is the launch identity of the row; a database without it ends the test at the
    // assertions below, where the old and the new terminal cannot be told apart.
    let _ = w.conn().execute(
        "UPDATE agents SET spawn_id = ?1 WHERE key_sha256 = ?2",
        rusqlite::params![spawn, format!("key-{spawn}")],
    );
}

fn secretary_record(w: &StateWriter, kind: &str, at: Option<i64>, spawn: &str, state: &str) {
    let mut p = json!({"task_id": null, "role": "secretary", "spawn_id": spawn});
    if kind == "terminal_status" {
        p["state"] = state.into();
    } else {
        p["reason"] = "removed".into();
    }
    let at = at.map(ts);
    w.append_event(&NewEvent {
        kind,
        actor: Some("makibad"),
        payload: Some(&p),
        occurred_at: at.as_deref(),
        ..Default::default()
    })
    .unwrap();
}

#[test]
fn a_gone_of_the_old_secretary_terminal_after_the_new_launch_is_not_the_new_ones() {
    let dir = tempfile::tempdir().unwrap();
    let w = StateWriter::open(&dir.path().join("state.db")).unwrap();
    secretary_launch(&w, 0, "old");
    secretary_record(&w, "terminal_status", Some(5), "old", "idle");
    secretary_launch(&w, 100, "new");
    // The old terminal's removal is recorded after the new one was launched.
    secretary_record(&w, "terminal_gone", Some(110), "old", "");
    let mut a = Anomalies::new();
    a.pass_at(&w, &ts(120)).unwrap();
    let raised: Vec<Value> = {
        let mut q = w
            .conn()
            .prepare("SELECT payload_json FROM events WHERE kind = 'anomaly_raised'")
            .unwrap();
        q.query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|p| serde_json::from_str(&p.unwrap()).unwrap())
            .collect()
    };
    assert!(
        raised.is_empty(),
        "secretary_gone was raised for the new launch by the old terminal's gone: {raised:?}"
    );
}

#[test]
fn a_push_to_an_idle_recipient_is_not_confirmed_by_a_turn_start_of_another_launch() {
    let dir = tempfile::tempdir().unwrap();
    let w = StateWriter::open(&dir.path().join("state.db")).unwrap();
    secretary_launch(&w, 0, "new");
    secretary_record(&w, "terminal_status", None, "new", "idle");
    let sent = w
        .append_event(&NewEvent {
            kind: "message_sent",
            ..Default::default()
        })
        .unwrap();
    w.conn()
        .execute(
            "INSERT INTO messages (event_id, sender, recipient, kind, body)
             VALUES (?1, 'w-a', 'secretary', 'message', 'x')",
            [sent],
        )
        .unwrap();
    w.conn()
        .execute(
            "INSERT INTO event_deliveries (source_event_id, recipient) VALUES (?1, 'secretary')",
            [sent],
        )
        .unwrap();
    let mut d = Delivery::new(Box::new(Accepting));
    d.pass(&w).unwrap();
    // A terminal of another launch starts a turn inside the window.
    secretary_record(&w, "terminal_status", None, "old", "working");
    d.pass(&w).unwrap();
    let status: String = w
        .conn()
        .query_row(
            "SELECT status FROM event_deliveries WHERE source_event_id = ?1",
            [sent],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        status, "pending",
        "the push was confirmed by another launch"
    );
}
