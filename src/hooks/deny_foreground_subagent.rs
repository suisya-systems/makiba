//! `makiba hook deny-foreground-subagent` (M4-3): a `PreToolUse` hook on the subagent tools that
//! refuses a subagent run in the foreground, which would hold the caller so that it reads no
//! message until the subagent ends (inventory HK-5, line L-K8). Ported from claude-org-ja's
//! `.hooks/block-foreground-subagent.sh` with the cases of its test.
//!
//! For `Agent`, `run_in_background` may be absent (the tool runs in the background by itself) or
//! `true`; any other value is refused. For the older `Task`, whose default was the foreground, it
//! must be `true`. Other tools pass; the names are matched exactly.

use super::Refusal;
use serde_json::Value;

/// The line this hook holds.
const LINE: &str = "L-K8";

pub fn check(payload: &Value) -> Result<(), Refusal> {
    let tool = super::field(payload, "/tool_name");
    if tool != "Agent" && tool != "Task" {
        return Ok(());
    }
    let Some(input) = payload.get("tool_input").and_then(Value::as_object) else {
        return Err(Refusal::new(
            super::FAIL_CLOSED,
            "the hook payload",
            format!("the {tool} payload has no tool_input object"),
        ));
    };
    match input.get("run_in_background") {
        Some(Value::Bool(true)) => Ok(()),
        None if tool == "Agent" => Ok(()),
        None => Err(Refusal::new(
            LINE,
            format!("{tool} with no run_in_background"),
            format!("{tool} without run_in_background runs in the foreground; set it to true"),
        )),
        Some(v) => Err(Refusal::new(
            LINE,
            format!("{tool} with run_in_background={v}"),
            format!(
                "{tool} with run_in_background other than true runs in the foreground; leave it out or set it to true"
            ),
        )),
    }
}
