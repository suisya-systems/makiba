//! The check of a test commit when it is made (Issue 322, line L-K9), run by the git hook
//! `commit-msg` after the secret scanner (`git_hook`), and the one place of the scan that says
//! whether a test target starts `makibad` other than through `tests/common/makibad.rs` (Issue
//! 312; `tests/knowledge_isolation.rs` runs the same functions over every target).
//!
//! A declared-test commit cannot change once anything past it is pushed, so its flaws are refused
//! before it exists. A message that declares `Acceptance-Test` (a line starting with that key, or
//! a trailer git reads under it) is refused when:
//!
//! 1. it has more such lines than git reads as trailers (`git interpret-trailers --parse`), as on
//!    pull request 308, where a blank line before `Co-Authored-By` left the key out of the block;
//! 2. the run's project has a `prepare-commit-msg` or `commit-msg` hook of its own: it runs after
//!    git wrote the commit's tree, so what it stages is in the index read here and not in the
//!    commit (and a `commit-msg` hook may add the declaration itself);
//! 3. a declared path names no file the commit adds or changes against HEAD (nor a folder holding
//!    one). Git tells the hook nothing of `--amend`, so an amend is judged against the commit it
//!    amends: an amend that does not change the declared file again is refused, and the refusal
//!    names the remedy (`git reset --soft HEAD~1`, then commit again);
//! 4. a declared Rust test target, as the index holds it, starts makibad directly, as on pull
//!    requests 315 and 316. Only a test that names makiba (its crate or binary) is scanned: the
//!    rule is makiba's own, and another project's test may well say "daemon".
//!
//! Known limit: lines below a scissors line are not counted, but git keeps a scissors line
//! written by hand, and what is below it, in a commit made with `-F`; such a key line passes here
//! and is refused at the push (`daemon::gate::stray_trailer`).
//!
//! A message with no such line and no such trailer is not read further. What cannot be read
//! refuses, as the scanner does.

use super::Refusal;
use super::git_hook::{PROJECT_KEY, git, staged};
use std::path::Path;

/// The line this check holds: the test-first proof of the acceptance gate.
const LINE: &str = "L-K9";

/// The trailer key, as the merge gate reads it.
const KEY: &str = crate::daemon::gate::TRAILER;

/// What git's scissors line holds after the comment character; git drops it and all below.
const CUT: &str = " ------------------------ >8 ------------------------";

/// Why each refusal matters, said once.
const FINAL: &str = "a test commit cannot change once anything past it is pushed";

/// Whether `rel` (a path from the repository's root) is a Cargo test target: `tests/<name>.rs`,
/// or `tests/<name>/main.rs` for a folder other than `tests/common`.
pub fn is_test_target(rel: &str) -> bool {
    let Some(r) = rel.strip_prefix("tests/") else {
        return false;
    };
    match r.split_once('/') {
        None => r.ends_with(".rs") && r.len() > 3,
        Some((dir, rest)) => rest == "main.rs" && !dir.is_empty() && dir != "common",
    }
}

/// Every line of the Rust source `text` that starts makibad directly, with its 1-based number and
/// its code: a call of `makiba::daemon::run*` or `main`, or of the binary's `daemon` or `up`, or an
/// import of those functions. Code only: what follows `//` on a line is left out.
pub fn direct_starts(text: &str) -> Vec<(usize, String)> {
    let direct = [
        "daemon::run(",
        "daemon::run_with(",
        "daemon::run_until(",
        "daemon::main(",
        "\"daemon\"",
        "arg(\"up\")",
        "args([\"up\"",
        "args(&[\"up\"",
        "makiba(&[\"up\"",
    ];
    let mut found = Vec::new();
    let mut in_use = false;
    for (i, line) in text.lines().enumerate() {
        let code = line.split("//").next().unwrap_or("");
        let code = code.trim();
        // `use makiba::daemon::{..., run_until, ...}` and its continuation lines.
        if code.starts_with("use makiba::daemon::") || code.starts_with("use makiba::daemon;") {
            in_use = !code.ends_with(';');
        }
        let imported = (code.starts_with("use makiba::daemon::") || in_use)
            && ["run", "run_with", "run_until", "main"].iter().any(|f| {
                code.split(|c: char| !(c.is_alphanumeric() || c == '_'))
                    .any(|w| w == *f)
            });
        if code.ends_with(';') {
            in_use = false;
        }
        if imported || direct.iter().any(|p| code.contains(p)) {
            found.push((i + 1, code.to_string()));
        }
    }
    found
}

fn text(bytes: Vec<u8>) -> String {
    String::from_utf8_lossy(&bytes).into_owned()
}

/// The value of a line that starts with the key (case aside, spaces allowed before the colon).
fn key_value(line: &str) -> Option<&str> {
    let t = line.trim_start();
    let head = t.get(..KEY.len())?;
    if !head.eq_ignore_ascii_case(KEY) {
        return None;
    }
    let rest = t[KEY.len()..].trim_start_matches([' ', '\t']);
    rest.strip_prefix(':').map(str::trim)
}

/// A declared path as the merge gate and the acceptance checker read it: trimmed, empty and `.`
/// segments dropped; nothing left is `.`.
fn normalized(value: &str) -> String {
    let parts: Vec<&str> = value
        .trim()
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect();
    if parts.is_empty() {
        ".".into()
    } else {
        parts.join("/")
    }
}

/// Whether `file` is `path` or inside it.
fn under(file: &str, path: &str) -> bool {
    file == path
        || file
            .strip_prefix(path)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// The lines of `message` git keeps: those above the scissors line, when there is one with
/// git's comment character (`#`, or `core.commentChar`). Another character cuts nothing, so a
/// line is never dropped that git would keep.
fn kept(message: &str) -> Vec<&str> {
    let configured = git(&["config", "--get", "core.commentChar"])
        .map(text)
        .unwrap_or_default();
    let configured = configured.trim();
    message
        .lines()
        .take_while(|l| {
            !l.strip_suffix(CUT)
                .is_some_and(|c| c == "#" || (!configured.is_empty() && c == configured))
        })
        .collect()
}

/// The values git reads under the key in the message file `file` (`--no-divider`: a commit's
/// trailers read none), with the repository's own separators.
fn read_values(file: &str) -> Result<Vec<String>, String> {
    let parsed = text(git(&[
        "interpret-trailers",
        "--parse",
        "--no-divider",
        file,
    ])?);
    Ok(parsed
        .lines()
        .filter_map(|l| {
            let end = l
                .find(|c: char| !(c.is_alphanumeric() || c == '-'))
                .unwrap_or(l.len());
            let rest = l[end..].trim_start();
            let sep = rest.chars().next()?;
            l[..end]
                .eq_ignore_ascii_case(KEY)
                .then(|| rest[sep.len_utf8()..].trim().to_string())
        })
        .collect())
}

/// The line the design check holds (Issue 337).
const DESIGN_LINE: &str = "L-K13";

/// Issue 337 (line L-K13): a commit whose message declares a test (a line starting with the key,
/// spaces or a tab allowed before the colon, case aside; or a trailer git reads under it) is
/// refused while `makibad`'s marker `<hooks path>/design-wait/<task>` exists, the task being the
/// name of the run's git folder `<state>/git/<task>`. The marker holds the reason: the task was
/// dispatched design-first and the person has not said yes to its design (`push::design_wait`).
/// Run before `check`, so its refusal is the one shown. A message that declares a test where the
/// hooks path or the git folder cannot be read is refused too; any other message is not read
/// further. The push holds the same line outside the worker's git, which reads its own command
/// line and environment.
pub fn design_wait(file: &str, message: &[u8]) -> Result<(), Refusal> {
    let message = String::from_utf8_lossy(message);
    let written = kept(&message)
        .into_iter()
        .find(|l| key_value(l).is_some())
        .map(|l| l.trim().to_string());
    let line = match written {
        Some(l) => l,
        None => {
            let unread = |why: String| Refusal::new(DESIGN_LINE, "the commit message", why);
            if read_values(file).map_err(unread)?.is_empty() {
                return Ok(());
            }
            format!("{KEY} (a trailer)")
        }
    };
    let cannot = |why: String| {
        Refusal::new(
            DESIGN_LINE,
            line.clone(),
            format!(
                "the commit declares a test, and whether its design is approved cannot be read ({why}); {FINAL}"
            ),
        )
    };
    let hooks = text(git(&["config", "--get", "core.hooksPath"]).map_err(&cannot)?);
    let common =
        text(git(&["rev-parse", "--path-format=absolute", "--git-common-dir"]).map_err(&cannot)?);
    let task = Path::new(common.trim_end_matches('\n'))
        .file_name()
        .ok_or_else(|| cannot(format!("the git folder {common:?} has no name")))?
        .to_string_lossy()
        .into_owned();
    let marker = Path::new(hooks.trim_end_matches('\n'))
        .join(crate::daemon::prepare::DESIGN_WAIT)
        .join(&task);
    if !marker.exists() {
        return Ok(());
    }
    let why = std::fs::read_to_string(&marker).unwrap_or_else(|e| {
        format!("task {task} waits for the person's approval of its design ({e})")
    });
    Err(Refusal::new(
        DESIGN_LINE,
        line,
        format!(
            "{}; the test commit is not made before the design is approved: report the design and wait for the person's yes to its pending entry of stop kind design; {FINAL}",
            why.trim()
        ),
    ))
}

/// Check the commit whose message file is `file`, holding `message`.
pub fn check(file: &str, message: &[u8]) -> Result<(), Refusal> {
    let cannot = |why: String| Refusal::new(LINE, "the commit message", why);
    let message = String::from_utf8_lossy(message);
    // The lines of the key, as written, with their numbers.
    let written: Vec<(usize, &str, &str)> = kept(&message)
        .into_iter()
        .enumerate()
        .filter_map(|(i, l)| key_value(l).map(|v| (i + 1, l.trim(), v)))
        .collect();
    let read = read_values(file).map_err(cannot)?;
    if written.is_empty() && read.is_empty() {
        return Ok(());
    }

    // 1. Every line of the key is a trailer git reads.
    if written.len() > read.len() {
        let mut left = read.clone();
        let mut stray = Vec::new();
        for (n, line, value) in &written {
            match left.iter().position(|r| r == value) {
                Some(at) => {
                    left.remove(at);
                }
                None => stray.push((*n, *line)),
            }
        }
        if stray.is_empty() {
            stray = written.iter().map(|(n, l, _)| (*n, *l)).collect();
        }
        let numbers: Vec<String> = stray.iter().map(|(n, _)| n.to_string()).collect();
        let lines: Vec<&str> = stray.iter().map(|(_, l)| *l).collect();
        return Err(Refusal::new(
            LINE,
            lines.join("; "),
            format!(
                "the commit message has an {KEY} line that git does not read as a trailer (line {}), so the commit would declare no test: put {KEY} and Co-Authored-By in one last paragraph with no blank line between them (git interpret-trailers --parse shows what git reads); {FINAL}",
                numbers.join(", ")
            ),
        ));
    }

    // 2. No project hook runs after git wrote the commit's tree: prepare-commit-msg before both
    // checks, commit-msg between the scanner's two. What either stages is in the index read here
    // and not in the commit, and a commit-msg hook may also add the declaration itself.
    if let Ok(dir) = git(&["config", "--get", PROJECT_KEY]) {
        let dir = text(dir);
        for name in ["prepare-commit-msg", "commit-msg"] {
            let hook = Path::new(dir.trim_end_matches('\n')).join(name);
            if is_executable(&hook) {
                let shown = hook.display().to_string();
                return Err(Refusal::new(
                    LINE,
                    shown.clone(),
                    format!(
                        "the project's {name} hook {shown} runs after git wrote the commit's tree and may stage what the commit does not hold, so the declared tests cannot be read as committed: make the test commit where that hook is not run, or ask the person; {FINAL}"
                    ),
                ));
            }
        }
    }

    // 3. Every declared path is a file the commit adds or changes, or a folder holding one.
    let changed: Vec<String> = staged()
        .map_err(cannot)?
        .into_iter()
        .map(|(p, _)| p)
        .collect();
    // A declaration the merge gate rejects (`gate::find_manifest`) is refused before it is
    // normalized: an absolute path, a `..` segment, a backslash, a leading `-`.
    if let Some(v) = read.iter().find(|v| {
        v.starts_with('/')
            || v.split('/').any(|s| s == "..")
            || v.contains('\\')
            || v.starts_with('-')
    }) {
        return Err(Refusal::new(
            LINE,
            format!("{KEY}: {v}"),
            format!(
                "the commit declares {v:?}, a path the merge gate rejects (absolute, with a .. segment, a backslash or a leading -): declare the test file by its path from the repository's root, such as tests/<name>.rs; {FINAL}"
            ),
        ));
    }
    let declared: Vec<String> = read.iter().map(|v| normalized(v)).collect();
    if let Some(d) = declared
        .iter()
        .find(|d| !changed.iter().any(|f| under(f, d)))
    {
        return Err(Refusal::new(
            LINE,
            format!("{KEY}: {d}"),
            format!(
                "the commit declares {d}, which it neither adds nor changes: declare the test file this commit adds, or add it in this commit. The check reads the index against HEAD, so an amend that does not change the declared file again is refused: to change a test commit already made, run git reset --soft HEAD~1 and commit again; {FINAL}"
            ),
        ));
    }

    // 4. No declared Rust test starts makibad directly, as the index holds it. The rule is
    // makiba's own: only a test that names the makiba crate or binary is scanned, so a test of
    // another project that says "daemon" is not refused.
    let index = git(&["ls-files", "-z"]).map_err(cannot)?;
    let index = text(index);
    for path in index
        .split('\0')
        .filter(|p| is_test_target(p) && declared.iter().any(|d| under(p, d)))
    {
        let source = git(&["cat-file", "blob", &format!(":{path}")]).map_err(cannot)?;
        let source = text(source);
        if !source.contains("makiba") {
            continue;
        }
        if let Some((n, code)) = direct_starts(&source).into_iter().next() {
            return Err(Refusal::new(
                LINE,
                format!("{path}:{n}: {code}"),
                format!(
                    "the declared test {path} starts makibad directly (line {n}): start it through tests/common/makibad.rs (daemon, up, run, run_with, run_until), the rule of tests/knowledge_isolation.rs; {FINAL}"
                ),
            ));
        }
    }
    Ok(())
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_keys_and_paths_are_read_as_the_gate_reads_them() {
        assert!(is_test_target("tests/a.rs"));
        assert!(is_test_target("tests/a/main.rs"));
        assert!(!is_test_target("tests/common/main.rs"));
        assert!(!is_test_target("tests/a/b.rs"));
        assert!(!is_test_target("tests/test_a.py"));
        assert!(!is_test_target("src/a.rs"));
        assert_eq!(key_value("Acceptance-Test: tests/a.rs"), Some("tests/a.rs"));
        assert_eq!(
            key_value("  acceptance-test :\ttests/a.rs"),
            Some("tests/a.rs")
        );
        assert_eq!(key_value("Acceptance-Tests: x"), None);
        assert_eq!(key_value("No Acceptance-Test: x"), None);
        assert_eq!(normalized("./tests//a.rs"), "tests/a.rs");
        assert_eq!(normalized("./"), ".");
        assert!(under("tests/a/main.rs", "tests/a"));
        assert!(!under("tests/ab.rs", "tests/a"));
    }
}
