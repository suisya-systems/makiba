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
//! command substitutions (also inside `${v:-...}` and `$((...))`), of `eval`, `bash -c`,
//! here-documents and here-strings fed to a shell, and the values of git aliases defined in the
//! command are read again as commands, with the variables of the text around them. The arguments
//! of another program (`python3 -c`, `node -e`) and a here-document fed to it are read as text,
//! as before. A here-document fed to a program that keeps it (git, gh) is data; one fed to a
//! text program that passes it on (`cat`) is data unless it reaches a shell: through a pipe after
//! it, through the output of the text it is part of, or, when it writes a file, through a shell
//! run later in the command. A word where git reads options that starts with a substitution or an
//! expansion with an operator is refused, since its value is not known. What the reading cannot
//! follow, or nests too deeply, leans to refusing.
//!
//! The argument reading stands in for the text reading only where nothing in the command runs
//! code (`pure`): every program is a text program or git with a subcommand that runs none, and no
//! word names a shell, `eval`, `source` or an interpreter, in the command or any substitution. In
//! any other command the text is read as before Issue 300 (`legacy`), and the arguments too, so
//! the hook refuses no less than before; there the letters in a message are refused as before.
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
    // The arguments are read in place of the text only where nothing in the command runs code;
    // anywhere else the text is read as before Issue 300, and the arguments as well.
    if !pure(&command, 0) {
        legacy(&command)?;
    }
    read(&command, Ctx::Data, 0, &Vars::new())?;
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

/// Of `TEXT`, the programs that keep the input they are fed (a commit message to git, an
/// Issue's body to gh): none of it goes on to their output, to a pipe or to a file.
const SINKS: &[&str] = &["git", "gh", "wc", "true", "false"];

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

/// The programs a command may run for its argument reading to stand in for the text reading:
/// they print, filter or keep text and run nothing (git is judged by its subcommand, below).
const PURE: &[&str] = &[
    "cat", "tee", "head", "tail", "wc", "grep", "egrep", "fgrep", "zgrep", "rg", "sort", "uniq",
    "cut", "tr", "jq", "diff", "true", "false", "echo", "printf", "git", "gh", "base64", "fold",
    "fmt", "nl", "column", "rev", "tac", "ls", "pwd", "test", "[", "cd", "mkdir", "touch", "cp",
    "mv", "rm", "chmod", "date", "basename", "dirname", "realpath", "readlink", "stat", "file",
];

/// git's subcommands that run no program the command names.
const GIT_PURE: &[&str] = &[
    "commit",
    "push",
    "merge",
    "pull",
    "am",
    "add",
    "status",
    "log",
    "diff",
    "show",
    "fetch",
    "branch",
    "switch",
    "checkout",
    "restore",
    "rm",
    "mv",
    "tag",
    "rev-parse",
    "ls-files",
    "blame",
    "reset",
    "stash",
    "init",
    "remote",
    "describe",
    "shortlog",
    "cherry-pick",
    "revert",
];

/// Whether `text` nests `$(` or `${` deeper than the reading follows: it nests a reader per
/// substitution, so such a text is refused before it is read.
fn too_deep(text: &str) -> bool {
    let mut open = 0usize;
    let mut deepest = 0usize;
    let mut prev = ' ';
    for c in text.chars() {
        match c {
            '(' | '{' if prev == '$' => {
                open += 1;
                deepest = deepest.max(open);
            }
            ')' | '}' => open = open.saturating_sub(1),
            _ => {}
        }
        prev = c;
    }
    deepest > 4 * DEPTH
}

/// Programs that run code they are given, wherever their name stands among the words.
fn runs_code(word: &str) -> bool {
    let p = program(word);
    SHELLS.contains(&p.as_str())
        || p.starts_with("python")
        || [
            "node",
            "nodejs",
            "deno",
            "bun",
            "perl",
            "ruby",
            "php",
            "lua",
            "awk",
            "gawk",
            "mawk",
            "nawk",
            "sed",
            "tclsh",
            "rscript",
            "osascript",
            "pwsh",
            "powershell",
            "xargs",
            "parallel",
            "find",
            "env",
            "sudo",
            "doas",
            "exec",
            "command",
            "builtin",
            "nohup",
            "time",
            "timeout",
            "nice",
            "ionice",
            "stdbuf",
            "setsid",
            "watch",
            "script",
        ]
        .contains(&p.as_str())
}

/// Whether nothing in `text` runs code: every command's program is one of `PURE` (or none, an
/// assignment alone); no word anywhere names a shell, `eval`, `source` or an interpreter; git runs
/// one of `GIT_PURE` with no `-c` or alias; gh defines no alias or extension; and the same holds
/// for every substitution, also in an expanding here-document. A reading that gets lost is not.
fn pure(text: &str, depth: usize) -> bool {
    if depth > DEPTH || too_deep(text) {
        return false;
    }
    let parsed = words::parse(text, &Vars::new());
    if parsed.lost {
        return false;
    }
    for cmd in &parsed.cmds {
        let words: Vec<&str> = cmd.words.iter().map(|w| w.text.as_str()).collect();
        if words.iter().any(|w| runs_code(w)) {
            return false;
        }
        let start = words
            .iter()
            .position(|w| words::assignment(w).is_none())
            .unwrap_or(words.len());
        if let Some(first) = words.get(start) {
            let p = program(first);
            if !PURE.contains(&p.as_str()) {
                return false;
            }
            let rest = &words[start + 1..];
            let ok = match p.as_str() {
                "git" => {
                    let sub = rest.iter().find(|w| !w.starts_with('-'));
                    !rest.iter().any(|w| {
                        *w == "-c" || w.starts_with("-c") && w.len() > 2 && !w.starts_with("--")
                    }) && !rest
                        .iter()
                        .any(|w| w.starts_with("--config-env") || w.starts_with("--exec-path"))
                        && sub.is_some_and(|s| GIT_PURE.contains(s))
                }
                "gh" => !rest
                    .iter()
                    .any(|w| matches!(*w, "alias" | "extension" | "ext")),
                "rg" => !rest.iter().any(|w| w.starts_with("--pre")),
                "sort" => !rest.iter().any(|w| w.starts_with("--compress")),
                _ => true,
            };
            if !ok {
                return false;
            }
        }
        if !cmd.substs.iter().all(|b| pure(b, depth + 1)) {
            return false;
        }
        for doc in cmd.heredocs.iter().filter(|d| !d.quoted) {
            match words::substitutions(&doc.body) {
                Some(bodies) if bodies.iter().all(|b| pure(b, depth + 1)) => {}
                _ => return false,
            }
        }
    }
    true
}

/// Shell and other code a command's output or input may reach: (a shell, another program).
type Reach = (bool, bool);

fn reach_of(c: Ctx) -> Reach {
    (c == Ctx::Shell, c == Ctx::Other)
}

fn either(a: Reach, b: Reach) -> Reach {
    (a.0 || b.0, a.1 || b.1)
}

/// Read `text` as shell commands whose output goes to `out`, with the variables `inherited`
/// from the text it is part of (a substitution and a shell's body see them).
fn read(text: &str, out: Ctx, depth: usize, inherited: &Vars) -> Result<(), Refusal> {
    if depth > DEPTH || too_deep(text) {
        return refuse(
            text,
            "the command nests substitutions, shells or aliases too deeply to read; write it plainly",
        );
    }
    let first = words::parse(text, inherited);
    let mut vars = inherited.clone();
    let mut own = std::collections::BTreeSet::new();
    for (name, value) in words::assignments(&first) {
        if name == "IFS" {
            return refuse(
                format!("IFS={value}"),
                "IFS changes how the shell splits words, so the hook cannot read them; leave it as it is",
            );
        }
        vars.insert(name.clone(), value);
        if !own.insert(name.clone()) {
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
    // Counted once, from the end, for each command `k`: what the commands from `k` on reach
    // (`all`), and what a command after `k` reaches through a pipe that starts at `k` or later
    // (`piped`; a group piped on, `{ cat; } | sh`, is a pipe after the command).
    let n = progs.len();
    let mut all = vec![(false, false); n + 1];
    let mut piped = vec![(false, false); n + 1];
    for k in (0..n).rev() {
        all[k] = either(all[k + 1], reach_of(ctx_of(progs[k].as_deref())));
        piped[k] = if parsed.cmds[k].piped {
            all[k + 1]
        } else {
            piped[k + 1]
        };
    }
    for (k, cmd) in parsed.cmds.iter().enumerate() {
        let prog = progs[k].as_deref();
        let own = ctx_of(prog);
        check_git(cmd, depth, &vars)?;
        match own {
            Ctx::Shell => {
                for body in shell_bodies(cmd, prog.unwrap_or_default()) {
                    read(&body, Ctx::Shell, depth + 1, &vars)?;
                }
            }
            // Another program's arguments may be code (`python3 -c '...'`): read as text, as
            // before Issue 300.
            Ctx::Other => {
                for w in cmd.words.iter().skip(1) {
                    legacy(&w.text)?;
                }
            }
            Ctx::Data => {}
        }
        // A substitution runs; its output becomes a word of this command, or, with no program
        // (`$(...)` alone, or an assignment's value kept for later), may be run as a command.
        let subst_out = if prog.is_none() { Ctx::Shell } else { own };
        for body in &cmd.substs {
            read(body, subst_out, depth + 1, &vars)?;
        }
        // Where a here-document (or here-string) fed to a program goes. A shell runs it and
        // another program reads it as its code. A text program that keeps it (git, gh) ends it.
        // One that passes it on sends it where this text's output goes, through a pipe after it
        // to the commands there, and, when it writes a file, to any later command (a file
        // written now and run after).
        let sink = prog.is_some_and(|p| SINKS.contains(&p));
        let (to_shell, to_other) = match own {
            Ctx::Data if sink => (false, false),
            Ctx::Data => {
                let mut r = either(reach_of(out), piped[k]);
                if cmd.writes {
                    r = either(r, all[k + 1]);
                }
                r
            }
            _ => reach_of(own),
        };
        for doc in &cmd.heredocs {
            if to_shell {
                read(&doc.body, Ctx::Shell, depth + 1, &vars)?;
            }
            if to_other {
                legacy(&doc.body)?;
            }
            if doc.quoted {
                continue;
            }
            // An unquoted body expands: its substitutions run.
            match words::substitutions(&doc.body) {
                Some(bodies) => {
                    for b in bodies {
                        read(&b, Ctx::Data, depth + 1, &vars)?;
                    }
                }
                None => legacy(&doc.body)?,
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
        if !w.starts_with(['-', '+']) {
            break;
        }
        // `-o <option>`, `-O <shopt>` and their `+` forms take the next word.
        i += if matches!(*w, "-o" | "+o" | "-O" | "+O") {
            2
        } else {
            1
        };
    }
    out
}

/// Each git of `cmd` (at any position): its subcommand's options, and the aliases it defines.
fn check_git(cmd: &Cmd, depth: usize, vars: &Vars) -> Result<(), Refusal> {
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
                    alias(&v.text, depth, vars)?;
                }
                j += 2;
            } else {
                if let Some(v) = o.strip_prefix("-c") {
                    alias(v, depth, vars)?;
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
                    alias(&format!("{}={}", args[k].text, v.text), depth, vars)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// A configuration `key=value` set in the command: an alias's value is read as git's arguments,
/// or with a leading `!` as a shell command.
fn alias(setting: &str, depth: usize, vars: &Vars) -> Result<(), Refusal> {
    let Some((key, value)) = setting.split_once('=') else {
        return Ok(());
    };
    if !key.to_lowercase().starts_with("alias.") {
        return Ok(());
    }
    match value.trim_start().strip_prefix('!') {
        Some(code) => read(code, Ctx::Shell, depth + 1, vars),
        None => read(&format!("git {value}"), Ctx::Data, depth + 1, vars),
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
        // A word whose start is a substitution or an expansion with an operator (`${v:-...}`):
        // its value, which may be an option, is not known.
        if w.built && (t.starts_with('$') || t.starts_with('`')) {
            return refuse(
                format!("{t} in git {sub}"),
                format!(
                    "a word of git {sub} made by a substitution or an expansion may be an option the hook cannot read; write the options plainly"
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
