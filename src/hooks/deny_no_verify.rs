//! `makiba hook deny-no-verify` (M4-3): a `PreToolUse` hook on Bash that refuses a git command
//! that skips the repository's hooks (inventory HK-2, lines L-K3 and L-K12). Ported from
//! claude-org-ja's `.hooks/block-no-verify.sh`:
//!
//! - `--no-verify` on `commit`, `push`, `merge`, `pull` or `am`, or an abbreviation git accepts
//!   (`--no-veri`);
//! - `-n` on `commit`, alone or inside a bundle of short options;
//! - `HUSKY=`, `SKIP_SECRET_SCAN=` or `NO_VERIFY=` beside one of those commands;
//! - `git -c core.hooksPath=...` or `git -c hook.*` beside one of them.
//!
//! The flag is read from git's arguments, not from the whole text (Issue 300): a commit message,
//! a quoted pattern or a here-document fed to a text program that holds the flag's letters is not
//! the flag. The command is read as the shell splits it (`words`): quotes keep a word whole,
//! redirections are dropped, an option's value (`-m <message>`) is skipped, and git's word is
//! looked for at any position, so a wrapper (`env`, `sudo`, `xargs`) needs no list. The bodies of
//! command substitutions, of `eval`, `bash -c`, here-documents fed to a shell, and the values of
//! git aliases defined in the command are read again as commands. A here-document fed to a text
//! program is data, unless its output goes on to a shell; one fed to another program (`python`)
//! is read as text, as before. What the reading cannot follow leans to refusing.
//!
//! The variable and `-c` checks still read the text (`shell`).

use super::Refusal;
use super::deny_dangerous_git::has_git_subcmd;
use super::re::Re;
use super::shell;
use super::words::{self, Cmd, Vars, Word};
use serde_json::Value;

/// The line this hook holds (and L-K12, the scanner it keeps from being skipped).
const LINE: &str = "L-K3";

const SKIPS: &str = "it skips the repository's hooks (the secret scanner among them, line L-K12)";

/// How deep substitutions, shell bodies and aliases are read before the hook gives up.
const DEPTH: usize = 8;

fn has(text: &str, pattern: &str) -> bool {
    Re::new(pattern).any_line(text)
}

fn icase(text: &str, pattern: &str) -> bool {
    Re::icase(pattern).any_line(text)
}

fn refuse(matched: impl Into<String>, reason: impl Into<String>) -> Result<(), Refusal> {
    Err(Refusal::new(LINE, matched, reason))
}

pub fn check(payload: &Value) -> Result<(), Refusal> {
    let command = super::command(payload);
    if command.is_empty() {
        return Ok(());
    }
    let (segments, assigns) = shell::segments_and_assignments(&command);
    if let Some((matched, why)) = shell::unreadable_at(&segments, &assigns) {
        return refuse(matched, why);
    }
    read(&command, Ctx::Data, 0)?;
    let mut whole = shell::plain_git(&shell::flatten_text(&command));
    if !assigns.is_empty() {
        whole = shell::expand(&whole, &assigns);
    }
    let any_sub = r"(commit|push|merge|pull|am)";
    if has(
        &whole,
        &format!(r"(^|[[:space:]])git[[:space:]]+{any_sub}([[:space:]]|$)"),
    ) || has(
        &whole,
        &format!(r"(^|[[:space:]])git[[:space:]].*[[:space:]]{any_sub}([[:space:]]|$)"),
    ) {
        for var in ["HUSKY", "SKIP_SECRET_SCAN", "NO_VERIFY"] {
            if has(&whole, &format!(r"(^|[[:space:]]){var}=")) {
                return refuse(
                    format!("{var}="),
                    format!(
                        "{var}=... switches the repository's hooks off; run the command without it"
                    ),
                );
            }
        }
    }
    if (has(&whole, r"(^|[[:space:]])git[[:space:]]+-c")
        || has(&whole, r"(^|[[:space:]])git[[:space:]].*[[:space:]]-c"))
        && has(&whole, &format!(r"(^|[[:space:]]){any_sub}([[:space:]]|$)"))
    {
        if icase(&whole, r"(^|[[:space:]])-c[[:space:]]*core\.hookspath=") {
            return refuse(
                "-c core.hooksPath=",
                "git -c core.hooksPath=... switches the repository's hooks off",
            );
        }
        if icase(&whole, r"(^|[[:space:]])-c[[:space:]]*hook\.") {
            return refuse("-c hook.", "git -c hook.* overrides the repository's hooks");
        }
    }
    Ok(())
}

/// Where the output of the commands being read goes, which decides what a here-document fed to
/// a text program is: data (to the terminal, a file, a text program or git), shell code, or
/// text of another language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ctx {
    Data,
    Shell,
    Other,
}

/// Programs that only read their input as data.
const TEXT: &[&str] = &[
    "cat", "tee", "head", "tail", "wc", "grep", "egrep", "fgrep", "zgrep", "rg", "sort", "uniq",
    "cut", "tr", "jq", "diff", "less", "more", "true", "false", "echo", "printf", "git", "gh",
    "base64", "fold", "fmt", "nl", "column", "rev", "tac",
];

/// Programs whose input or arguments are shell code.
const SHELLS: &[&str] = &[
    "bash", "sh", "zsh", "dash", "ksh", "mksh", "ash", "busybox", "eval", "source", ".",
];

/// Words that run the next word as the command.
const WRAPPERS: &[&str] = &[
    "env", "sudo", "doas", "command", "builtin", "exec", "nohup", "nice", "ionice", "time",
    "stdbuf", "setsid", "timeout", "!", "{", "if", "then", "else", "elif", "do", "while", "until",
];

/// The program's own name: the last part of a path, without `.exe`, lowercased.
fn program(word: &str) -> String {
    let lower = word.to_lowercase();
    let base = lower.rsplit(['/', '\\']).next().unwrap_or(&lower);
    base.strip_suffix(".exe").unwrap_or(base).to_string()
}

/// The program a command runs: its first word past assignments and wrappers (and their options).
fn command_program(words: &[Word]) -> Option<String> {
    let mut i = 0;
    while i < words.len() && words::assignment(&words[i].text).is_some() {
        i += 1;
    }
    while let Some(w) = words.get(i) {
        let p = program(&w.text);
        if !WRAPPERS.contains(&p.as_str()) {
            return Some(p);
        }
        i += 1;
        // A wrapper's options, its assignments (`env A=1`) and timeout's duration.
        while words
            .get(i)
            .is_some_and(|w| w.text.starts_with('-') || words::assignment(&w.text).is_some())
        {
            i += 1;
        }
        if p == "timeout" && i < words.len() {
            i += 1;
        }
    }
    None
}

fn ctx_of(program: Option<&str>) -> Ctx {
    match program {
        None => Ctx::Data,
        Some(p) if SHELLS.contains(&p) => Ctx::Shell,
        Some(p) if TEXT.contains(&p) => Ctx::Data,
        Some(_) => Ctx::Other,
    }
}

/// Read `text` as shell commands whose output goes to `out`.
fn read(text: &str, out: Ctx, depth: usize) -> Result<(), Refusal> {
    if depth > DEPTH {
        return refuse(
            text,
            "the command nests substitutions, shells or aliases too deeply to read; write it plainly",
        );
    }
    let first = words::parse(text, &Vars::new());
    let mut vars = Vars::new();
    for (name, value) in words::assignments(&first) {
        if name == "IFS" {
            return refuse(
                format!("IFS={value}"),
                "IFS changes how the shell splits words, so the hook cannot read them; leave it as it is",
            );
        }
        if vars.insert(name.clone(), value).is_some() {
            return refuse(
                format!("{name}= twice"),
                format!(
                    "{name} is assigned more than once, so the hooks cannot tell its value where it is used; use one name per value"
                ),
            );
        }
    }
    let parsed = words::parse(text, &vars);
    if parsed.lost {
        // A quote, a substitution or a here-document that does not close: read as text.
        return legacy(text);
    }
    let progs: Vec<Option<String>> = parsed
        .cmds
        .iter()
        .map(|c| command_program(&c.words))
        .collect();
    for (k, cmd) in parsed.cmds.iter().enumerate() {
        let prog = progs[k].as_deref();
        let own = ctx_of(prog);
        check_git(cmd, depth)?;
        if own == Ctx::Shell {
            for body in shell_bodies(cmd, prog.unwrap_or_default()) {
                read(&body, Ctx::Shell, depth + 1)?;
            }
        }
        // A substitution runs; its output becomes a word of this command.
        for body in &cmd.substs {
            read(body, own, depth + 1)?;
        }
        // Where a here-document fed to a text program goes: through the pipe to the next
        // command, or where this command's own output goes.
        let onward = if cmd.piped {
            ctx_of(progs.get(k + 1).and_then(|p| p.as_deref()))
        } else {
            out
        };
        for doc in &cmd.heredocs {
            let goes = if own == Ctx::Data { onward } else { own };
            match goes {
                Ctx::Shell => read(&doc.body, Ctx::Shell, depth + 1)?,
                Ctx::Other => legacy(&doc.body)?,
                Ctx::Data if doc.quoted => {}
                Ctx::Data => match words::substitutions(&doc.body) {
                    Some(bodies) => {
                        for b in bodies {
                            read(&b, Ctx::Data, depth + 1)?;
                        }
                    }
                    None => legacy(&doc.body)?,
                },
            }
        }
    }
    Ok(())
}

/// The shell code a shell program of `cmd` runs from its words: `eval`'s words, `source`'s are a
/// file (none), and the word after `-c` (or a bundle holding `c`) of a shell.
fn shell_bodies(cmd: &Cmd, prog: &str) -> Vec<String> {
    let words: Vec<&str> = cmd.words.iter().map(|w| w.text.as_str()).collect();
    let Some(at) = words.iter().position(|w| program(w) == prog) else {
        return Vec::new();
    };
    if prog == "eval" {
        return vec![words[at + 1..].join(" ")];
    }
    let mut out = Vec::new();
    let mut i = at + 1;
    while let Some(w) = words.get(i) {
        if w.starts_with('-') && !w.starts_with("--") && w.contains('c') {
            if let Some(body) = words.get(i + 1) {
                out.push(body.to_string());
            }
            break;
        }
        if !w.starts_with('-') {
            break;
        }
        i += 1;
    }
    out
}

/// Each git of `cmd` (at any position): its subcommand's options, and the aliases it defines.
fn check_git(cmd: &Cmd, depth: usize) -> Result<(), Refusal> {
    let words = &cmd.words;
    for (i, w) in words.iter().enumerate() {
        if program(&w.text) != "git" {
            continue;
        }
        let mut j = i + 1;
        while let Some(o) = words.get(j).filter(|o| o.text.starts_with('-')) {
            let o = &o.text;
            if shell::git_option_takes_value(o) {
                if o == "-c"
                    && let Some(v) = words.get(j + 1)
                {
                    alias(&v.text, depth)?;
                }
                j += 2;
            } else {
                if let Some(v) = o.strip_prefix("-c") {
                    alias(v, depth)?;
                }
                j += 1;
            }
        }
        let Some(sub) = words.get(j) else { continue };
        let args = &words[j + 1..];
        match sub.text.as_str() {
            "commit" => options(args, "commit", COMMIT_SHORT, "Su", COMMIT_LONG)?,
            "push" => options(args, "push", PUSH_SHORT, "", PUSH_LONG)?,
            "merge" => options(args, "merge", MERGE_SHORT, "S", MERGE_LONG)?,
            "pull" => options(args, "pull", PULL_SHORT, "S", PULL_LONG)?,
            "am" => options(args, "am", AM_SHORT, "SCp", AM_LONG)?,
            "config" => {
                // `git config [--global ...] alias.<name> <value>`: the value is read as git's.
                if let Some(k) = args
                    .iter()
                    .position(|a| a.text.to_lowercase().starts_with("alias."))
                    && let Some(v) = args.get(k + 1)
                {
                    alias(&format!("{}={}", args[k].text, v.text), depth)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// A configuration `key=value` set in the command: an alias's value is read as git's arguments,
/// or with a leading `!` as a shell command.
fn alias(setting: &str, depth: usize) -> Result<(), Refusal> {
    let Some((key, value)) = setting.split_once('=') else {
        return Ok(());
    };
    if !key.to_lowercase().starts_with("alias.") {
        return Ok(());
    }
    match value.trim_start().strip_prefix('!') {
        Some(code) => read(code, Ctx::Shell, depth + 1),
        None => read(&format!("git {value}"), Ctx::Data, depth + 1),
    }
}

/// Short options (in a bundle) that take a value: the rest of the bundle, or the next word.
const COMMIT_SHORT: &str = "mFCct";
/// Long options that take a value, unless written with `=`.
const COMMIT_LONG: &[&str] = &[
    "message",
    "file",
    "reuse-message",
    "reedit-message",
    "template",
    "author",
    "date",
    "cleanup",
    "fixup",
    "squash",
    "trailer",
    "pathspec-from-file",
];
const PUSH_SHORT: &str = "o";
const PUSH_LONG: &[&str] = &["push-option", "repo", "receive-pack", "exec"];
const MERGE_SHORT: &str = "mFsX";
const MERGE_LONG: &[&str] = &[
    "message",
    "file",
    "strategy",
    "strategy-option",
    "into-name",
    "cleanup",
];
const PULL_SHORT: &str = "sXo";
const PULL_LONG: &[&str] = &[
    "strategy",
    "strategy-option",
    "upload-pack",
    "depth",
    "deepen",
    "shallow-since",
    "shallow-exclude",
    "negotiation-tip",
    "server-option",
];
const AM_SHORT: &str = "";
const AM_LONG: &[&str] = &[
    "patch-format",
    "resolvemsg",
    "exclude",
    "include",
    "directory",
];

/// `--no-verify`, or an abbreviation of it git would accept.
fn no_verify(word: &str) -> bool {
    word.len() >= "--no-v".len() && "--no-verify".starts_with(word)
}

/// The options of `git <sub>` in `args`, up to `--`: refused when one skips the hooks. In a
/// bundle of short options, one of `short_value` takes the rest of the bundle or the next word as
/// its value, and one of `attached` an optional value only in the rest of the bundle.
fn options(
    args: &[Word],
    sub: &str,
    short_value: &str,
    attached: &str,
    long_value: &[&str],
) -> Result<(), Refusal> {
    let mut k = 0;
    while let Some(w) = args.get(k) {
        let t = w.text.as_str();
        k += 1;
        if t == "--" {
            break;
        }
        if w.built && (t.starts_with("$(") || t.starts_with('`')) {
            return refuse(
                format!("{t} in git {sub}"),
                format!(
                    "a word of git {sub} made by a command substitution may be an option the hook cannot read; write the options plainly"
                ),
            );
        }
        if let Some(long) = t.strip_prefix("--") {
            let (name, has_value) = match long.split_once('=') {
                Some((n, _)) => (n, true),
                None => (long, false),
            };
            if no_verify(&format!("--{name}")) {
                return refuse(
                    format!("{t} in git {sub}"),
                    format!(
                        "git {sub} --no-verify skips the repository's hooks; {SKIPS}. Run it without the flag"
                    ),
                );
            }
            // An option that takes a value (git also takes a unique abbreviation of its name).
            if !has_value && !name.is_empty() && long_value.iter().any(|v| v.starts_with(name)) {
                k += 1;
            }
            continue;
        }
        let Some(bundle) = t.strip_prefix('-').filter(|b| !b.is_empty()) else {
            continue;
        };
        for (at, c) in bundle.char_indices() {
            if sub == "commit" && c == 'n' {
                return refuse(
                    format!("{t} in git {sub}"),
                    format!("git commit -n is --no-verify; {SKIPS}. Run it without the flag"),
                );
            }
            if short_value.contains(c) {
                if at + c.len_utf8() == bundle.len() {
                    k += 1;
                }
                break;
            }
            if attached.contains(c) {
                break;
            }
        }
    }
    Ok(())
}

/// The reading before Issue 300, for text whose language the hook does not read (a body fed to
/// a program other than a shell or a text program, a command whose quotes do not close): the
/// flag's spelling anywhere in a segment with git's subcommand.
fn legacy(text: &str) -> Result<(), Refusal> {
    let (segments, assigns) = shell::segments_and_assignments(text);
    if let Some((matched, why)) = shell::unreadable_at(&segments, &assigns) {
        return refuse(matched, why);
    }
    for segment in segments.iter().filter(|s| !s.is_empty()) {
        let flat = shell::flat_segment(segment, &assigns);
        let subs = ["commit", "push", "merge", "pull", "am"].map(|s| has_git_subcmd(&flat, s));
        if !subs.iter().any(|&s| s) {
            continue;
        }
        let reason = "the text, which the hook cannot read as commands, holds git's flag that skips the hooks; write the command plainly, or the text in a file";
        if has(&flat, r"(^|[[:space:]])--no-veri") {
            return refuse(flat.trim(), format!("{reason} ({SKIPS})"));
        }
        if subs[0]
            && (has(&flat, r"(^|[[:space:]])-n([[:space:]]|$)")
                || has(&flat, r"(^|[[:space:]])-[a-zA-Z]*n[a-zA-Z]+([[:space:]]|$)")
                || has(&flat, r"(^|[[:space:]])-[a-zA-Z]+n([[:space:]]|$)"))
        {
            return refuse(flat.trim(), format!("{reason} ({SKIPS})"));
        }
    }
    Ok(())
}
