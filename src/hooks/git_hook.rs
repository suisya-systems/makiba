//! `makiba hook pre-commit|pre-merge-commit|pre-applypatch|commit-msg|applypatch-msg`
//! (Issue 136, line L-K12):
//! the git hooks that keep a token-shaped line out of the history. `makibad` points a code run's
//! `core.hooksPath` at a folder of its own whose scripts run these, and then the project's own
//! hook of the same name (Issue 236; see `install`).
//!
//! Unlike the Claude Code hooks of this module's parent these are run by git: they read no JSON
//! payload. The first three read every blob the index would commit that differs from `HEAD`, as
//! raw bytes from the object store (no diff text, so attributes, text conversion and binary
//! detection change nothing); `commit-msg` and `applypatch-msg` (`git am`) read the message file
//! git names. A match refuses
//! (exit 1) naming the file, the line and the shape, never the matched text. Anything that cannot
//! be read refuses too.
//!
//! When the message is clean, `commit-msg` also checks a test commit before it is made (Issue
//! 322, line L-K9; see `test_commit`): an `Acceptance-Test` line git does not read as a trailer, a
//! declared path the commit does not change, or a declared test that starts makibad directly.
//!
//! `makiba hook prepare-commit-msg <pid>` (Issue 300, line L-K3) holds git's flag that skips the
//! hooks: git runs `prepare-commit-msg` even with that flag, and its script passes the id of the
//! git that runs it, whose argv (as exec received it, after the shell) is read for the flag
//! (`skipping_flag`). It reads no shell text. A commit git's own rebase makes with the flag
//! (`git rebase --continue` after a stop; git 2.43's cherry-pick continues without it, and
//! revert commits in its own process) is refused too, with no exception (Issue 300, the
//! person's choice): the refusal names `git merge` as the way to bring a branch up to date.
//! Linux only; elsewhere it passes and the worker's Bash hook keeps reading the flag in the
//! command text.

use super::Refusal;
use regex::bytes::Regex;
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};

/// The git hooks of the scanner, by the names git runs them.
pub const NAMES: [&str; 5] = [
    "pre-commit",
    "pre-merge-commit",
    "pre-applypatch",
    "commit-msg",
    "applypatch-msg",
];

/// The fixed list of token shapes: a name and a pattern each.
const SHAPES: [(&str, &str); 6] = [
    ("an AWS access key id", r"AKIA[0-9A-Z]{16}"),
    ("a GitHub token", r"gh[pousr]_[A-Za-z0-9]{36}"),
    (
        "a GitHub fine-grained token",
        r"github_pat_[A-Za-z0-9_]{82}",
    ),
    ("a private key", r"-----BEGIN ([A-Z0-9]+ )?PRIVATE KEY-----"),
    ("an Anthropic key", r"sk-ant-[A-Za-z0-9_-]{20,}"),
    ("a Slack token", r"xox[abprs]-[A-Za-z0-9-]{10,}"),
];

/// The first token shape in `bytes`: its name and its 1-based line.
pub fn find(bytes: &[u8]) -> Option<(&'static str, usize)> {
    SHAPES
        .iter()
        .filter_map(|(name, pattern)| {
            let m = Regex::new(pattern).expect("a valid pattern").find(bytes)?;
            Some((m.start(), *name))
        })
        .min()
        .map(|(at, name)| {
            let line = bytes[..at].iter().filter(|&&b| b == b'\n').count() + 1;
            (name, line)
        })
}

pub(super) fn git(args: &[&str]) -> Result<Vec<u8>, String> {
    let out = Command::new("git")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out.stdout)
}

/// Every path and blob the index holds that differs from `HEAD` (all of it before a first
/// commit), gitlinks and deletions left out.
pub(super) fn staged() -> Result<Vec<(String, String)>, String> {
    let base = match git(&["rev-parse", "--verify", "-q", "HEAD^{tree}"]) {
        Ok(t) => String::from_utf8_lossy(&t).trim().to_string(),
        // No commit yet: the empty tree, in the repository's own hash.
        Err(_) => {
            let t = git(&["hash-object", "-t", "tree", "/dev/null"])?;
            String::from_utf8_lossy(&t).trim().to_string()
        }
    };
    let raw = git(&[
        "diff-index",
        "--cached",
        "--raw",
        "-z",
        "--no-renames",
        "--no-abbrev",
        &base,
    ])?;
    // `:<old mode> <new mode> <old oid> <new oid> <status>` NUL `<path>` NUL, repeated.
    let mut fields = raw.split(|&b| b == 0).filter(|f| !f.is_empty());
    let mut out = Vec::new();
    while let Some(meta) = fields.next() {
        let path = fields.next().ok_or("git diff-index ended early")?;
        let meta = String::from_utf8_lossy(meta);
        let parts: Vec<&str> = meta.trim_start_matches(':').split(' ').collect();
        let [_, mode, _, oid, status] = parts[..] else {
            return Err(format!("cannot read git diff-index: {meta}"));
        };
        if status == "D" || mode == "160000" {
            continue;
        }
        out.push((String::from_utf8_lossy(path).into_owned(), oid.to_string()));
    }
    Ok(out)
}

/// The line the scanner holds.
const LINE: &str = "L-K12";

fn check(name: &str, args: &[String]) -> Result<(), Refusal> {
    let found = if name == "commit-msg" || name == "applypatch-msg" {
        let cannot = |why: String| Refusal::new(LINE, "the commit message file", why);
        let file = args
            .first()
            .ok_or_else(|| cannot("git named no message file".into()))?;
        let text = std::fs::read(file).map_err(|e| cannot(format!("cannot read {file}: {e}")))?;
        match find(&text) {
            Some((shape, line)) => Some(("the commit message".to_string(), shape, line)),
            // A clean message of a commit: the test commit's check (Issue 322).
            None if name == "commit-msg" => {
                super::test_commit::design_wait(file, &text)?;
                super::test_commit::check(file, &text)?;
                None
            }
            None => None,
        }
    } else {
        let cannot = |why: String| Refusal::new(LINE, "the staged files", why);
        let mut found = None;
        for (path, oid) in staged().map_err(cannot)? {
            let bytes = git(&["cat-file", "blob", &oid]).map_err(cannot)?;
            if let Some((shape, line)) = find(&bytes) {
                found = Some((path, shape, line));
                break;
            }
        }
        found
    };
    match found {
        None => Ok(()),
        // The file, the line and the shape; never the matched text, which may be a secret.
        Some((what, shape, line)) => Err(Refusal::new(
            LINE,
            format!("{what}, line {line}, {shape}"),
            format!(
                "{what} holds a line shaped like {shape} (line {line}); a secret must not enter the git history. \
                 Take it out, or build test data at run time; a person commits a false positive themselves"
            ),
        )),
    }
}

/// Run the git hook `name` with git's arguments.
pub fn main(name: &str, args: &[String]) -> ExitCode {
    match check(name, args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(refusal) => {
            eprintln!("{}", refusal.message(name));
            ExitCode::from(1)
        }
    }
}

/// The git hook that refuses git's flag that skips the hooks (Issue 300, line L-K3). git runs it
/// even with that flag, so it sees what `pre-commit` and `commit-msg` would not.
pub const FLAG_HOOK: &str = "prepare-commit-msg";

/// The line the flag check holds.
const FLAG_LINE: &str = "L-K3";

/// git commit's short options that take a value (required `m F C c t`, stuck-optional `S u`):
/// what follows one of them in a bundle is its value.
const VALUE_SHORT: &[char] = &['m', 'F', 'C', 'c', 't', 'S', 'u'];

/// The element of `argv` (the argv of the git that runs the hook) that asks git commit to skip
/// the hooks, if any. The commit's options are the elements after the first one equal to
/// `commit` (after argv[0] when its basename is `git-commit`), read to the end: a `--` is not a
/// stop, so `-m -- -n` is read too. Refused: a prefix of `--no-verify` at least as long as
/// `--no-ver` (up to `=`), and a single-dash bundle whose letters hold `n` before the first
/// short option that takes a value. Conservative: a separate value that looks like the flag
/// (`-m -n`) is refused too; with no `commit` element (merge, rebase, cherry-pick), nothing is.
pub fn skipping_flag(argv: &[String]) -> Option<String> {
    let dashed = argv
        .first()
        .and_then(|a| Path::new(a).file_name())
        .is_some_and(|n| n == "git-commit");
    let start = if dashed {
        1
    } else {
        argv.iter().skip(1).position(|a| a == "commit")? + 2
    };
    argv.iter().skip(start).find(|a| is_skip(a)).cloned()
}

fn is_skip(arg: &str) -> bool {
    if let Some(long) = arg.strip_prefix("--") {
        let name = long.split('=').next().unwrap_or_default();
        return name.len() >= "no-ver".len() && "no-verify".starts_with(name);
    }
    let Some(bundle) = arg.strip_prefix('-') else {
        return false;
    };
    for c in bundle.chars() {
        if c == 'n' {
            return true;
        }
        if VALUE_SHORT.contains(&c) || !c.is_ascii_alphabetic() {
            return false;
        }
    }
    false
}

/// The argv of process `pid`, as exec received it.
#[cfg(target_os = "linux")]
fn argv_of(pid: &str) -> Result<Vec<String>, String> {
    if pid.is_empty() || !pid.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!(
            "{pid:?} is not the id of the git that runs the hook"
        ));
    }
    let raw = std::fs::read(format!("/proc/{pid}/cmdline"))
        .map_err(|e| format!("cannot read the argv of git (process {pid}): {e}"))?;
    Ok(raw
        .split(|&b| b == 0)
        .filter(|a| !a.is_empty())
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect())
}

fn check_flag(args: &[String]) -> Result<(), Refusal> {
    #[cfg(target_os = "linux")]
    {
        let pid = args.first().map(String::as_str).unwrap_or_default();
        let argv = argv_of(pid).map_err(|why| Refusal::new(FLAG_LINE, "the argv of git", why))?;
        if let Some(arg) = skipping_flag(&argv) {
            return Err(Refusal::new(
                FLAG_LINE,
                arg.clone(),
                format!(
                    "git commit {arg} skips the repository's hooks (the secret scanner among them); run it without the flag. \
                     If these letters are part of the message, pass it as -m \"<text>\" or put it in a file and pass -F <file>. \
                     A rebase continued after a stop makes git commit the step with this flag, so it is refused too: \
                     abort it (git rebase --abort) and bring the branch up to date with git merge"
                ),
            ));
        }
    }
    // Elsewhere the Bash hook `deny-no-verify-code-run` keeps reading the flag in the text.
    #[cfg(not(target_os = "linux"))]
    let _ = args;
    Ok(())
}

/// Run `FLAG_HOOK` with the id of the git that runs it, then git's arguments.
pub fn flag_main(args: &[String]) -> ExitCode {
    match check_flag(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(refusal) => {
            eprintln!("{}", refusal.message(FLAG_HOOK));
            ExitCode::from(1)
        }
    }
}

/// The shell text of a refusal by the script of hook `name`, as `Refusal::message` writes it;
/// `reason` and `matched` may hold shell expansions (inside double quotes).
fn script_refusal(name: &str, reason: &str, matched: &str) -> String {
    format!(
        "echo \"makiba: refused by hook {name} (line {LINE} in docs/design/lines.md): {reason}. Matched: \\\"{matched}\\\"\" >&2"
    )
}

/// Shell single quotes around `s`.
fn quoted(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// The hook names whose scripts only forward to the project's own hook (Issue 236): every name
/// git runs by name except the scanner's, `FLAG_HOOK`, `push-to-checkout` and `proc-receive`
/// (whose mere presence changes what git does) and `fsmonitor-watchman` (not run by name).
pub const FORWARDED: [&str; 18] = [
    "post-commit",
    "pre-rebase",
    "post-checkout",
    "post-merge",
    "post-rewrite",
    "post-applypatch",
    "pre-push",
    "pre-auto-gc",
    "reference-transaction",
    "post-index-change",
    "sendemail-validate",
    "pre-receive",
    "update",
    "post-receive",
    "post-update",
    "p4-changelist",
    "p4-prepare-changelist",
    "p4-post-changelist",
];

/// The run's configuration key naming the project's own hooks folder (Issue 236).
pub const PROJECT_KEY: &str = "makiba.projectHooksPath";

/// The shell lines that run the project's hook `name` from the folder `PROJECT_KEY` names, if
/// there is one, as the last step when `last` (its status is the script's).
fn forward(name: &str, last: bool) -> String {
    let run = if last { "exec " } else { "" };
    let then = if last { "" } else { " || exit $?" };
    let refused = script_refusal(
        name,
        &format!("{PROJECT_KEY} names the secret scanner's own folder $d"),
        &format!("{PROJECT_KEY}=$d"),
    );
    format!(
        "d=$(git config --get {PROJECT_KEY}) || exit 0\n\
         [ -x \"$d/{name}\" ] || exit 0\n\
         if [ \"$d/{name}\" -ef \"$0\" ]; then\n\
         \x20 {refused}\n\
         \x20 exit 1\n\
         fi\n\
         {run}\"$d/{name}\" \"$@\"{then}\n"
    )
}

/// Write the scripts of `NAMES`, `FLAG_HOOK` and `FORWARDED` into `dir`, each running `makiba`
/// (an absolute path) and refusing with the path when it is missing or cannot run. A scanner
/// script scans, runs the project's hook of its name (Issue 236) and scans again, for what that
/// hook staged or wrote; the `FLAG_HOOK` script passes makiba its parent's id (`$PPID`, the git
/// that runs the hook, since git starts a hook itself) and then runs the project's hook; a
/// forwarding script runs the project's hook only. What `prepare-commit-msg` or
/// `commit-msg` stages is not in that commit (git writes its tree before them): it stays staged,
/// and the next commit's `pre-commit` scan reads it. A script already as wanted is left as it is.
pub fn install(dir: &Path, makiba: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir)?;
    let m = quoted(&makiba.to_string_lossy());
    let head = |name: &str| {
        let refused = script_refusal(
            name,
            "the secret scanner $m is missing or cannot run; install makiba at that path again, or restart makibad from the installed binary and prepare a worktree, which rewrites this hook",
            "$m",
        );
        format!(
            "#!/bin/sh\n\
             # Written by makibad (Issues 136, 236): the secret scanner of a code run, then the project's own hook. Preparing a worktree rewrites it.\n\
             m={m}\n\
             if [ ! -x \"$m\" ]; then\n\
             \x20 {refused}\n\
             \x20 exit 1\n\
             fi\n"
        )
    };
    let scripts = NAMES
        .iter()
        .map(|name| {
            let script = format!(
                "{}\"$m\" hook {name} \"$@\" || exit $?\n{}exec \"$m\" hook {name} \"$@\"\n",
                head(name),
                forward(name, false)
            );
            (name, script)
        })
        .chain(std::iter::once((
            &FLAG_HOOK,
            format!(
                "{}\"$m\" hook {FLAG_HOOK} \"$PPID\" \"$@\" || exit $?\n{}",
                head(FLAG_HOOK),
                forward(FLAG_HOOK, true)
            ),
        )))
        .chain(FORWARDED.iter().map(|name| {
            let script = format!(
                "#!/bin/sh\n# Written by makibad (Issue 236): the project's own hook. Preparing a worktree rewrites it.\n{}",
                forward(name, true)
            );
            (name, script)
        }));
    for (name, script) in scripts {
        let path = dir.join(name);
        if std::fs::read_to_string(&path).ok().as_deref() != Some(&script) {
            std::fs::write(&path, &script)?;
        }
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn the_flag_is_read_from_git_commits_arguments_only() {
        let refused = |w: &[&str]| skipping_flag(&argv(w));
        let g = ["git", "-C", "/w", "-c", "user.name=t"];
        let with = |rest: &[&'static str]| [&g[..], rest].concat();
        for (rest, arg) in [
            (&["commit", "-n", "-m", "x"][..], "-n"),
            (&["commit", "--no-verify"], "--no-verify"),
            (&["commit", "--no-ver"], "--no-ver"),
            (&["commit", "--no-verify=1"], "--no-verify=1"),
            (&["commit", "-anm", "x"], "-anm"),
            (&["commit", "-qn"], "-qn"),
            (&["commit", "-m", "--", "-n"], "-n"),
            (&["commit", "-m", "-n"], "-n"),
        ] {
            assert_eq!(refused(&with(rest)).as_deref(), Some(arg), "{rest:?}");
        }
        assert_eq!(
            refused(&["/usr/lib/git-core/git-commit", "-n"]).as_deref(),
            Some("-n")
        );
        for rest in [
            &["commit", "-m", "bash -n; never --no-verify"][..],
            &["commit", "-uno", "-m", "x"],
            &["commit", "-Cmain"],
            &["commit", "-Fnotes.txt"],
            &["commit", "-mrun"],
            &["commit", "--no-verbose", "-m", "x"],
            &["commit", "--no-ve"],
            &["merge", "-n", "x"],
            &["rebase", "--continue"],
        ] {
            assert_eq!(refused(&with(rest)), None, "{rest:?}");
        }
    }

    #[test]
    fn each_shape_is_found_on_its_line_and_plain_text_is_not() {
        let aws = format!("{}{}", "AKIA", "Z".repeat(16));
        assert_eq!(
            find(format!("a\nb {aws}\n").as_bytes()),
            Some((SHAPES[0].0, 2))
        );
        assert_eq!(find(b"AKIA lowercase akia0123456789abcdef"), None);
        // Line counting goes through NUL bytes, as in a file git calls binary.
        assert_eq!(
            find(format!("\0\n\0{aws}").as_bytes()).map(|f| f.1),
            Some(2)
        );
        // The patterns as written here are not tokens themselves.
        for (_, p) in SHAPES {
            assert_eq!(find(p.as_bytes()), None, "{p}");
        }
    }
}
