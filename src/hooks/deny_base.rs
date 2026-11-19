//! `makiba hook deny-base` (M4-1): a `PreToolUse` hook on Bash that refuses a command naming one of
//! the base's commands (`base::worker_denies().commands`) as a command word, or as a path whose
//! last component is one of them. It is the second layer: the role settings deny the command by
//! permission rule first, and the sandbox refuses the Unix socket the command would need.
//!
//! The payload reader fails closed, as claude-org-ja's `.hooks/lib` does: input that cannot be
//! read, is empty, is not JSON or carries no string `tool_input.command` is refused.
//!
//! The command check is not a shell parser; each simplification leans to refusing. Quotes and
//! backslashes are dropped, so a quoted or escaped name still reads as the name. A new command
//! starts after `;`, `&`, `|`, a newline, a parenthesis, a brace or a backtick, which covers
//! pipes, lists, subshells, groups and command substitution. Leading assignments and redirections
//! are skipped. After a wrapper (`env`, `sudo`, a shell, `eval`, `xargs`, ...) or find's `-exec`,
//! every later word of that command counts, so an argument equal to the name is refused there.
//!
//! Known limits: a name built by expansion (a variable, `$'...'`, a glob), a script file or
//! `source` that runs the command, and an alias are not seen.

use serde_json::Value;
use std::io::Read;
use std::process::ExitCode;

/// Words after which every later word of the same command may be the command that runs.
const WRAPPERS: &[&str] = &[
    "env", "sudo", "doas", "exec", "command", "builtin", "nohup", "nice", "ionice", "time",
    "timeout", "stdbuf", "setsid", "xargs", "eval", "bash", "sh", "zsh", "dash", "ksh", "fish",
    "if", "then", "else", "elif", "do", "while", "until", "!",
];

pub fn main() -> ExitCode {
    let mut raw = Vec::new();
    let verdict = match std::io::stdin().read_to_end(&mut raw) {
        Ok(_) => check_payload(&raw, &crate::base::worker_denies().commands),
        Err(e) => Err(format!("cannot read the hook payload: {e}")),
    };
    match verdict {
        Ok(()) => ExitCode::SUCCESS,
        Err(reason) => {
            eprintln!("makiba: refused: {reason}");
            ExitCode::from(2)
        }
    }
}

/// `Ok` when the payload is readable and its command names none of `denied`; else the reason.
pub fn check_payload(raw: &[u8], denied: &[String]) -> Result<(), String> {
    let text = std::str::from_utf8(raw).map_err(|_| "the hook payload is not UTF-8".to_string())?;
    if text.trim().is_empty() {
        return Err("the hook payload is empty".into());
    }
    let payload: Value =
        serde_json::from_str(text).map_err(|e| format!("the hook payload is not JSON: {e}"))?;
    let command = payload
        .pointer("/tool_input/command")
        .and_then(Value::as_str)
        .ok_or("the hook payload has no string tool_input.command")?;
    match named(command, denied) {
        Some(name) => Err(format!(
            "`{name}` is the base's command; no agent role may run it (spec section 1). Ask through the door's tools instead"
        )),
        None => Ok(()),
    }
}

/// The first of `denied` that `command` runs, as far as the check above can tell.
pub fn named<'a>(command: &str, denied: &'a [String]) -> Option<&'a str> {
    let flat: String = command
        .chars()
        .filter(|c| !matches!(c, '\'' | '"' | '\\'))
        .map(|c| match c {
            ';' | '&' | '|' | '\n' | '(' | ')' | '{' | '}' | '`' => '\n',
            c => c,
        })
        .collect();
    for segment in flat.lines() {
        // A redirection written against a word (`cmd>out`) is a word of its own.
        let mut words = segment
            .split_whitespace()
            .flat_map(|w| match w.find(['<', '>']) {
                Some(i) if !w[..i].chars().all(|c| c.is_ascii_digit()) => vec![&w[..i], &w[i..]],
                _ => vec![w],
            });
        let mut wrapped = false;
        let mut past_command = false;
        while let Some(w) = words.next() {
            if past_command {
                // An argument, unless find's -exec family starts a command here.
                wrapped = w.starts_with("-exec") || w.starts_with("-ok");
                past_command = !wrapped;
                continue;
            }
            if !wrapped && is_assignment(w) {
                continue;
            }
            if !wrapped && is_redirection(w) {
                if w.trim_start_matches(|c: char| c.is_ascii_digit())
                    .trim_start_matches(['<', '>', '&'])
                    .is_empty()
                {
                    words.next();
                }
                continue;
            }
            let base = w.rsplit('/').next().unwrap_or(w);
            if let Some(d) = denied.iter().find(|d| d.as_str() == base) {
                return Some(d);
            }
            if WRAPPERS.contains(&base) {
                wrapped = true;
            } else if !wrapped {
                past_command = true;
            }
        }
    }
    None
}

fn is_assignment(w: &str) -> bool {
    w.split_once('=').is_some_and(|(name, _)| {
        name.chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

fn is_redirection(w: &str) -> bool {
    w.trim_start_matches(|c: char| c.is_ascii_digit())
        .starts_with(['<', '>'])
}

#[cfg(test)]
mod tests {
    use super::named;

    fn hit(command: &str) -> bool {
        named(command, &["tool".to_string()]).is_some()
    }

    #[test]
    fn command_words_are_found_past_quotes_lists_and_substitutions() {
        for c in [
            "tool ls",
            "/opt/bin/tool ls",
            "'tool' ls",
            "tool>/dev/null",
            "/opt/bin/tool<in",
            "tool 2>&1",
            "t\\ool ls",
            "X=1 tool",
            ">log tool",
            "2> log tool",
            "echo a; tool",
            "echo a && tool",
            "cat f | tool",
            "echo $(tool ls)",
            "echo `tool`",
            "(tool)",
            "env -u X tool",
            "sudo -u me tool",
            "bash -c 'tool ls'",
            "find . -exec tool {} ;",
            "if true; then tool; fi",
        ] {
            assert!(hit(c), "{c}");
        }
    }

    #[test]
    fn arguments_and_longer_names_pass() {
        for c in [
            "echo tool",
            "grep -rn tool docs",
            "tools ls",
            "/opt/tool/bin/other",
            "cat tool.md",
        ] {
            assert!(!hit(c), "{c}");
        }
    }
}
