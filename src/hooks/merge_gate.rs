//! `makiba hook merge-gate` (Issue 47): the secretary's `PreToolUse` hook on Bash, ported from
//! claude-org-ja's `.hooks/gate-acceptance-before-merge.sh` and `tools/acceptance_merge_hook.py`.
//!
//! The only merge it lets through is the one form
//!
//! ```text
//! gh pr merge <number> --repo <OWNER/REPO> (--squash|--merge|--rebase) --match-head-commit <40-hex> [--delete-branch]
//! ```
//!
//! as the whole command, and only when makibad's acceptance gate (the door verb `gate`) answers
//! `allow`. It asks with the key makibad wrote for this hook alone (spec section 3); anything that
//! keeps it from asking refuses.
//!
//! It refuses, where the program is the command (after assignments and wrappers, with quotes,
//! known variables, `eval` and `bash -c` read as the shell reads them):
//!
//! - `gh pr merge` in any other form;
//! - `gh api` with a writing method on a `pulls/<n>/merge` path, and `gh api graphql` carrying a
//!   merge mutation or a body it cannot read;
//! - `gh alias` that defines or imports a merge;
//! - `curl` or `wget` naming the GitHub API;
//! - a command word or a `gh` subcommand it cannot read, a program that runs code given as an
//!   argument (`python -c`, `sed`, `xargs`, ...) with `gh`, a variable or a substitution in it, a
//!   command substitution anywhere: each of these when the command also has the word merge.
//!
//! A command that only names one of these as an argument (`echo "gh pr merge"`, a commit message,
//! a `grep` pattern) is not refused. This stops mistakes, not an adversary: an alias in the
//! person's gh configuration, the GitHub web UI and other sessions are outside it. The bound
//! against merging around the gate is GitHub's branch protection, not this hook.

use super::deny_push;
use super::shell;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

/// How long the hook waits for the gate: below the 900 s the role settings give it.
pub const GATE_WAIT: Duration = Duration::from_secs(840);

const FORM: &str = "merge with exactly: gh pr merge <number> --repo <OWNER/REPO> --squash|--merge|--rebase --match-head-commit <full head SHA> [--delete-branch], alone on one line, so the acceptance gate can read it. A PR this form cannot merge (for example a repository without CI) is merged by the person in the GitHub web UI";

pub fn check(payload: &Value) -> Result<(), String> {
    if super::field(payload, "/tool_name") != "Bash" {
        return Ok(());
    }
    let command = super::command(payload);
    match parse(&command)? {
        None => Ok(()),
        Some((pr, repo, head)) => ask_gate(&repo, pr, &head),
    }
}

/// `(number, OWNER/REPO, head)` for the allowed form; `None` for a command that cannot merge; the
/// reason for one the hook refuses.
pub fn parse(command: &str) -> Result<Option<(i64, String, String)>, String> {
    if let Some(m) = allowed_form(command.trim()) {
        return Ok(Some(m));
    }
    match refused(command) {
        Some(why) => Err(format!("{why}; {FORM}")),
        None => Ok(None),
    }
}

/// The allowed form, read exactly: single spaces, a lowercase full SHA, nothing before or after.
fn allowed_form(c: &str) -> Option<(i64, String, String)> {
    let w: Vec<&str> = c.split(' ').collect();
    if !(9..=10).contains(&w.len()) || w[..3] != ["gh", "pr", "merge"] || w[4] != "--repo" {
        return None;
    }
    let number = w[3]
        .parse::<i64>()
        .ok()
        .filter(|_| w[3].bytes().all(|b| b.is_ascii_digit()))?;
    let name_ok = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    };
    let (owner, name) = w[5].split_once('/')?;
    let sha = w[8];
    let ok = name_ok(owner)
        && name_ok(name)
        && ["--squash", "--merge", "--rebase"].contains(&w[6])
        && w[7] == "--match-head-commit"
        && sha.len() == 40
        && sha
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && (w.len() == 9 || w[9] == "--delete-branch");
    ok.then(|| (number, w[5].to_string(), sha.to_string()))
}

/// True when `s` has the word merge on its own (not inside a longer name such as
/// `merge_approval_mode`, `mergedAt` or `before-merge.sh`), or a GraphQL merge mutation's name.
fn merge_word(s: &str) -> bool {
    let s = s.to_lowercase();
    if s.contains("mergepullrequest") || s.contains("automerge") || s.contains("mergebranch") {
        return true;
    }
    let b = s.as_bytes();
    let alnum = |i: usize| b.get(i).is_some_and(|c| c.is_ascii_alphanumeric());
    let joiner = |i: usize| b.get(i).is_some_and(|c| *c == b'_' || *c == b'-');
    s.match_indices("merge").any(|(i, _)| {
        let before = i > 0 && (alnum(i - 1) || (i > 1 && joiner(i - 1) && alnum(i - 2)));
        let end = i + 5;
        let after = alnum(end) || (joiner(end) && alnum(end + 1));
        !before && !after
    })
}

/// Whether the command has the word merge as the shell may run it (carried `parse`): quotes and
/// continuations joined, and with a `${...}` operation or a variable named for it, anywhere.
fn mentions_merge(command: &str) -> bool {
    let base = command
        .replace("\\\n", "")
        .replace('\n', " ")
        .to_lowercase();
    let plain: String = base
        .chars()
        .filter(|c| !matches!(c, '"' | '\'' | '\\'))
        .collect();
    let op = regex::Regex::new(r"\$\{[^}]*[#%/:^,]|\$\{?\w*merge").expect("pattern");
    if op.is_match(&base) {
        return plain.contains("merge");
    }
    // An expansion adds no text of its own (a word break), except the default word in `${A-x}`.
    let braces = regex::Regex::new(r"\$\{[^}]*\}?").expect("pattern");
    let s = braces.replace_all(&base, |c: &regex::Captures| {
        c[0].chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || ch == '_' {
                    ch
                } else {
                    ' '
                }
            })
            .collect::<String>()
    });
    let s = regex::Regex::new(r"\$(\w+|.?)")
        .expect("pattern")
        .replace_all(&s, " ");
    let unquote = |v: &str| v.replace(['"', '\''], "");
    let escaped = regex::Regex::new(r"\\.?")
        .expect("pattern")
        .replace_all(&s, " ");
    merge_word(&unquote(&s.replace('\\', ""))) || merge_word(&unquote(&escaped))
}

const WRAPPERS: &[&str] = &[
    "env", "command", "builtin", "exec", "sudo", "nohup", "time", "nice", "timeout", "doas",
    "stdbuf", "setsid", "xargs", "then", "do", "else", "elif", "if", "while", "until", "!", "{",
];
/// Programs that run code or a program given in their arguments.
const RUNNERS: &[&str] = &[
    "sh", "bash", "zsh", "dash", "ksh", "fish", "python", "python3", "py", "node", "perl", "ruby",
    "php", "lua", "sed", "awk", "gawk", "less", "find", "eval", "source", ".", "watch", "script",
    "parallel", "make", "just", "npx", "uvx",
];

/// The program's own name: the last part of a path, without `.exe`, lowercased.
fn program(word: &str) -> String {
    let lower = word.to_lowercase();
    let base = lower.rsplit(['/', '\\']).next().unwrap_or(&lower);
    base.strip_suffix(".exe").unwrap_or(base).to_string()
}

fn opaque(word: &str) -> bool {
    word.contains(['$', '`'])
}

/// A redirection word: `>f`, `2>>f`, `&>f`, `<f`, or the operator alone (its target follows).
fn redirection(w: &str) -> Option<bool> {
    let rest = w.trim_start_matches(|c: char| c.is_ascii_digit() || c == '&');
    let op = rest.trim_start_matches(['<', '>', '&', '|']);
    rest.starts_with(['<', '>']).then_some(op.is_empty())
}

/// The words of a segment from its command word on, wrappers, assignments and redirections
/// skipped (a redirection may come before the program).
fn command_words(words: &[String]) -> &[String] {
    let mut i = 0;
    while i < words.len() {
        let w = &words[i];
        if let Some(alone) = redirection(w) {
            i += if alone { 2 } else { 1 };
            continue;
        }
        let assignment = w.split_once('=').is_some_and(|(n, _)| {
            !n.is_empty() && n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        });
        if assignment
            || w.starts_with('-') && i > 0 && WRAPPERS.contains(&program(&words[i - 1]).as_str())
        {
            i += 1;
            continue;
        }
        let p = program(w);
        if WRAPPERS.contains(&p.as_str()) {
            i += 1;
            // `timeout <duration>`, `nice -n <n>`, `sudo -u <user>` and option words.
            while i < words.len()
                && (words[i].starts_with('-')
                    || (p == "timeout"
                        && words[i].chars().next().is_some_and(|c| c.is_ascii_digit())))
            {
                let takes = matches!(
                    words[i].as_str(),
                    "-n" | "-u" | "-g" | "-I" | "-i" | "-s" | "-k" | "-C" | "-P" | "-L" | "-d"
                );
                i += if takes { 2 } else { 1 };
            }
            continue;
        }
        break;
    }
    &words[i.min(words.len())..]
}

/// Why the command is refused, or `None`.
fn refused(command: &str) -> Option<String> {
    let mentions = mentions_merge(command);
    // A substitution runs what it holds, which this reading does not follow everywhere.
    if mentions
        && (command.contains("$(")
            || command.contains('`')
            || command.contains("<(")
            || command.contains(">("))
    {
        return Some("a command substitution with the word merge cannot be read".into());
    }
    let (segments, assigns) = shell::segments_and_assignments(command);
    if mentions
        && (shell::split_segments(command).1 || shell::unreadable(&segments, &assigns).is_some())
    {
        return Some("the command cannot be read as the shell runs it".into());
    }
    for segment in segments.iter().filter(|s| !s.trim().is_empty()) {
        // The word may come from an assignment in another segment (`x=merge_; gh pr ${x%_}`).
        let seg_mentions = mentions || mentions_merge(segment);
        // As written and with the known variables expanded: a `${x%_}` the expansion would
        // resolve to a longer name stays unreadable as written.
        for variant in [segment.clone(), shell::expand(segment, &assigns)] {
            if let Some(why) = refused_segment(&variant, segment, seg_mentions) {
                return Some(why);
            }
        }
    }
    None
}

/// The words of `text` as the shell splits them: blanks inside quotes or after a backslash do not
/// split, and the quotes and backslashes are then removed (`NOTE="two words" gh` is two words).
/// An unclosed quote runs to the end.
fn shell_words(text: &str) -> Vec<String> {
    let (mut words, mut word, mut quote, mut any) = (Vec::new(), String::new(), None, false);
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (None, '\'' | '"') => {
                quote = Some(c);
                any = true;
            }
            (Some(q), _) if c == q => quote = None,
            (None | Some('"'), '\\') => {
                if let Some(next) = chars.next() {
                    word.push(next);
                }
                any = true;
            }
            (None, _) if c.is_whitespace() => {
                if any || !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
                any = false;
            }
            _ => word.push(c),
        }
    }
    if any || !word.is_empty() {
        words.push(word);
    }
    words
}

/// One segment's words, quotes removed: why it is refused, if it is.
fn refused_segment(variant: &str, segment: &str, seg_mentions: bool) -> Option<String> {
    let text: String = variant
        .chars()
        .filter(|c| !matches!(c, '"' | '\'' | '\\'))
        .collect();
    let mentions = seg_mentions || mentions_merge(&text);
    let words: Vec<String> = shell_words(variant)
        .into_iter()
        .map(|w| w.trim_start_matches(['(', '{']).to_string())
        .filter(|w| !w.is_empty())
        .collect();
    let cmd = command_words(&words);
    let first = cmd.first()?;
    if opaque(first) {
        return mentions.then(|| format!("the program {first} cannot be read"));
    }
    let p = program(first);
    let args = &cmd[1..];
    match p.as_str() {
        "gh" => refused_gh(args, &text, mentions),
        "curl" | "wget" | "http" | "https" | "xh" => args
            .iter()
            .any(|a| a.to_lowercase().contains("api.github.com"))
            .then(|| format!("{p} to the GitHub API")),
        // ripgrep runs a program only with --pre; its pattern is a mention.
        "rg" => (mentions && args.iter().any(|a| a == "--pre" || a.starts_with("--pre=")))
            .then(|| "rg --pre runs a program, with the word merge".to_string()),
        _ if RUNNERS.contains(&p.as_str()) => {
            let gh_word = regex::Regex::new(r"(?i)(^|[^a-z0-9_.-])gh(\.exe)?([^a-z0-9_-]|$)")
                .expect("pattern");
            let code = args.join(" ");
            let runs_gh = gh_word.is_match(&code) || code.contains(['$', '`']);
            (mentions && (runs_gh || segment.contains(['$', '`'])))
                .then(|| format!("{p} runs code that names gh and merge"))
        }
        _ => None,
    }
}

/// A `gh` call (its arguments): why it is refused, if it is.
fn refused_gh(args: &[String], line: &str, mentions: bool) -> Option<String> {
    let f: Vec<&str> = args.iter().map(String::as_str).collect();
    // Global flags; -R / --repo take a value.
    let mut i = 0;
    while i < f.len() && f[i].starts_with('-') {
        i += if matches!(f[i], "-R" | "--repo" | "--hostname") {
            2
        } else {
            1
        };
    }
    let group = f.get(i)?;
    if opaque(group) {
        return mentions.then(|| format!("gh's command {group} cannot be read"));
    }
    let group = group.to_lowercase();
    if group == "api" {
        // gh's own word list: `gh` then `api` at index 1, as the push guard reads it.
        let mut words = vec!["gh"];
        words.extend(&f[i..]);
        let verdicts = deny_push::classify_api(&words, 1, line);
        let endpoint = deny_push::next_word(&words, 2).map(|e| words[e].to_lowercase());
        let graphql = endpoint.as_deref() == Some("graphql");
        let merge_path =
            regex::Regex::new(r"(^|/)repos/[^/]+/[^/]+/pulls/[^/]+/merge/?$").expect("pattern");
        if graphql {
            if verdicts.iter().any(|v| v.contains("undecidable")) {
                return Some("gh api graphql with a body the hook cannot read".into());
            }
            if merge_word(line) {
                return Some("gh api graphql with a merge mutation".into());
            }
            return None;
        }
        let path = endpoint.unwrap_or_default();
        let path = path.split('?').next().unwrap_or_default().to_string();
        if merge_path.is_match(&path) && !verdicts.is_empty() {
            return Some(format!("gh api writing to {path}"));
        }
        if opaque(&path) && mentions && !verdicts.is_empty() {
            return Some("gh api writing to a path the hook cannot read".into());
        }
        return None;
    }
    let sub = deny_push::next_word(&f, i + 1).map(|s| f[s]);
    match sub {
        Some(s) if opaque(s) => mentions.then(|| format!("gh's subcommand {s} cannot be read")),
        Some(s) => {
            let s = s.to_lowercase();
            if group == "pr" && s == "merge" {
                return Some("gh pr merge in another form than the allowed one".into());
            }
            if group == "alias" && (s == "set" || s == "import") && mentions {
                return Some("gh alias naming merge".into());
            }
            None
        }
        None => None,
    }
}

/// Ask makibad's gate; `Ok` only for an answer that allows the merge.
fn ask_gate(repo: &str, pr: i64, head: &str) -> Result<(), String> {
    let key = std::fs::read_to_string(crate::cli::gate_key_path())
        .map_err(|e| {
            format!(
                "cannot read the merge gate's key {}: {e}; is makibad running?",
                crate::cli::gate_key_path().display()
            )
        })?
        .trim()
        .to_string();
    let path = crate::cli::door_path()
        .filter(|_| !crate::cli::door_off())
        .ok_or("no door to ask the acceptance gate (MAKIBA_DOOR)")?;
    let req = json!({"v": 1, "key": key, "verb": "gate",
                     "args": {"repo": repo, "pr": pr, "head": head}});
    let mut s = UnixStream::connect(&path)
        .map_err(|e| format!("cannot reach makibad at {}: {e}", path.display()))?;
    let io = |e: std::io::Error| format!("the door: {e}");
    s.set_read_timeout(Some(GATE_WAIT)).map_err(io)?;
    writeln!(s, "{req}").map_err(io)?;
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line).map_err(io)?;
    let answer: Value =
        serde_json::from_str(&line).map_err(|e| format!("the door answered {line:?}: {e}"))?;
    if answer["ok"] != json!(true) {
        return Err(format!(
            "the acceptance gate could not answer: {}",
            answer["error"]["message"].as_str().unwrap_or("no reason")
        ));
    }
    let r = &answer["result"];
    if r["allow"] == json!(true) {
        return Ok(());
    }
    Err(format!(
        "ACCEPTANCE_GATE {} for {repo}#{pr} (merge_approval_mode {}): {}",
        r["verdict"].as_str().unwrap_or("?"),
        r["mode"].as_str().unwrap_or("?"),
        r["reason"].as_str().unwrap_or("")
    ))
}
