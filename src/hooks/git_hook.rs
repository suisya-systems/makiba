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

fn git(args: &[&str]) -> Result<Vec<u8>, String> {
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
fn staged() -> Result<Vec<(String, String)>, String> {
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

fn check(name: &str, args: &[String]) -> Result<(), String> {
    let found = if name == "commit-msg" || name == "applypatch-msg" {
        let file = args.first().ok_or("git named no message file")?;
        let text = std::fs::read(file).map_err(|e| format!("cannot read {file}: {e}"))?;
        find(&text).map(|(shape, line)| ("the commit message".to_string(), shape, line))
    } else {
        let mut found = None;
        for (path, oid) in staged()? {
            let bytes = git(&["cat-file", "blob", &oid])?;
            if let Some((shape, line)) = find(&bytes) {
                found = Some((path, shape, line));
                break;
            }
        }
        found
    };
    match found {
        None => Ok(()),
        Some((what, shape, line)) => Err(format!(
            "{what} holds a line shaped like {shape} (line {line}); a secret must not enter the git history. \
             Take it out, or build test data at run time; a person commits a false positive themselves"
        )),
    }
}

/// Run the git hook `name` with git's arguments.
pub fn main(name: &str, args: &[String]) -> ExitCode {
    match check(name, args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("makiba: refused: {why}");
            ExitCode::from(1)
        }
    }
}

/// Shell single quotes around `s`.
fn quoted(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// The hook names whose scripts only forward to the project's own hook (Issue 236): every name
/// git runs by name except the scanner's, `push-to-checkout` and `proc-receive` (whose mere
/// presence changes what git does) and `fsmonitor-watchman` (not run by name).
pub const FORWARDED: [&str; 19] = [
    "prepare-commit-msg",
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
    format!(
        "d=$(git config --get {PROJECT_KEY}) || exit 0\n\
         [ -x \"$d/{name}\" ] || exit 0\n\
         if [ \"$d/{name}\" -ef \"$0\" ]; then\n\
         \x20 echo \"makiba: refused: {PROJECT_KEY} names the secret scanner's own folder $d\" >&2\n\
         \x20 exit 1\n\
         fi\n\
         {run}\"$d/{name}\" \"$@\"{then}\n"
    )
}

/// Write the scripts of `NAMES` and `FORWARDED` into `dir`, each running `makiba` (an absolute
/// path) and refusing with the path when it is missing or cannot run. A scanner script scans,
/// runs the project's hook of its name (Issue 236) and scans again, for what that hook staged or
/// wrote; a forwarding script runs the project's hook only. What `prepare-commit-msg` or
/// `commit-msg` stages is not in that commit (git writes its tree before them): it stays staged,
/// and the next commit's `pre-commit` scan reads it. A script already as wanted is left as it is.
pub fn install(dir: &Path, makiba: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir)?;
    let m = quoted(&makiba.to_string_lossy());
    let head = format!(
        "#!/bin/sh\n\
         # Written by makibad (Issues 136, 236): the secret scanner of a code run, then the project's own hook. Preparing a worktree rewrites it.\n\
         m={m}\n\
         if [ ! -x \"$m\" ]; then\n\
         \x20 echo \"makiba: refused: the secret scanner $m is missing or cannot run; install makiba at that path again, or restart makibad from the installed binary and prepare a worktree, which rewrites this hook\" >&2\n\
         \x20 exit 1\n\
         fi\n"
    );
    let scripts = NAMES
        .iter()
        .map(|name| {
            let script = format!(
                "{head}\"$m\" hook {name} \"$@\" || exit $?\n{}exec \"$m\" hook {name} \"$@\"\n",
                forward(name, false)
            );
            (name, script)
        })
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
