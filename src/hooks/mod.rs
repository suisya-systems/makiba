//! `makiba hook <name>`: the Claude Code hooks a role's settings name (spec section 14). Each reads
//! the hook payload on standard input; exit 0 lets the tool call run, exit 2 refuses it with the
//! reason on standard error.
//!
//! Every hook reads its payload with `read_payload`, which fails closed as claude-org-ja's
//! `.hooks/lib` does (inventory HK-1): input that cannot be read, is empty or blank, is not one
//! JSON object, or carries a `tool_input` that is neither absent, null nor an object, is refused.

use serde_json::Value;
use std::io::Read;
use std::process::ExitCode;

pub mod deny_base;
pub mod deny_dangerous_git;
pub mod deny_foreground_subagent;
pub mod deny_no_verify;
pub mod deny_push;
pub mod deny_workers_delete;
pub mod re;
pub mod shell;
pub mod worker_boundary;

/// A hook: the payload in, `Err(reason)` to refuse.
type Hook = fn(&Value) -> Result<(), String>;

const HOOKS: &[(&str, Hook)] = &[
    ("deny-base", deny_base::check),
    ("deny-push", deny_push::check),
    ("deny-dangerous-git", deny_dangerous_git::check),
    ("deny-no-verify", deny_no_verify::check),
    ("deny-workers-delete", deny_workers_delete::check),
    ("worker-boundary", worker_boundary::check),
    ("deny-foreground-subagent", deny_foreground_subagent::check),
];

/// A payload larger than this is refused unread: the command checks recurse per character, and
/// a crash would exit with a code other than 2, which Claude Code takes as a pass.
pub const PAYLOAD_LIMIT: usize = 64 * 1024;

/// The stack of the thread that checks: room for a payload at `PAYLOAD_LIMIT`. It is reserved,
/// not used, until a deep check needs it.
const CHECK_STACK: usize = 512 * 1024 * 1024;

/// A check that takes longer is refused: a hook Claude Code times out (60 s by default) does
/// not block the call. The matcher is not linear on some long commands.
const CHECK_BUDGET: std::time::Duration = std::time::Duration::from_secs(20);

pub fn main(args: &[String]) -> ExitCode {
    let name = args.first().map(String::as_str);
    let Some(&(_, hook)) = HOOKS.iter().find(|(n, _)| Some(*n) == name) else {
        let names: Vec<&str> = HOOKS.iter().map(|(n, _)| *n).collect();
        eprintln!("usage: makiba hook <{}>", names.join("|"));
        return ExitCode::from(2);
    };
    let mut raw = Vec::new();
    let verdict = match std::io::stdin()
        .take(PAYLOAD_LIMIT as u64 + 1)
        .read_to_end(&mut raw)
    {
        Err(e) => Err(format!("cannot read the hook payload: {e}")),
        Ok(n) if n > PAYLOAD_LIMIT => Err(format!(
            "the hook payload is larger than {PAYLOAD_LIMIT} bytes; split the command"
        )),
        Ok(_) => {
            let (tx, rx) = std::sync::mpsc::channel();
            let started = std::thread::Builder::new()
                .stack_size(CHECK_STACK)
                .spawn(move || {
                    let _ = tx.send(read_payload(&raw).and_then(|p| hook(&p)));
                });
            match started {
                Err(e) => Err(format!("cannot start the check: {e}")),
                // A panic drops the sender; a check that runs past the budget is not waited for.
                Ok(_) => rx.recv_timeout(CHECK_BUDGET).unwrap_or_else(|e| {
                    Err(match e {
                        std::sync::mpsc::RecvTimeoutError::Timeout => format!(
                            "the check took longer than {} s; split the command",
                            CHECK_BUDGET.as_secs()
                        ),
                        _ => "the check failed".into(),
                    })
                }),
            }
        }
    };
    verdict_exit(verdict)
}

/// Exit 0 for `Ok`; else the reason on standard error and exit 2.
pub fn verdict_exit(verdict: Result<(), String>) -> ExitCode {
    match verdict {
        Ok(()) => ExitCode::SUCCESS,
        Err(reason) => {
            eprintln!("makiba: refused: {reason}");
            ExitCode::from(2)
        }
    }
}

/// The payload, if it is one JSON object whose `tool_input` is absent, null or an object.
pub fn read_payload(raw: &[u8]) -> Result<Value, String> {
    let text = std::str::from_utf8(raw).map_err(|_| "the hook payload is not UTF-8".to_string())?;
    if text.trim().is_empty() {
        return Err("the hook payload is empty".into());
    }
    let payload: Value =
        serde_json::from_str(text).map_err(|e| format!("the hook payload is not JSON: {e}"))?;
    if !payload.is_object() {
        return Err("the hook payload is not a JSON object".into());
    }
    match payload.get("tool_input") {
        None | Some(Value::Null) | Some(Value::Object(_)) => Ok(payload),
        Some(_) => Err("the hook payload's tool_input is not a JSON object".into()),
    }
}

/// A field of the payload as `jq -r '<field> // empty'` prints it, with its trailing newlines
/// dropped as a shell's command substitution drops them.
pub fn field(payload: &Value, pointer: &str) -> String {
    let text = match payload.pointer(pointer) {
        None | Some(Value::Null) | Some(Value::Bool(false)) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
    };
    text.trim_end_matches('\n').to_string()
}

/// The Bash command of the payload, or empty.
pub fn command(payload: &Value) -> String {
    field(payload, "/tool_input/command")
}
