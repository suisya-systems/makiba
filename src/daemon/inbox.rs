//! The box (spec sections 8 and 12): the function behind the door verb `inbox`. It returns the
//! caller's `pending` messages, each wrapped, oldest first, at most `INBOX_LIMIT` of them, and
//! marks exactly those `delivered`, in one transaction. The caller is the agent the door
//! authenticated; the door (M3-3) passes it in.
//!
//! A message stored as a typed one is not in the box: it was typed once and is never sent again.
//! Reading the box is not a push: `attempt` and `last_attempt_at` stay as they were.

use super::delivery::wrap;
use crate::db::writer::StateWriter;
use rusqlite::params;

/// How many messages one call returns (a constant that can change later; owner, 2026-11-18).
pub const INBOX_LIMIT: usize = 20;

pub struct InboxMessage {
    pub id: i64,
    /// The wrapped text of spec section 8, the same as a push or a typed send carries.
    pub text: String,
}

pub struct Inbox {
    pub messages: Vec<InboxMessage>,
    /// More unconfirmed messages are waiting: call again.
    pub more: bool,
}

pub fn inbox(w: &StateWriter, caller: &str) -> crate::db::Result<Inbox> {
    w.transaction(|w| {
        let mut q = w.conn().prepare(
            "SELECT m.id, m.event_id, m.sender, m.kind, m.body
             FROM messages m
             JOIN event_deliveries d ON d.source_event_id = m.event_id AND d.recipient = m.recipient
             JOIN events e ON e.id = m.event_id
             WHERE m.recipient = ?1 AND d.status = 'pending'
               AND COALESCE(json_extract(e.payload_json, '$.typed'), 0) = 0
             ORDER BY m.id LIMIT ?2",
        )?;
        let rows = q
            .query_map(params![caller, INBOX_LIMIT as i64 + 1], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(q);
        let more = rows.len() > INBOX_LIMIT;
        let mut messages = Vec::new();
        for (id, event_id, sender, kind, body) in rows.into_iter().take(INBOX_LIMIT) {
            w.confirm_push(event_id, caller)?;
            messages.push(InboxMessage {
                id,
                text: wrap(id, &sender, &kind, &body),
            });
        }
        Ok(Inbox { messages, more })
    })
}
