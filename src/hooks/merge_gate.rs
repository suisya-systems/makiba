//! `makiba hook merge-gate` (Issue 47, Issue 433): the secretary's `PreToolUse` hook on Bash, ported
//! from claude-org-ja's `.hooks/gate-acceptance-before-merge.sh` and `tools/acceptance_merge_hook.py`.
//!
//! It lets no landing through (docs/design/merge-act.md section 8): makibad runs the landing itself
//! when its acceptance gate allows it (the door verb `merge`), so every landing or enqueue written
//! on the secretary's command line is refused, and the refusal names the verb. It asks no door and
//! reads no key: it answers the same with makibad down.
//!
//! It reads each simple command of the line (after `eval` and `bash -c` are unwrapped) as the
//! shell splits it into words. For a command whose program is the GitHub CLI, every word up to and
//! including its subcommand must be read with certainty (for `gh api`, `curl` and `wget`, every
//! word): plain characters, quotes without `$`, backquotes or backslashes, a redirection as a word
//! of its own. Anything else there is refused, with a request to write the command plainly. Then
//! it refuses:
//!
//! - `gh pr merge` in any form, `gh alias set|import`, and a `gh` group it does not know (an alias
//!   or an extension);
//! - `gh api` with a writing method on a `pulls/<n>/merge*` path (`merge`, `merge-async`, any
//!   later sibling), and `gh api graphql` carrying a merge or enqueue mutation or a body it cannot
//!   read;
//! - `curl` or `wget` naming the GitHub API;
//! - a program word it cannot read; the GitHub CLI among the words of a program that may run its
//!   arguments (anything but a few text programs such as `echo`, `grep`, `git`); `xargs`, `find`
//!   or `parallel` feeding it; and, when the command has the word merge, a program that runs code
//!   naming gh, or a command substitution anywhere.
//!
//! Wrappers it reads through: `env` (not `env -S`), `command` (`command -v` only looks a name up),
//! `builtin`, `exec`, `nohup`, `time`, `nice`, `timeout`, `sudo`, `doas`, `stdbuf`, `setsid` and
//! the shell keywords before a command.
//!
//! A text program that only names one of these as an argument (`echo "gh pr merge"`, a commit
//! message, a `grep` pattern) is not refused. Taking a pull request out of the merge queue
//! (`dequeuePullRequest`) only narrows and passes. This stops mistakes, not an adversary: a renamed
//! binary, a script on disk, other HTTP clients and the GitHub web UI are outside it. The bound
//! against landing around makibad is GitHub's branch protection, not this hook.

use super::deny_push;
use super::shell;
use serde_json::Value;

/// What every refusal ends with: the way a landing goes now.
const VERB: &str = "a merge or an enqueue goes through makibad: call mcp__makiba__merge with repo, pr and head (the full head of the CI notice); a merge it cannot run (under merge_approval_mode human, or with the gate's warning) the person does in the GitHub web UI";

/// The line this hook holds.
const LINE: &str = "L-K9";

pub fn check(payload: &Value) -> Result<(), super::Refusal> {
    if super::field(payload, "/tool_name") != "Bash" {
        return Ok(());
    }
    let command = super::command(payload);
    parse(&command).map_err(|why| super::Refusal::new(LINE, command.trim(), why).at("gh pr merge"))
}

/// `Ok` for a command that cannot land or enqueue; the reason for one the hook refuses.
pub fn parse(command: &str) -> Result<(), String> {
    match refused(command) {
        Some(why) => Err(format!("{why}; {VERB}")),
        None => Ok(()),
    }
}

/// True when `s` has the word merge on its own (not inside a longer name such as
/// `merge_approval_mode`, `mergedAt` or `before-merge.sh`), a GraphQL merge or enqueue
/// mutation's name, or a `pulls/<n>/merge*` path (`merge-async` too, which the word alone misses).
fn merge_word(s: &str) -> bool {
    let s = s.to_lowercase();
    if regex::Regex::new(r"pulls/[^/]*/merge")
        .expect("pattern")
        .is_match(&s)
    {
        return true;
    }
    if [
        "mergepullrequest",
        "automerge",
        "mergebranch",
        "enqueuepullrequest",
    ]
    .iter()
    .any(|m| s.contains(m))
    {
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

/// Wrappers that run their arguments as a command; the hook reads the command after them. Each
/// with the options that take a value.
const WRAPPERS: &[(&str, &[&str])] = &[
    ("env", &["-u", "--unset", "-C", "--chdir"]),
    ("command", &[]),
    ("builtin", &[]),
    ("exec", &["-a"]),
    ("nohup", &[]),
    ("time", &["-f", "--format", "-o", "--output"]),
    ("nice", &["-n", "--adjustment"]),
    ("timeout", &["-k", "--kill-after", "-s", "--signal"]),
    (
        "sudo",
        &["-u", "-g", "-h", "-p", "-C", "-D", "-r", "-t", "-U"],
    ),
    ("doas", &["-u", "-C"]),
    ("stdbuf", &["-i", "-o", "-e"]),
    ("setsid", &[]),
    // Shell keywords before a command.
    ("then", &[]),
    ("do", &[]),
    ("else", &[]),
    ("elif", &[]),
    ("if", &[]),
    ("while", &[]),
    ("until", &[]),
    ("!", &[]),
];
/// Programs that run code or a program given in their arguments.
const RUNNERS: &[&str] = &[
    "sh", "bash", "zsh", "dash", "ksh", "fish", "python", "python3", "py", "node", "perl", "ruby",
    "php", "lua", "sed", "awk", "gawk", "less", "eval", "source", ".", "watch", "script", "make",
    "just", "npx", "uvx", "flock", "ionice", "chrt", "taskset", "strace", "unbuffer",
];
/// Programs that only read or print text: `gh` among their words is a mention.
const TEXT: &[&str] = &[
    "echo", "printf", "grep", "egrep", "fgrep", "zgrep", "rg", "cat", "head", "tail", "wc", "ls",
    "diff", "tee", "sed", "git", "jq", "sort", "uniq", "cut", "tr", "less", "more", "true",
    "false", "test", "[", "cd", "pwd", "mkdir", "touch", "cp", "mv", "rm", "ln", "chmod", "date",
    "sleep", "which", "type", "basename", "dirname", "realpath", "readlink", "file", "stat",
];
/// Programs that run another program with words they read from input or files.
const FEEDERS: &[&str] = &["xargs", "find", "parallel"];

/// The program's own name: the last part of a path, without `.exe`, lowercased.
fn program(word: &str) -> String {
    let lower = word.to_lowercase();
    let base = lower.rsplit(['/', '\\']).next().unwrap_or(&lower);
    base.strip_suffix(".exe").unwrap_or(base).to_string()
}

/// A word as the shell will pass it, quotes removed, and whether the hook reads it with
/// certainty: plain characters, single quotes, double quotes holding no `$`, backquote,
/// backslash or `!`. A backslash escape, a glob, an expansion, a substitution or an operator glued
/// to the word makes it uncertain.
#[derive(Debug, Clone)]
struct Tok {
    text: String,
    certain: bool,
    /// `NAME=value` before any quote.
    assignment: bool,
    /// A redirection: `Some(true)` when the operator stands alone (its target is the next word).
    redir: Option<bool>,
}

fn plain(c: char) -> bool {
    c.is_ascii_alphanumeric() || "_./:=,@%+~-".contains(c)
}

/// The words of one segment, as the shell splits them. `(`, `)`, `{` and `}` outside quotes are
/// grouping and separate words; a `#` that starts a word starts a comment.
fn tokens(segment: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut chars = segment.chars().peekable();
    let new = || Tok {
        text: String::new(),
        certain: true,
        assignment: false,
        redir: None,
    };
    let mut t = new();
    let mut started = false;
    let mut quoted = false;
    let flush = |t: &mut Tok, started: &mut bool, quoted: &mut bool, out: &mut Vec<Tok>| {
        if *started {
            out.push(std::mem::replace(
                t,
                Tok {
                    text: String::new(),
                    certain: true,
                    assignment: false,
                    redir: None,
                },
            ));
        }
        *started = false;
        *quoted = false;
    };
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'\n') => {
                chars.next();
            }
            c if c.is_whitespace() || "(){}".contains(c) => {
                flush(&mut t, &mut started, &mut quoted, &mut out)
            }
            '#' if !started => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            '\'' | '"' => {
                started = true;
                quoted = true;
                let mut closed = false;
                while let Some(q) = chars.next() {
                    if q == c {
                        closed = true;
                        break;
                    }
                    if c == '"' && "$`\\!".contains(q) {
                        t.certain = false;
                    }
                    if c == '"' && q == '\\' {
                        if let Some(n) = chars.next() {
                            t.text.push(n);
                        }
                        continue;
                    }
                    t.text.push(q);
                }
                t.certain &= closed;
            }
            '\\' => {
                started = true;
                t.certain = false;
                if let Some(n) = chars.next() {
                    t.text.push(n);
                }
            }
            '<' | '>' => {
                let lead = !quoted && t.text.chars().all(|c| c.is_ascii_digit() || c == '&');
                if lead && t.redir.is_none() {
                    started = true;
                    t.redir = Some(true);
                    t.text.push(c);
                    while let Some(&n) = chars.peek() {
                        if !"<>&|".contains(n) {
                            break;
                        }
                        t.text.push(n);
                        chars.next();
                    }
                    let alone = chars.peek().is_none_or(|n| n.is_whitespace());
                    t.redir = Some(alone);
                    if alone {
                        flush(&mut t, &mut started, &mut quoted, &mut out);
                    } else {
                        // The target follows glued; it is read on, as the rest of this word.
                        t.text.clear();
                    }
                } else {
                    // An operator glued to a word: the shell splits there, the hook does not.
                    started = true;
                    t.certain = false;
                    t.text.push(c);
                }
            }
            '=' if !quoted
                && !t.assignment
                && t.redir.is_none()
                && t.text
                    .chars()
                    .next()
                    .is_some_and(|f| f.is_ascii_alphabetic() || f == '_')
                && t.text
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_') =>
            {
                started = true;
                t.assignment = true;
                t.text.push(c);
            }
            c => {
                started = true;
                if !plain(c) {
                    t.certain = false;
                }
                t.text.push(c);
            }
        }
    }
    flush(&mut t, &mut started, &mut quoted, &mut out);
    out
}

/// The words without redirections and their targets. `false` when a redirection's target cannot
/// be read with certainty.
fn without_redirections(toks: Vec<Tok>) -> (Vec<Tok>, bool) {
    let (mut words, mut certain, mut target) = (Vec::new(), true, false);
    for t in toks {
        if std::mem::take(&mut target) {
            certain &= t.certain;
            continue;
        }
        match t.redir {
            Some(alone) => {
                target = alone;
                certain &= alone || t.certain;
            }
            None => words.push(t),
        }
    }
    (words, certain)
}

/// The index of the command word: assignments and wrappers (with their options) skipped. `None`
/// when a word before it is uncertain or a wrapper's options cannot be read (`env -S`).
fn command_index(words: &[Tok]) -> Option<usize> {
    let mut i = 0;
    while i < words.len() {
        let w = &words[i];
        if w.assignment {
            i += 1;
            continue;
        }
        if !w.certain {
            return (i == 0 || words[..i].iter().all(|w| w.assignment)).then_some(i);
        }
        let Some((name, takes)) = WRAPPERS.iter().find(|(n, _)| *n == program(&w.text)) else {
            return Some(i);
        };
        i += 1;
        while i < words.len() && (words[i].text.starts_with('-') || words[i].assignment) {
            let o = words[i].text.as_str();
            if !words[i].certain
                || (*name == "env" && (o.starts_with("-S") || o.starts_with("--split-string")))
            {
                return None;
            }
            // `command -v` only looks a name up.
            if *name == "command" && (o == "-v" || o == "-V") {
                return Some(words.len());
            }
            i += if takes.contains(&o) { 2 } else { 1 };
        }
        // `timeout <duration>`.
        if *name == "timeout" && i < words.len() {
            i += 1;
        }
    }
    Some(i)
}

/// The command without the bodies of here-documents fed to a text program (`cat > body.md <<'EOF'`):
/// such a body is data. A body fed to anything else (`bash <<EOF`) is kept and read as commands;
/// a substitution in a body is caught by the rule on substitutions before this.
fn without_text_heredocs(command: &str) -> String {
    let start =
        regex::Regex::new(r#"<<-?\s*\\?['"]?([A-Za-z_][A-Za-z0-9_]*)['"]?"#).expect("pattern");
    let mut out = Vec::new();
    let mut lines = command.split('\n');
    while let Some(line) = lines.next() {
        out.push(line);
        // Not in a comment, and fed to the last command of the line before the operator.
        let Some(m) = start
            .captures(line)
            .filter(|m| !line[..m.get(0).unwrap().start()].contains('#'))
        else {
            continue;
        };
        let before = &line[..m.get(0).unwrap().start()];
        let last = before.rsplit(['|', ';', '&']).next().unwrap_or(before);
        let (words, _) = without_redirections(tokens(last));
        let text = command_index(&words)
            .and_then(|i| words.get(i))
            .is_some_and(|w| {
                w.certain && TEXT.contains(&program(&w.text).as_str()) && program(&w.text) != "sed"
            });
        if !text {
            continue;
        }
        let delimiter = &m[1];
        for body in lines.by_ref() {
            if body.trim_start_matches('\t') == delimiter {
                out.push(body);
                break;
            }
        }
    }
    out.join("\n")
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
    let command = without_text_heredocs(command);
    let (segments, assigns) = segments_and_assignments(&command);
    if mentions
        && (shell::split_segments(&command).1 || shell::unreadable(&segments, &assigns).is_some())
    {
        return Some("the command cannot be read as the shell runs it".into());
    }
    segments
        .iter()
        .filter(|s| !s.trim().is_empty())
        .find_map(|s| refused_segment(s, mentions))
}

/// `shell::segments_and_assignments`, except that a newline inside quotes does not end a segment
/// (Issue 200): a quoted argument of several lines, such as a commit message, stays one argument.
/// A command with a here-document, a command or process substitution, anywhere, is cut at every
/// newline as before: their bodies may span lines inside quotes or land in a later segment.
fn segments_and_assignments(command: &str) -> (Vec<String>, Vec<String>) {
    if ["<<", "$(", "`", "<(", ">("]
        .iter()
        .any(|c| command.contains(c))
    {
        return shell::segments_and_assignments(command);
    }
    let mut segs: Vec<String> = shell::split_segments(command)
        .0
        .iter()
        .flat_map(|s| lines(s))
        .collect();
    // The bodies of `eval` and `bash -c` are code: cut at every newline, as before.
    let bodies = shell::unwrap(&segs);
    segs.extend(
        bodies
            .iter()
            .flat_map(|b| b.split('\n'))
            .filter(|l| !l.is_empty())
            .map(String::from),
    );
    let assigns = shell::collect_assignments(&segs)
        .into_iter()
        .filter(|a| !a.is_empty())
        .collect();
    (segs, assigns)
}

/// `segment` cut at the newlines outside quotes (`'...'`, `"..."`, `$'...'`) and outside
/// comments. A segment whose quote does not close is cut at every newline, as before.
fn lines(segment: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    // The quote open: `'`, `"`, or `$` for `$'...'`.
    let mut quote: Option<char> = None;
    let mut word_start = true;
    let mut chars = segment.chars().peekable();
    while let Some(c) = chars.next() {
        let cur = out.last_mut().expect("a line");
        cur.push(c);
        if let Some(q) = quote {
            if c == '\\' && q != '\'' {
                cur.extend(chars.next());
            } else if c == if q == '"' { '"' } else { '\'' } {
                quote = None;
            }
            continue;
        }
        match c {
            '\n' => {
                cur.pop();
                out.push(String::new());
            }
            '#' if word_start => {
                while let Some(n) = chars.next_if(|n| *n != '\n') {
                    cur.push(n);
                }
            }
            '\\' => cur.extend(chars.next()),
            '\'' | '"' => quote = Some(c),
            '$' if chars.peek() == Some(&'\'') => {
                cur.extend(chars.next());
                quote = Some('$');
            }
            _ => {}
        }
        word_start = c.is_whitespace() || ";&|()".contains(c);
    }
    if quote.is_some() {
        return segment.split('\n').map(String::from).collect();
    }
    out
}

const PLAINLY: &str = "write the command plainly: plain words or quotes without $, backquotes or backslashes, a redirection as a word of its own";

/// One segment: why it is refused, if it is.
fn refused_segment(segment: &str, mentions: bool) -> Option<String> {
    let (words, redirections_certain) = without_redirections(tokens(segment));
    let Some(at) = command_index(&words) else {
        return Some(format!("a wrapper's options cannot be read; {PLAINLY}"));
    };
    let first = words.get(at)?;
    if !first.certain {
        return Some(format!(
            "the program {:?} cannot be read; {PLAINLY}",
            first.text
        ));
    }
    let p = program(&first.text);
    let args = &words[at + 1..];
    let gh_word = |w: &Tok| program(&w.text) == "gh";
    let code: Vec<&str> = args.iter().map(|w| w.text.as_str()).collect();
    let code = code.join(" ");
    match p.as_str() {
        "gh" => refused_gh(args, redirections_certain),
        "curl" | "wget" | "http" | "https" | "xh" => {
            if !redirections_certain || args.iter().any(|w| !w.certain) {
                return Some(format!("{p} with words the hook cannot read; {PLAINLY}"));
            }
            code.to_lowercase()
                .contains("api.github.com")
                .then(|| format!("{p} to the GitHub API"))
        }
        _ if FEEDERS.contains(&p.as_str()) => (args.iter().any(gh_word)
            || (mentions && args.iter().any(|w| !w.certain)))
        .then(|| format!("{p} runs a program with words the hook cannot see")),
        // ripgrep runs a program only with --pre; its pattern is a mention.
        "rg" => (mentions
            && args
                .iter()
                .any(|w| w.text == "--pre" || w.text.starts_with("--pre=")))
        .then(|| "rg --pre runs a program, with the word merge".to_string()),
        // A git alias may run a shell command.
        "git" => (mentions && args.iter().any(|w| w.text.contains("alias.")))
            .then(|| "a git alias with the word merge".to_string()),
        _ => {
            if RUNNERS.contains(&p.as_str()) {
                let gh_in_code =
                    regex::Regex::new(r"(?i)(^|[^a-z0-9_.-])gh(\.exe)?([^a-z0-9_-]|$)")
                        .expect("pattern");
                let runs = gh_in_code.is_match(&code) || segment.contains(['$', '`']);
                if mentions && runs {
                    return Some(format!("{p} runs code that names gh and merge"));
                }
            }
            // A program that may run its arguments: gh among them is not a mention.
            (!TEXT.contains(&p.as_str()) && args.iter().any(gh_word))
                .then(|| format!("{p} may run gh; run gh as the command itself"))
        }
    }
}

/// A `gh` call (its words after `gh`): why it is refused, if it is. Up to and including the
/// subcommand every word must be read with certainty; for `gh api`, every word.
fn refused_gh(args: &[Tok], redirections_certain: bool) -> Option<String> {
    let unsure = |w: &Tok| format!("gh's word {:?} cannot be read; {PLAINLY}", w.text);
    // Global flags; -R / --repo / --hostname take a value.
    let mut i = 0;
    while i < args.len() && args[i].text.starts_with('-') {
        let takes = matches!(args[i].text.as_str(), "-R" | "--repo" | "--hostname");
        for w in &args[i..(i + if takes { 2 } else { 1 }).min(args.len())] {
            if !w.certain {
                return Some(unsure(w));
            }
        }
        i += if takes { 2 } else { 1 };
    }
    let group = args.get(i)?;
    if !group.certain {
        return Some(unsure(group));
    }
    let g = group.text.to_lowercase();
    if !deny_push::GROUPS.contains(&g.as_str()) {
        return Some(format!(
            "gh {:?} is not a gh command (an alias or an extension cannot be read); run the command it stands for",
            group.text
        ));
    }
    let rest = &args[i + 1..];
    if g == "api" {
        if !redirections_certain {
            return Some(format!(
                "gh api with a redirection the hook cannot read; {PLAINLY}"
            ));
        }
        if let Some(w) = rest.iter().find(|w| !w.certain) {
            return Some(unsure(w));
        }
        let mut words = vec!["gh", "api"];
        words.extend(rest.iter().map(|w| w.text.as_str()));
        let line = words.join(" ");
        let verdicts = deny_push::classify_api(&words, 1, &line);
        let endpoint = deny_push::next_word(&words, 2).map(|e| words[e].to_lowercase());
        if endpoint.as_deref() == Some("graphql") {
            if verdicts.iter().any(|v| v.contains("undecidable")) {
                return Some("gh api graphql with a body the hook cannot read".into());
            }
            return merge_word(&line)
                .then(|| "gh api graphql with a merge mutation (an enqueue lands too)".into());
        }
        let path = endpoint.unwrap_or_default();
        let path = path.split('?').next().unwrap_or_default();
        // `merge`, `merge-async`, and any later sibling whose name starts with merge.
        let merge_path = regex::Regex::new(r"(^|/)repos/[^/]+/[^/]+/pulls/[^/]+/merge[^/]*/?$")
            .expect("pattern");
        return (merge_path.is_match(path) && !verdicts.is_empty())
            .then(|| format!("gh api writing to {path}"));
    }
    // The subcommand: the first word after the group that is not an option or its value.
    let mut j = 0;
    while j < rest.len() && rest[j].text.starts_with('-') {
        let n = if deny_push::takes_value(&rest[j].text) {
            2
        } else {
            1
        };
        for w in &rest[j..(j + n).min(rest.len())] {
            if !w.certain {
                return Some(unsure(w));
            }
        }
        j += n;
    }
    let sub = rest.get(j)?;
    if !sub.certain {
        return Some(unsure(sub));
    }
    match (g.as_str(), sub.text.to_lowercase().as_str()) {
        ("pr", "merge") => Some("gh pr merge lands nothing from the command line".into()),
        ("alias", "set" | "import") => Some("gh alias set or import can define a merge".into()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::parse;

    /// The word, kept out of this file's own command lines.
    const M: &str = "mer\x67e";

    /// The Issue 433 diff review: a write to a `pulls/<n>/merge*` path that a runner or a
    /// substitution wraps is read as naming merge, as the plain word is.
    #[test]
    fn a_wrapped_write_to_a_merge_path_is_refused() {
        for cmd in [
            format!(
                "python3 -c 'import subprocess; subprocess.run([\"gh\",\"api\",\"-X\",\"POST\",\"repos/o/r/pulls/7/{M}-async\"])'"
            ),
            format!(
                "python3 -c 'import os; os.system(\"gh api -X PUT repos/o/r/pulls/7/{M}-queue\")'"
            ),
            format!(
                "perl -e 'system(\"gh\", \"api\", \"-X\", \"PUT\", \"repos/o/r/pulls/7/{M}-async\")'"
            ),
            format!("gh api -X PUT $(echo repos/o/r/pulls/7/{M}-async)"),
            format!("echo repos/o/r/pulls/7/{M}-async | xargs gh api -X PUT"),
        ] {
            let why = parse(&cmd).expect_err(&cmd);
            assert!(why.contains("mcp__makiba__merge"), "{cmd}: {why}");
        }
        // A mention, and a read, still pass.
        for cmd in [
            format!("grep -rn 'pulls/[0-9]*/{M}-async' src"),
            format!("gh api repos/o/r/pulls/7/{M}-async"),
        ] {
            assert_eq!(parse(&cmd), Ok(()), "{cmd}");
        }
    }
}
