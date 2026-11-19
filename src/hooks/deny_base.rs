//! `makiba hook deny-base` (M4-1): a `PreToolUse` hook on Bash that refuses a command naming one of
//! the base's commands (`base::worker_denies().commands`) as a command word, or as a path whose
//! last component is one of them. It is the second layer: the role settings deny the command by
//! permission rule first, and the sandbox refuses the Unix socket the command would need.
//!
//! The payload is read by `hooks::read_payload`, which fails closed; a payload with no string
//! `tool_input.command` is refused too.
//!
//! The command check is not a shell parser; each simplification leans to refusing. The command is
//! read twice, and refused if either reading finds the name. The first keeps quoted text as one
//! word and redirections (`2>&1`, `>out`) as words of their own. The second drops quotes and
//! backslashes, so a name inside a quoted command substitution still reads as the name. In both, a
//! new command starts after `;`, `&`, `|`, a newline, a parenthesis, a brace or a backtick, which
//! covers pipes, lists, subshells, groups and command substitution. Leading assignments and
//! redirections are skipped. After a wrapper (`env`, `sudo`, a shell, `eval`, `xargs`, ...) or
//! find's `-exec`, every later word of that command counts, so an argument equal to the name is
//! refused there.
//!
//! Known limits: a name built by expansion (a variable, `$'...'`, a glob), a script file or
//! `source` that runs the command, and an alias are not seen.

use serde_json::Value;

/// Words after which every later word of the same command may be the command that runs.
const WRAPPERS: &[&str] = &[
    "env", "sudo", "doas", "exec", "command", "builtin", "nohup", "nice", "ionice", "time",
    "timeout", "stdbuf", "setsid", "xargs", "eval", "bash", "sh", "zsh", "dash", "ksh", "fish",
    "if", "then", "else", "elif", "do", "while", "until", "!",
];

/// `Ok` when the payload's command names none of the base's commands; else the reason.
pub fn check(payload: &Value) -> Result<(), String> {
    let command = payload
        .pointer("/tool_input/command")
        .and_then(Value::as_str)
        .ok_or("the hook payload has no string tool_input.command")?;
    match named(command, &crate::base::worker_denies().commands) {
        Some(name) => Err(format!(
            "`{name}` is the base's command; no agent role may run it (spec section 1). Ask through the door's tools instead"
        )),
        None => Ok(()),
    }
}

/// The first of `denied` that `command` runs, as far as the check above can tell.
pub fn named<'a>(command: &str, denied: &'a [String]) -> Option<&'a str> {
    // A backslash-newline continues the line, so the shell joins the words around it.
    let command = &command.replace("\\\n", "");
    [quoted(command), flattened(command)]
        .iter()
        .flatten()
        .find_map(|words| first_run(words, denied))
}

/// The commands of `command`, each as its words, with quoting kept.
fn quoted(command: &str) -> Vec<Vec<String>> {
    let mut segments = vec![Vec::new()];
    let mut word = String::new();
    let mut started = false;
    let mut chars = command.chars();
    fn end(segments: &mut [Vec<String>], word: &mut String, started: &mut bool) {
        if *started {
            segments.last_mut().unwrap().push(std::mem::take(word));
            *started = false;
        }
    }
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                word.extend(chars.by_ref().take_while(|&c| c != '\''));
                started = true;
            }
            '"' => {
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' => word.extend(chars.next()),
                        c => word.push(c),
                    }
                }
                started = true;
            }
            '\\' => {
                word.extend(chars.next());
                started = true;
            }
            c if c.is_whitespace() && c != '\n' => end(&mut segments, &mut word, &mut started),
            '&' if word.ends_with(['<', '>']) => word.push(c),
            '<' | '>' => {
                if !word.chars().all(|c| c.is_ascii_digit() || c == '&') {
                    end(&mut segments, &mut word, &mut started);
                }
                word.push(c);
                started = true;
            }
            ';' | '&' | '|' | '\n' | '(' | ')' | '{' | '}' | '`' => {
                end(&mut segments, &mut word, &mut started);
                segments.push(Vec::new());
            }
            c => {
                word.push(c);
                started = true;
            }
        }
    }
    end(&mut segments, &mut word, &mut started);
    segments
}

/// The commands of `command`, each as its words, with quotes and backslashes dropped.
fn flattened(command: &str) -> Vec<Vec<String>> {
    let flat: String = command
        .chars()
        .filter(|c| !matches!(c, '\'' | '"' | '\\'))
        .map(|c| match c {
            ';' | '&' | '|' | '\n' | '(' | ')' | '{' | '}' | '`' => '\n',
            c => c,
        })
        .collect();
    flat.lines()
        .map(|segment| {
            segment
                .split_whitespace()
                .flat_map(split_redirection)
                .collect()
        })
        .collect()
}

/// A word split at its first `<` or `>`, unless what comes before is only a descriptor (`2>&1`).
fn split_redirection(word: &str) -> Vec<String> {
    match word.find(['<', '>']) {
        Some(i) if !word[..i].chars().all(|c| c.is_ascii_digit() || c == '&') => {
            vec![word[..i].to_string(), word[i..].to_string()]
        }
        _ => vec![word.to_string()],
    }
}

/// The first of `denied` that the command of `words` runs.
fn first_run<'a>(words: &[String], denied: &'a [String]) -> Option<&'a str> {
    let mut words = words.iter().map(String::as_str);
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
        if is_redirection(w) {
            // A bare operator (`>`, `2>`, `&>`) takes the next word as its target.
            if w.trim_matches(|c: char| c.is_ascii_digit() || matches!(c, '<' | '>' | '&'))
                .is_empty()
                && !w.ends_with(|c: char| c.is_ascii_digit())
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
        .starts_with(['<', '>', '&'])
        && w.contains(['<', '>'])
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
            "2>&1 /opt/bin/tool ls",
            ">&2 tool",
            "&>log tool",
            "X='a b' /opt/bin/tool ls",
            "echo \"$(tool ls)\"",
            "/opt/bin/to\\\nol ls",
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
            "bash -c 'tool>/dev/null'",
            "sh -c \"/opt/bin/tool<in\"",
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
