//! The notify gate (Issue 21), carried from claude-org-ja `tools/notify_gate.py`.
//!
//! A notice that reaches the secretary costs it a turn, so only what it can act on is queued. The
//! decision is code, here, and pure: `Decision { send, reason }`. A suppressed notice is still an
//! event (the caller writes `anomaly_raised` with the reason); only the message is skipped.
//!
//! Fail loud: when the input cannot be judged or the kind is unknown, the answer is `send`. A
//! suppression must be provably pointless. Nothing here looks at whether the secretary is busy or
//! away, and a delivered message is never taken as acted on.
//!
//! `decide` is what the daemon applies (anomaly rule names and notice kinds). The three carried
//! rules (`decide_event_cursor_reset`, `decide_curator_vanished`, `decide_retro_recorded`) have no
//! producer in makiba yet: they are kept as pure rules with their tests and wired to nothing.

use serde_json::Value;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct Decision {
    pub send: bool,
    pub reason: &'static str,
}

const fn d(send: bool, reason: &'static str) -> Decision {
    Decision { send, reason }
}

/// The gate for a notice about `kind` (an anomaly rule name, `unknown_terminal`, or
/// `worker_spawn_failed`). The two anomalies about the secretary are told to the person, not to
/// the secretary; every other known kind is actionable, and an unknown one is sent.
pub fn decide(kind: &str) -> Decision {
    match kind {
        "secretary_gone" | "secretary_still" => d(false, "person_only"),
        "blocked_question" | "blocked_permission" | "blocked_auth" | "error" | "gone"
        | "done_no_report" | "no_status" | "unknown_terminal" => d(true, "actionable"),
        "worker_spawn_failed" => d(true, "actionable"),
        _ => d(true, "unknown_kind"),
    }
}

/// `EVENT_CURSOR_RESET` only matters for background-tab workers. `idle_state` is the parsed
/// worker-idle-state (`None`: the file is absent, so no worker records at all; JSON `null` is not
/// absent, it is unreadable).
pub fn decide_event_cursor_reset(idle_state: Option<&Value>) -> Decision {
    let Some(state) = idle_state else {
        return d(false, "no_background_worker");
    };
    let Some(map) = state.as_object() else {
        return d(true, "unreadable_state");
    };
    let mut unknown = false;
    for rec in map.values() {
        let Some(rec) = rec.as_object() else {
            return d(true, "unreadable_state");
        };
        match rec.get("placement") {
            Some(Value::String(p)) if p == "background_tab" => {
                return d(true, "background_worker_present");
            }
            None | Some(Value::Null) => {}
            Some(Value::String(p)) if p == "same_tab" => {}
            Some(_) => unknown = true,
        }
    }
    if unknown {
        d(true, "unknown_placement")
    } else {
        d(false, "no_background_worker")
    }
}

/// A vanished curator only needs the secretary if no `CURATE_*` result arrived.
pub fn decide_curator_vanished(curate_result: Option<&str>) -> Decision {
    match curate_result {
        Some("done" | "skipped") => d(false, "result_already_received"),
        _ => d(true, "no_result_received"),
    }
}

/// `RETRO_RECORDED` is information only: the secretary has nothing to do.
pub fn decide_retro_recorded() -> Decision {
    d(false, "info_only")
}
