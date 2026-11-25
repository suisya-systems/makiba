//! Whether a pull request's head is ready for the merge queue (Issue 475, docs/design/queue-ready.md):
//! the one place that holds which landings make a head not ready, and the git reading the PR
//! watch's readiness readers run. It owns no thread, table or timer: the PR watch
//! (`prwatch.rs`) decides when to read and writes what it decides.
//!
//! A head is ready when its CI passed, it does not conflict with its base, and no landing since
//! the base it holds (`git merge-base`, a conservative stand-in for the base its CI ran on) changed
//! what the checks check: a file under one of `CHECKED_PATHS`, where a change under `.github/` that
//! only alters how runs are scheduled (triggers, concurrency, caching) does not count. makiba has
//! no YAML parser, so that last rule is a line reader that counts whatever it cannot read with
//! certainty (`scheduling_only`).

use super::run_repo::HOOKS_OFF;
use std::path::Path;
use std::process::{Command, Stdio};

/// The paths whose change by a landing makes a head not ready (docs/design/queue-ready.md
/// section 1). `.github/` counts only for what `scheduling_only` cannot clear.
pub const CHECKED_PATHS: &[&str] = &["tests/common/", "tools/", ".github/"];

/// A landing on the base that changed what the checks check, with the paths that count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Landing {
    pub sha: String,
    pub paths: Vec<String>,
}

/// One reading of a head against its base.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reading {
    /// The branch at origin is not at the head read for: a newer head is coming.
    Stale,
    /// The base tip is the one already covered: nothing to read.
    Unchanged,
    /// The base tip `base_tip`, the base the head holds `ci_base`, and the landings since that
    /// count (none: ready).
    Read {
        base_tip: String,
        ci_base: String,
        landings: Vec<Landing>,
    },
}

/// `git -C <dir> <args>` with no hook, prompt or replace object: its trimmed output, or git's
/// message.
fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", HOOKS_OFF])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
    } else {
        Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// The refs a reading of `task` fetches into: its own, never `refs/remotes/origin/*`, which the
/// `update` job fetches into (two fetches of one ref collide on git's ref lock, Issue 369).
pub fn refs(task: &str) -> (String, String) {
    (
        format!("refs/makiba/ready/{task}/base"),
        format!("refs/makiba/ready/{task}/head"),
    )
}

/// Read `head` of `branch` against `base` in the project's repository `repo`: fetch both from
/// `origin`, then, unless the base tip is `covered`, the landings since the base the head holds.
pub fn read(
    repo: &Path,
    task: &str,
    base: &str,
    branch: &str,
    head: &str,
    covered: Option<&str>,
) -> Result<Reading, String> {
    let (base_ref, head_ref) = refs(task);
    for r in [&base_ref, &head_ref] {
        git(repo, &["check-ref-format", r])
            .map_err(|_| format!("{r} is not a ref name git takes"))?;
    }
    git(
        repo,
        &[
            "fetch",
            "-q",
            "--no-write-fetch-head",
            "--no-auto-gc",
            "--no-tags",
            "origin",
            &format!("+refs/heads/{base}:{base_ref}"),
            &format!("+refs/heads/{branch}:{head_ref}"),
        ],
    )?;
    let id = |r: &str| git(repo, &["rev-parse", "--verify", &format!("{r}^{{commit}}")]);
    let at = id(&head_ref)?;
    if !at.eq_ignore_ascii_case(head) {
        return Ok(Reading::Stale);
    }
    let base_tip = id(&base_ref)?;
    if covered.is_some_and(|c| c.eq_ignore_ascii_case(&base_tip)) {
        return Ok(Reading::Unchanged);
    }
    let ci_base = git(repo, &["merge-base", head, &base_tip])?;
    let mut landings = Vec::new();
    let list = git(
        repo,
        &[
            "rev-list",
            "--first-parent",
            "--reverse",
            &format!("{ci_base}..{base_tip}"),
        ],
    )?;
    for sha in list.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let paths = landing_paths(repo, sha)?;
        if !paths.is_empty() {
            landings.push(Landing {
                sha: sha.to_string(),
                paths,
            });
        }
    }
    Ok(Reading::Read {
        base_tip,
        ci_base,
        landings,
    })
}

/// The paths of the landing `sha` (against its first parent) that count.
fn landing_paths(repo: &Path, sha: &str) -> Result<Vec<String>, String> {
    let parents = git(repo, &["rev-list", "--parents", "-n", "1", sha])?;
    let Some(parent) = parents.split_whitespace().nth(1) else {
        // A root commit on the base's line: every path is new.
        let all = git(repo, &["ls-tree", "-r", "-z", "--name-only", sha])?;
        return Ok(all
            .split('\0')
            .filter(|p| !p.is_empty() && checked(p))
            .map(String::from)
            .collect());
    };
    let changed = git(
        repo,
        &[
            "diff-tree",
            "-r",
            "-z",
            "--no-renames",
            "--no-commit-id",
            "--name-status",
            parent,
            sha,
        ],
    )?;
    let fields: Vec<&str> = changed.split('\0').collect();
    let mut out = Vec::new();
    for pair in fields.chunks(2) {
        let [status, path] = pair else { continue };
        if path.is_empty() {
            continue;
        }
        if counts(repo, parent, sha, status, path)? {
            out.push(path.to_string());
        }
    }
    Ok(out)
}

/// Whether `path` lies under one of `CHECKED_PATHS`.
fn checked(path: &str) -> bool {
    CHECKED_PATHS.iter().any(|p| path.starts_with(p))
}

/// A workflow file of GitHub Actions: `.github/workflows/<name>.yml` or `.yaml`.
fn workflow(path: &str) -> bool {
    path.strip_prefix(".github/workflows/")
        .is_some_and(|n| !n.contains('/') && (n.ends_with(".yml") || n.ends_with(".yaml")))
}

/// Whether the change of `path` (git's name status `status`) from `parent` to `sha` counts.
fn counts(repo: &Path, parent: &str, sha: &str, status: &str, path: &str) -> Result<bool, String> {
    if !checked(path) {
        return Ok(false);
    }
    if !path.starts_with(".github/") || !workflow(path) || status != "M" {
        return Ok(true);
    }
    let old = git(repo, &["show", &format!("{parent}:{path}")])?;
    let new = git(repo, &["show", &format!("{sha}:{path}")])?;
    let diff = git(
        repo,
        &[
            "diff",
            "-U0",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            parent,
            sha,
            "--",
            path,
        ],
    )?;
    let (removed, added) = hunk_lines(&diff);
    Ok(!scheduling_only(&old, &new, &removed, &added))
}

/// The line numbers (1-based) a `-U0` diff removes from the old file and adds to the new.
pub fn hunk_lines(diff: &str) -> (Vec<usize>, Vec<usize>) {
    let range = |s: &str| -> Vec<usize> {
        let (start, len) = match s.split_once(',') {
            Some((a, b)) => (a.parse().unwrap_or(0), b.parse().unwrap_or(0)),
            None => (s.parse().unwrap_or(0), 1),
        };
        (start..start + len).collect()
    };
    let (mut removed, mut added) = (Vec::new(), Vec::new());
    for line in diff.lines() {
        let Some(rest) = line.strip_prefix("@@ -") else {
            continue;
        };
        let mut parts = rest.split_whitespace();
        let (Some(old), Some(new)) = (parts.next(), parts.next()) else {
            continue;
        };
        removed.extend(range(old));
        added.extend(range(new.trim_start_matches('+')));
    }
    (removed, added)
}

/// Whether every removed line of `old` and every added line of `new` (1-based numbers) only
/// changes how runs are scheduled. A version that cannot be read with certainty, or a line number
/// outside it, makes it false.
pub fn scheduling_only(old: &str, new: &str, removed: &[usize], added: &[usize]) -> bool {
    let (Some(a), Some(b)) = (classes(old), classes(new)) else {
        return false;
    };
    let fine = |cls: &[Class], lines: &[usize]| {
        lines
            .iter()
            .all(|n| *n >= 1 && cls.get(n - 1).is_some_and(|c| *c != Class::Counts))
    };
    fine(&a, removed) && fine(&b, added)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    /// Blank or a comment, outside every block scalar.
    Neutral,
    Scheduling,
    Counts,
}

struct Frame {
    /// The column of the key, or of the item's dash.
    indent: usize,
    /// The key, unquoted; `None` for a sequence item.
    key: Option<String>,
    /// The frame's index in `frames`, to mark an item as a cache step.
    id: usize,
}

/// The key and the value of `text` when it is a mapping entry (`key: value`, `key:`, a quoted
/// key); `None` otherwise.
fn key_of(text: &str) -> Option<(String, &str)> {
    let (key, value) = if let Some(q) = text.strip_prefix('"') {
        let end = q.find('"')?;
        (q[..end].to_string(), q[end + 1..].strip_prefix(':')?)
    } else if let Some(q) = text.strip_prefix('\'') {
        let end = q.find('\'')?;
        (q[..end].to_string(), q[end + 1..].strip_prefix(':')?)
    } else {
        let colon = text
            .find(": ")
            .or_else(|| text.strip_suffix(':').map(str::len))?;
        let key = &text[..colon];
        if key.is_empty() || key.contains(['#', '{', '[', '"', '\'']) {
            return None;
        }
        (key.to_string(), &text[colon + 1..])
    };
    if !(value.is_empty() || value.starts_with(' ')) {
        return None;
    }
    Some((key, value.trim()))
}

/// The value without a trailing comment (a `#` after a space, outside quotes).
fn strip_comment(value: &str) -> &str {
    let (mut single, mut double) = (false, false);
    let bytes = value.as_bytes();
    for (i, c) in value.char_indices() {
        match c {
            '\'' if !double => single = !single,
            '"' if !single => double = !double,
            '#' if !single && !double && (i == 0 || bytes[i - 1] == b' ') => {
                return value[..i].trim_end();
            }
            _ => {}
        }
    }
    value.trim_end()
}

/// Whether a value opens a block scalar (`|` or `>`, with indicators).
fn block_scalar(value: &str) -> bool {
    let mut c = value.chars();
    matches!(c.next(), Some('|' | '>')) && c.all(|x| matches!(x, '-' | '+' | '0'..='9'))
}

/// Whether a value cannot be read with certainty: an anchor or alias, a flow collection or a
/// quoted scalar that does not end on its line.
fn unreadable(value: &str) -> bool {
    if value.starts_with(['&', '*']) || value.starts_with("<<") {
        return true;
    }
    let balanced = |open: char, close: char| {
        value.chars().filter(|c| *c == open).count()
            == value.chars().filter(|c| *c == close).count()
    };
    if value.starts_with(['[', '{']) && !(balanced('[', ']') && balanced('{', '}')) {
        return true;
    }
    for q in ['"', '\''] {
        if value.starts_with(q) && (value.len() < 2 || !value.ends_with(q)) {
            return true;
        }
    }
    false
}

/// Each line's class, or `None` for a file this reader does not read (a tab in an indentation, a
/// document marker, an anchor or alias, a flow or quoted value spanning lines).
fn classes(text: &str) -> Option<Vec<Class>> {
    let mut stack: Vec<Frame> = Vec::new();
    // Per frame id: its key, whether it is an item, whether it is a cache step.
    let mut frames: Vec<(Option<String>, bool)> = Vec::new();
    let mut cache: Vec<bool> = Vec::new();
    // Per line: neutral, or the frame ids enclosing it.
    let mut lines: Vec<Option<Vec<usize>>> = Vec::new();
    // An open block scalar: the column of its key and the frames enclosing it.
    let mut scalar: Option<(usize, Vec<usize>)> = None;
    for raw in text.lines() {
        let body = raw.trim_start_matches(' ');
        let indent = raw.len() - body.len();
        if body.starts_with('\t') || raw[..indent].contains('\t') {
            return None;
        }
        if let Some((col, enclosing)) = &scalar {
            if body.trim().is_empty() || indent > *col {
                lines.push(Some(enclosing.clone()));
                continue;
            }
            scalar = None;
        }
        let trimmed = body.trim_end();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            lines.push(None);
            continue;
        }
        if trimmed == "---"
            || trimmed.starts_with("--- ")
            || trimmed == "..."
            || body.contains('\t')
        {
            return None;
        }
        while stack.last().is_some_and(|f| f.indent >= indent) {
            stack.pop();
        }
        let mut col = indent;
        let mut rest = trimmed;
        if rest == "-" || rest.starts_with("- ") {
            let id = frames.len();
            frames.push((None, true));
            cache.push(false);
            stack.push(Frame {
                indent: col,
                key: None,
                id,
            });
            let after = rest[1..].trim_start_matches(' ');
            col += rest.len() - after.len();
            rest = after;
        }
        if !rest.is_empty() {
            match key_of(rest) {
                Some((key, value)) => {
                    let value = strip_comment(value);
                    if unreadable(value) {
                        return None;
                    }
                    let id = frames.len();
                    frames.push((Some(key.clone()), false));
                    cache.push(false);
                    if key == "uses"
                        && [
                            "actions/cache@",
                            "actions/cache/restore@",
                            "actions/cache/save@",
                        ]
                        .iter()
                        .any(|p| value.starts_with(p))
                        && let Some(owner) = stack.iter().rev().find(|f| f.key.is_none())
                        && stack.last().is_some_and(|f| f.id == owner.id)
                    {
                        cache[owner.id] = true;
                    }
                    stack.push(Frame {
                        indent: col,
                        key: Some(key),
                        id,
                    });
                    if block_scalar(value) {
                        scalar = Some((col, stack.iter().map(|f| f.id).collect()));
                    }
                }
                None => {
                    if unreadable(strip_comment(rest)) {
                        return None;
                    }
                }
            }
        }
        lines.push(Some(stack.iter().map(|f| f.id).collect()));
    }
    Some(
        lines
            .into_iter()
            .map(|l| match l {
                None => Class::Neutral,
                Some(ids) => classify(&ids, &frames, &cache),
            })
            .collect(),
    )
}

/// The class of a line inside the frames `ids` (outermost first).
fn classify(ids: &[usize], frames: &[(Option<String>, bool)], cache: &[bool]) -> Class {
    let key = |i: usize| ids.get(i).and_then(|id| frames[*id].0.as_deref());
    match key(0) {
        Some("on" | "true" | "concurrency") => return Class::Scheduling,
        Some("jobs") if key(2) == Some("concurrency") => return Class::Scheduling,
        _ => {}
    }
    if ids.iter().any(|id| frames[*id].1 && cache[*id]) {
        return Class::Scheduling;
    }
    Class::Counts
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `.github/workflows/ci.yml` before PR 473 (4cefa34), the part it changed.
    const BEFORE_473: &str = "name: CI
on: [push, pull_request, merge_group]
jobs:
  tests:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      # rustup installs the toolchain of rust-toolchain.toml on the first cargo call.
      - run: cargo test --locked
";

    /// The same after PR 473: triggers, concurrency and a cache step.
    const AFTER_473: &str = "name: CI
on:
  push:
    branches: [main]
  pull_request:
  merge_group:
# One group per pull request: a newer head cancels the older head's run.
concurrency:
  group: ${{ github.workflow }}-${{ github.event_name }}
  cancel-in-progress: ${{ github.event_name == 'pull_request' }}
jobs:
  tests:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      # The Rust build cache, one per job (#471).
      - uses: actions/cache@v4
        with:
          path: |
            ~/.cargo/registry
            target
          key: ${{ runner.os }}-cargo-${{ github.job }}
          restore-keys: |
            ${{ runner.os }}-cargo-
      # rustup installs the toolchain of rust-toolchain.toml on the first cargo call.
      - run: cargo test --locked
";

    /// The real-base job before PR 483 (3bb84aa), the part it changed.
    const BEFORE_483: &str = "jobs:
  real-base:
    # wire checks. One test thread:
    # the fixtures share one session per process.
    runs-on: ubuntu-latest
    steps:
      - name: Suite on the real base
        run: |
          export BASE_BIN=\"$HOME/.makiba-base/bin\"
          MAKIBA_TEST_REAL_BASE=1 cargo test --locked -- --test-threads=1
";

    const AFTER_483: &str = "jobs:
  real-base:
    # wire checks. Four test threads: each fixture gets a
    # session of its own.
    runs-on: ubuntu-latest
    steps:
      - name: Suite on the real base
        run: |
          export BASE_BIN=\"$HOME/.makiba-base/bin\"
          MAKIBA_TEST_REAL_BASE=1 cargo test --locked -- --test-threads=4
";

    /// The changed lines of `old` and `new` by a line diff of their own (an LCS), 1-based.
    fn changed(old: &str, new: &str) -> (Vec<usize>, Vec<usize>) {
        let a: Vec<&str> = old.lines().collect();
        let b: Vec<&str> = new.lines().collect();
        let mut l = vec![vec![0usize; b.len() + 1]; a.len() + 1];
        for i in (0..a.len()).rev() {
            for j in (0..b.len()).rev() {
                l[i][j] = if a[i] == b[j] {
                    l[i + 1][j + 1] + 1
                } else {
                    l[i + 1][j].max(l[i][j + 1])
                };
            }
        }
        let (mut i, mut j, mut removed, mut added) = (0, 0, Vec::new(), Vec::new());
        while i < a.len() || j < b.len() {
            if i < a.len() && j < b.len() && a[i] == b[j] {
                i += 1;
                j += 1;
            } else if j < b.len() && (i == a.len() || l[i][j + 1] >= l[i + 1][j]) {
                added.push(j + 1);
                j += 1;
            } else {
                removed.push(i + 1);
                i += 1;
            }
        }
        (removed, added)
    }

    fn only_scheduling(old: &str, new: &str) -> bool {
        let (r, a) = changed(old, new);
        assert!(!(r.is_empty() && a.is_empty()), "no change");
        scheduling_only(old, new, &r, &a)
    }

    #[test]
    fn the_paths_are_one_list() {
        assert!(checked("tests/common/mod.rs"));
        assert!(checked("tools/check_language.py"));
        assert!(checked(".github/workflows/ci.yml"));
        assert!(!checked("tests/other.rs"));
        assert!(!checked("src/daemon/prwatch.rs"));
        assert!(!checked("tests/commonplace.rs"));
    }

    #[test]
    fn pr_473_changes_only_how_runs_are_scheduled() {
        assert!(only_scheduling(BEFORE_473, AFTER_473));
    }

    #[test]
    fn pr_483_changes_what_a_check_runs() {
        assert!(!only_scheduling(BEFORE_483, AFTER_483));
    }

    #[test]
    fn a_comment_between_steps_is_neutral_but_not_inside_a_run_scalar() {
        let base = "jobs:\n  t:\n    steps:\n      # one\n      - run: |\n          echo a\n          # data\n\n          echo b\n";
        let comment = base.replace("      # one", "      # two");
        assert!(only_scheduling(base, &comment));
        let inside = base.replace("          # data", "          # other data");
        assert!(!only_scheduling(base, &inside));
        let blank = base.replace("\n\n          echo b", "\n          echo b");
        assert!(!only_scheduling(base, &blank));
    }

    #[test]
    fn a_cache_steps_key_is_scheduling_and_a_new_job_is_not() {
        let base = "jobs:\n  t:\n    steps:\n      - uses: actions/cache@v4\n        with:\n          key: a\n      - run: cargo test\n";
        assert!(only_scheduling(base, &base.replace("key: a", "key: b")));
        let job = format!("{base}  u:\n    steps:\n      - run: cargo clippy\n");
        assert!(!only_scheduling(base, &job));
        let run = base.replace("cargo test", "cargo test --locked");
        assert!(!only_scheduling(base, &run));
    }

    #[test]
    fn a_jobs_concurrency_and_the_triggers_are_scheduling() {
        let base = "on: [push]\njobs:\n  t:\n    concurrency: a\n    steps:\n      - run: x\n";
        assert!(only_scheduling(
            base,
            &base.replace("concurrency: a", "concurrency: b")
        ));
        assert!(only_scheduling(
            base,
            &base.replace("[push]", "[push, pull_request]")
        ));
        assert!(only_scheduling(
            base,
            &base.replace("    concurrency: a\n", "")
        ));
    }

    #[test]
    fn what_cannot_be_read_counts() {
        let base = "on: [push]\njobs:\n  t:\n    steps:\n      - run: x\n";
        let anchor = base.replace("on: [push]", "on: &trig [push]");
        assert!(!only_scheduling(base, &anchor));
        let flow = base.replace("on: [push]", "on: [push,\n  pull_request]");
        assert!(!only_scheduling(base, &flow));
        let tab = base.replace("      - run: x", "\t- run: x");
        assert!(!only_scheduling(base, &tab));
        let doc = format!("{base}---\non: [push]\n");
        assert!(!only_scheduling(base, &doc));
    }

    #[test]
    fn a_hunk_header_gives_its_lines() {
        let diff = "diff --git a/x b/x\n@@ -2 +2,5 @@\n-a\n+b\n@@ -10,0 +14,2 @@\n+c\n+d\n";
        let (r, a) = hunk_lines(diff);
        assert_eq!(r, vec![2]);
        assert_eq!(a, vec![2, 3, 4, 5, 6, 14, 15]);
    }
}
