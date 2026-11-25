//! `makiba hook <name>`: the Claude Code hooks a role's settings name (spec section 14). Each reads
//! the hook payload on standard input; exit 0 lets the tool call run, exit 2 refuses it with the
//! reason on standard error.
//!
//! Every hook reads its payload with `read_payload`, which fails closed as claude-org-ja's
//! `.hooks/lib` does (inventory HK-1): input that cannot be read, is empty or blank, is not one
//! JSON object, or carries a `tool_input` that is neither absent, null nor an object, is refused.
//!
//! Every refusal names the hook, the line it holds (its id in docs/design/lines.md) and the part
//! of the input that matched (Issue 306), so that the agent need not guess which rule stopped it:
//!
//! ```text
//! makiba: refused by hook <name> (line <id> in docs/design/lines.md): <reason>. Matched: "<text>"
//! ```

use serde_json::Value;
use std::io::Read;
use std::process::ExitCode;

pub mod deny_base;
pub mod deny_dangerous_git;
pub mod deny_foreground_subagent;
pub mod deny_no_verify;
pub mod deny_push;
pub mod deny_workers_delete;
pub mod git_hook;
pub mod merge_gate;
pub mod premise_check;
pub mod re;
pub mod shell;
pub mod test_commit;
pub mod worker_boundary;

/// Why a hook refuses: the line of docs/design/lines.md it holds, the part of the input that
/// matched, and the reason with what to do instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub line: &'static str,
    pub matched: String,
    pub reason: String,
    /// The words inside `matched` that the hook refused on, kept in view when `matched` is cut.
    pub key: Option<String>,
}

impl Refusal {
    pub fn new(line: &'static str, matched: impl Into<String>, reason: impl Into<String>) -> Self {
        Refusal {
            line,
            matched: matched.into(),
            reason: reason.into(),
            key: None,
        }
    }

    /// The refusal with `key`, the words inside the matched text that were refused on.
    pub fn at(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }

    /// The line a refusal by hook `name` prints.
    pub fn message(&self, name: &str) -> String {
        format!(
            "makiba: refused by hook {name} (line {} in docs/design/lines.md): {}. Matched: \"{}\"",
            self.line,
            self.reason.trim_end_matches('.'),
            shown(&self.matched, self.key.as_deref())
        )
    }
}

/// The longest matched text a refusal shows, in characters.
const SHOWN: usize = 200;

/// `text` on one line (a newline shown as `\n`). Text longer than `SHOWN` characters is cut in
/// the middle, so that its start and its end are shown; when `key` is inside the cut part, the
/// part around `key` is shown in place of the end.
fn shown(text: &str, key: Option<&str>) -> String {
    let one = text.trim().replace('\n', "\\n");
    let chars: Vec<char> = one.chars().collect();
    if chars.len() <= SHOWN {
        return one;
    }
    let take = |from: usize, to: usize| chars[from..to].iter().collect::<String>();
    let half = SHOWN / 2;
    let key_at = key
        .map(|k| k.trim().replace('\n', "\\n"))
        .filter(|k| !k.is_empty())
        .and_then(|k| {
            one.find(&k)
                .map(|byte| (one[..byte].chars().count(), k.chars().count()))
        });
    match key_at {
        // The key ends after the head and starts before the tail: show the part around it.
        Some((at, len)) if at + len > half && at < chars.len() - half => {
            let head = half.min(at);
            let around = half.max(len);
            let from = at.saturating_sub((around - len) / 2).max(head);
            let to = (from + around).min(chars.len());
            let gap = if from > head { "..." } else { "" };
            let tail = if to < chars.len() { "..." } else { "" };
            format!("{}{gap}{}{tail}", take(0, head), take(from, to))
        }
        _ => format!(
            "{}...{}",
            take(0, half),
            take(chars.len() - half, chars.len())
        ),
    }
}

/// The line a hook that cannot read its input holds.
const FAIL_CLOSED: &str = "L-K11";

/// A hook: the payload in, `Err` to refuse.
type Hook = fn(&Value) -> Result<(), Refusal>;

const HOOKS: &[(&str, Hook)] = &[
    ("deny-base", deny_base::check),
    ("deny-push", deny_push::check),
    ("deny-dangerous-git", deny_dangerous_git::check),
    ("deny-no-verify", deny_no_verify::check),
    ("deny-workers-delete", deny_workers_delete::check),
    ("worker-boundary", worker_boundary::check),
    ("deny-foreground-subagent", deny_foreground_subagent::check),
    ("merge-gate", merge_gate::check),
];

/// A Bash command longer than this (in bytes) is refused unchecked: the command checks recurse
/// per character, and a crash would exit with a code other than 2, which Claude Code takes as a
/// pass. Only the command counts, so a large file written by the file tools is not refused.
pub const COMMAND_LIMIT: usize = 64 * 1024;

/// The stack of the thread that checks: room for a command at `COMMAND_LIMIT`. It is reserved,
/// not used, until a deep check needs it.
const CHECK_STACK: usize = 512 * 1024 * 1024;

/// A check that takes longer is refused: a hook Claude Code times out (60 s by default) does
/// not block the call. The matcher is not linear on some long commands.
const CHECK_BUDGET: std::time::Duration = std::time::Duration::from_secs(20);

pub fn main(args: &[String]) -> ExitCode {
    let name = args.first().map(String::as_str);
    // The git hooks of the secret scanner (Issue 136) are run by git and read no payload.
    if let Some(git) = git_hook::NAMES.iter().find(|n| Some(**n) == name) {
        return git_hook::main(git, &args[1..]);
    }
    // The premise reminder (Issue 269) only prints a line and never refuses.
    if name == Some("premise-check") {
        return premise_check::main();
    }
    let Some(&(_, hook)) = HOOKS.iter().find(|(n, _)| Some(*n) == name) else {
        let names: Vec<&str> = HOOKS
            .iter()
            .map(|(n, _)| *n)
            .chain(git_hook::NAMES)
            .chain(["premise-check"])
            .collect();
        eprintln!("usage: makiba hook <{}>", names.join("|"));
        return ExitCode::from(2);
    };
    // Every hook, the merge gate too (Issue 433: it asks no door), answers within the same budget.
    let budget = CHECK_BUDGET;
    let mut raw = Vec::new();
    let verdict = match std::io::stdin().read_to_end(&mut raw) {
        Err(e) => Err(payload_refused(format!(
            "cannot read the hook payload: {e}"
        ))),
        Ok(_) => {
            // What the check reads, for a refusal of the frame: the command, or the payload.
            let seen = read_payload(&raw)
                .map(|p| command(&p))
                .ok()
                .filter(|c| !c.is_empty())
                .unwrap_or_else(|| "the hook payload".into());
            let (tx, rx) = std::sync::mpsc::channel();
            let started = std::thread::Builder::new()
                .stack_size(CHECK_STACK)
                .spawn(move || {
                    let _ = tx.send(read_payload(&raw).map_err(payload_refused).and_then(|p| {
                        let len = command(&p).len();
                        if len > COMMAND_LIMIT {
                            return Err(Refusal::new(
                                FAIL_CLOSED,
                                format!("the command, {len} bytes"),
                                format!(
                                    "the command is longer than {COMMAND_LIMIT} bytes; split it, or write long text with the file tools"
                                ),
                            ));
                        }
                        hook(&p)
                    }));
                });
            match started {
                Err(e) => Err(Refusal::new(
                    FAIL_CLOSED,
                    seen,
                    format!("cannot start the check: {e}"),
                )),
                // A panic drops the sender; a check that runs past the budget is not waited for.
                Ok(_) => rx.recv_timeout(budget).unwrap_or_else(|e| {
                    let reason = match e {
                        std::sync::mpsc::RecvTimeoutError::Timeout => format!(
                            "the check took longer than {} s; split the command",
                            budget.as_secs()
                        ),
                        _ => "the check failed".into(),
                    };
                    Err(Refusal::new(FAIL_CLOSED, seen, reason))
                }),
            }
        }
    };
    verdict_exit(name.unwrap_or_default(), verdict)
}

/// A refusal of a payload that cannot be read (line L-K11).
fn payload_refused(reason: String) -> Refusal {
    Refusal::new(FAIL_CLOSED, "the hook payload", reason)
}

/// Exit 0 for `Ok`; else the refusal, naming hook `name`, on standard error and exit 2.
pub fn verdict_exit(name: &str, verdict: Result<(), Refusal>) -> ExitCode {
    match verdict {
        Ok(()) => ExitCode::SUCCESS,
        Err(refusal) => {
            eprintln!("{}", refusal.message(name));
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

#[cfg(test)]
mod tests {
    use super::{SHOWN, shown};

    #[test]
    fn a_long_match_keeps_its_end_and_its_key_in_view() {
        let short = "git push origin main";
        assert_eq!(shown(short, None), short);
        let tail = format!("echo {}; git push", "x".repeat(250));
        assert!(shown(&tail, None).ends_with("; git push"));
        let middle = format!("{} --no-verify {}", "a".repeat(250), "b".repeat(250));
        let cut = shown(&middle, Some("--no-verify"));
        assert!(cut.contains("--no-verify"), "{cut}");
        assert!(cut.chars().count() <= SHOWN + 6, "{cut}");
        let no_key = shown(&middle, None);
        assert!(no_key.starts_with("aaa") && no_key.ends_with("bbb"));
        // A key that starts inside the head is shown whole, not split by the cut.
        let straddle = format!("{} --no-verify {}", "a".repeat(95), "b".repeat(300));
        assert!(shown(&straddle, Some("--no-verify")).contains("--no-verify"));
    }
}
