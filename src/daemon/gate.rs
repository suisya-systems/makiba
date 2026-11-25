//! The acceptance gate behind the door verb `gate` (Issue 47), ported from claude-org-ja's
//! `tools/acceptance_gate.py` and the deciding half of `tools/acceptance_merge_hook.py`.
//!
//! It runs no test. It judges from git and from the CI records of the PR's own repository, so a
//! project of any language can use it. A PR passes only when all hold:
//!
//! (a) the protected tree entries at head equal those at the commit that added the acceptance test
//!     (the manifest commit M), and M is the PR's first own commit (its parent is in the base and
//!     every other commit of `base..head` comes after it). Merging the base after M keeps this, and
//!     a protected path M left alone that head holds as the base does counts as a change from the
//!     base, not from the PR. M may change only what `scripts/acceptance-check.sh` lets a test
//!     commit change (its declared paths and test helpers: one rule, run from the built-in copy of
//!     that script, Issue 361), and every path M changes is protected;
//! (b) either the base names one check in `.github/acceptance-check`, `.github/workflows/` at head
//!     equals the base, and every run of that check at head succeeded; or before the fix the test
//!     failed: a failed `ci_completed` at M is recorded, the same CI job (app, check name, workflow)
//!     concluded `failure` at M and `success` at head, and the runs of M and of head were made
//!     against one and the same base.
//!
//! Before it, the PR's head must be the head asked for and the latest `ci_completed` of the PR must
//! be `passed` at that head. A PR without a manifest is `no_manifest`; the merge approval mode
//! decides it (`human` lets it through, `acceptance` only when the change is docs-only).
//!
//! What it does not prove: that the declared test itself ran in CI. A change outside the protected
//! paths can drop it from discovery. And it is not the bound: GitHub's branch protection is.
//!
//! Edited tests (Issue 282): a test that was there before the test commit, is not declared and
//! differs at head is reported (`edited_tests`); under `acceptance` such a pull request is refused,
//! under `human` the person is asked. A test kept outside a folder `tests` is not seen.
//!
//! Merge order (Issue 331, docs/design/merge-order.md): a pull request whose own changes touch
//! `landing::CHECKS` may add or tighten a check over `tests/`; `merge_order` reads, git only,
//! which other open pull requests have a test commit (whose declared tests cannot change), and the
//! door holds it while there are any.
//!
//! Pin: the first verdict that saw a manifest records a digest of the declared files and of the
//! other paths the test commit changed (its test helpers); a later
//! verdict for the same PR whose digest differs, or that finds no manifest, is rejected.

use super::gh::{self, GhError};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;

pub const TRAILER: &str = "Acceptance-Test";
const INFRA_NAMES: &[&str] = &[
    "conftest.py",
    "pytest.ini",
    ".pytest.ini",
    "pytest.toml",
    ".pytest.toml",
    "pyproject.toml",
    "setup.cfg",
    "tox.ini",
    "package.json",
];
// ponytail: the carried list; Rust's Cargo.toml / Cargo.lock are not protected (owner to decide).
const INFRA_PREFIXES: &[&str] = &["vitest.config.", "vitest.workspace.", "vite.config."];
const WORKFLOWS: &str = ".github/workflows/";
const REPO_CHECK: &str = ".github/acceptance-check";
/// The mode's setting until `config` stores it (spec section 2).
pub const MODE_ENV: &str = "MAKIBA_MERGE_APPROVAL_MODE";

/// A refusal with its reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reject(pub String);

fn reject<T>(why: impl Into<String>) -> Result<T, Reject> {
    Err(Reject(why.into()))
}

/// Why a read of the gate gave no answer (Issue 512): a refusal on what was read, or a read of
/// GitHub that failed (gh's non-zero exit, a timeout, no program, a failed fetch), which says
/// nothing about the pull request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fault {
    Refused(String),
    Unavailable(String),
}

impl From<Reject> for Fault {
    fn from(r: Reject) -> Fault {
        Fault::Refused(r.0)
    }
}

impl From<Fault> for Reject {
    fn from(f: Fault) -> Reject {
        match f {
            Fault::Refused(why) | Fault::Unavailable(why) => Reject(why),
        }
    }
}

/// The most tries of one read of GitHub (Issue 512).
const READ_TRIES: u32 = 3;
/// No new try of a read starts this long after its first began: a timed-out read is not tripled.
const READ_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);
/// The wait between two tries, unless `$MAKIBA_GATE_READ_WAIT_MS` (the test seam) says otherwise.
const READ_WAIT_MS: u64 = 5000;
pub const READ_WAIT_ENV: &str = "MAKIBA_GATE_READ_WAIT_MS";

/// One try of a read: its answer, a fault that no other try changes, or a failed read of GitHub
/// that may answer on another try.
enum Try<T> {
    Done(Result<T, Fault>),
    Again(String),
}

/// A read of GitHub tried up to `READ_TRIES` times, `$MAKIBA_GATE_READ_WAIT_MS` apart, each try
/// starting before `READ_WINDOW` has passed since the first began; a read that still fails is
/// `Fault::Unavailable` with its last message and the number of tries.
fn with_tries<T>(read: impl FnMut() -> Try<T>) -> Result<T, Fault> {
    let wait = std::env::var(READ_WAIT_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(READ_WAIT_MS);
    let started = std::time::Instant::now();
    with_tries_in(
        std::time::Duration::from_millis(wait),
        READ_WINDOW,
        || started.elapsed(),
        std::thread::sleep,
        read,
    )
}

/// `with_tries` with its wait, window, clock (the time since the first try began) and sleep: no
/// try is started once `window` has passed, so a wait that would end after it ends the read
/// instead, and the window is read again after the wait (a sleep may last longer than asked: a
/// busy host, a suspended one).
fn with_tries_in<T>(
    wait: std::time::Duration,
    window: std::time::Duration,
    elapsed: impl Fn() -> std::time::Duration,
    sleep: impl Fn(std::time::Duration),
    mut read: impl FnMut() -> Try<T>,
) -> Result<T, Fault> {
    let mut tries = 0;
    loop {
        tries += 1;
        let why = match read() {
            Try::Done(r) => return r,
            Try::Again(why) => why,
        };
        if tries < READ_TRIES && elapsed() + wait < window {
            sleep(wait);
            if elapsed() < window {
                continue;
            }
        }
        let n = if tries == 1 {
            "1 try".to_string()
        } else {
            format!("{tries} tries")
        };
        return Err(Fault::Unavailable(format!("{why} (after {n})")));
    }
}

/// A failed gh call as one line: its message, and the first line of what it wrote on stderr (the
/// error GitHub answered); and whether another try may answer.
fn gh_failure(program: &str, e: GhError) -> (String, bool) {
    match e {
        GhError::Missing => (format!("no program {program}"), false),
        GhError::Cancelled => ("cancelled".into(), false),
        GhError::Failed(f) => {
            let first = f.stderr.lines().map(str::trim).find(|l| !l.is_empty());
            match first {
                Some(l) => (format!("{}: {l}", f.message), true),
                None => (f.message, true),
            }
        }
    }
}

/// One try of a gh read: a failure that another try may change is `Again`, one that it will not
/// (no program, cancelled) is unavailable at once.
fn gh_try(program: &str, args: &[&str], what: &str) -> Try<String> {
    let never = AtomicBool::new(false);
    match gh::run_gh(program, args, &never) {
        Ok(out) => Try::Done(Ok(out)),
        Err(e) => match gh_failure(program, e) {
            (why, true) => Try::Again(format!("{what}: {why}")),
            (why, false) => Try::Done(Err(Fault::Unavailable(format!("{what}: {why}")))),
        },
    }
}

/// The merge approval mode, and a warning for a value that is neither: `human` unless the setting
/// says `acceptance` (as `tools/merge_approval_mode.py`: trimmed, any case, the whole value).
pub fn mode_of(value: Option<&str>) -> (&'static str, Option<String>) {
    let Some(raw) = value else {
        return ("human", None);
    };
    match raw.trim().to_lowercase().as_str() {
        "human" => ("human", None),
        "acceptance" => ("acceptance", None),
        _ => (
            "human",
            Some(format!(
                "merge_approval_mode: invalid value {:?}, using human",
                raw.trim()
            )),
        ),
    }
}

/// The mode now, read from makibad's environment before every answer.
pub fn merge_approval_mode() -> &'static str {
    let value = std::env::var(MODE_ENV).ok();
    let (mode, warning) = mode_of(value.as_deref());
    if let Some(w) = warning {
        eprintln!("makibad: {w}");
    }
    mode
}

// ---- git -----------------------------------------------------------------------------------------

/// No git the gate runs in the clone runs a hook (Issue 236).
const HOOKS_OFF: &str = super::run_repo::HOOKS_OFF;

fn git(repo: &Path, args: &[&str]) -> Result<String, Reject> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["-c", HOOKS_OFF])
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| Reject(format!("cannot run git: {e}")))?;
    if !out.status.success() {
        return reject(format!(
            "git failed: {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn git_ok(repo: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["-c", HOOKS_OFF])
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn is_infra(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    INFRA_NAMES.contains(&name)
        || INFRA_PREFIXES.iter().any(|p| name.starts_with(p))
        || path.starts_with(WORKFLOWS)
}

fn under(path: &str, p: &str) -> bool {
    path == p || path.starts_with(&format!("{p}/"))
}

fn is_protected(path: &str, manifest: &[String]) -> bool {
    is_infra(path) || manifest.iter().any(|p| under(path, p))
}

type Tree = BTreeMap<String, (String, String)>;

/// path -> (mode, object) of every entry of the commit's tree.
fn tree(repo: &Path, sha: &str) -> Result<Tree, Reject> {
    let mut out = Tree::new();
    for rec in git(repo, &["ls-tree", "-r", "-z", "--full-tree", sha])?.split('\0') {
        let Some((meta, path)) = rec.split_once('\t') else {
            continue;
        };
        let f: Vec<&str> = meta.split_whitespace().collect();
        if f.len() == 3 {
            out.insert(path.to_string(), (f[0].to_string(), f[2].to_string()));
        }
    }
    Ok(out)
}

/// The one commit of `base..head` that declares the test, with its paths; `None` when none does.
pub fn find_manifest(
    repo: &Path,
    base: &str,
    head: &str,
) -> Result<Option<(String, Vec<String>)>, Reject> {
    let format = format!("--format=%H%x00%(trailers:key={TRAILER},valueonly,separator=%x1f)%x1e");
    let log = git(repo, &["log", &format, &format!("{base}..{head}")])?;
    let mut found = Vec::new();
    for rec in log.split('\u{1e}') {
        let (sha, values) = rec.trim().split_once('\0').unwrap_or((rec.trim(), ""));
        let paths: Vec<String> = values
            .split('\u{1f}')
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(String::from)
            .collect();
        if !sha.is_empty() && !paths.is_empty() {
            found.push((sha.to_string(), paths));
        }
    }
    if found.len() > 1 {
        let shas: Vec<&str> = found.iter().map(|(s, _)| &s[..12]).collect();
        return reject(format!(
            "more than one commit declares {TRAILER} ({}): the manifest was rewritten",
            shas.join(", ")
        ));
    }
    let Some((sha, paths)) = found.pop() else {
        return Ok(None);
    };
    let mut clean = Vec::new();
    for p in paths {
        let parts: Vec<&str> = p.split('/').collect();
        if p.starts_with('/') || parts.contains(&"..") || p.contains('\\') || p.starts_with('-') {
            return reject(format!("invalid manifest path {p:?}"));
        }
        let norm: Vec<&str> = parts
            .into_iter()
            .filter(|s| !s.is_empty() && *s != ".")
            .collect();
        clean.push(norm.join("/"));
    }
    Ok(Some((sha, clean)))
}

/// A commit of `base..head` with an `Acceptance-Test:` line that git does not read as a trailer of
/// that key (a blank line inside the trailer block, as on pull request 308): its id. Such a
/// branch would declare no test, and CI would print `ACCEPTANCE: none`. A valid trailer does not
/// cover a second such line outside the trailer block: git reads only the valid one, and the
/// manifest would leave the other test unprotected, so more lines than values read name the commit.
pub fn stray_trailer(repo: &Path, base: &str, head: &str) -> Result<Option<String>, Reject> {
    let format =
        format!("--format=%H%x00%(trailers:key={TRAILER},valueonly,separator=%x1f)%x00%B%x1e");
    let log = git(repo, &["log", &format, &format!("{base}..{head}")])?;
    let key = format!("{}:", TRAILER.to_lowercase());
    for rec in log.split('\u{1e}') {
        let mut parts = rec.trim_start().splitn(3, '\0');
        let (Some(sha), Some(read), Some(body)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let lines = body
            .lines()
            .filter(|l| l.trim_start().to_lowercase().starts_with(&key))
            .count();
        let values = read
            .split('\u{1f}')
            .filter(|v| !v.trim().is_empty())
            .count();
        if lines > values {
            return Ok(Some(sha.to_string()));
        }
    }
    Ok(None)
}

/// Issue 307: whether `head` may be pushed or recorded before CI. Refused when a commit of
/// `base..head` has an `Acceptance-Test:` line git does not read as a trailer, and when the branch
/// declares a test in a commit M other than `head` with no failed CI run recorded at M (`failed`;
/// `scope` says where it was looked for). The merge gate takes a test-first proof only from that
/// failed run, and the order cannot be repaired once a later head is at origin.
pub fn before_ci(
    repo: &Path,
    base: &str,
    head: &str,
    failed: &dyn Fn(&str) -> bool,
    scope: &str,
) -> Result<(), Reject> {
    if let Some(sha) = stray_trailer(repo, base, head)? {
        return reject(format!(
            "commit {} has an {TRAILER} line that git does not read as a trailer (a blank line inside the trailer block, such as one before Co-Authored-By, or a line outside the block?), so the branch would not declare every test it names: fix the message before it is pushed (git interpret-trailers --parse shows what git reads). Line L-K9: a merge needs CI on the exact head and the acceptance gate",
            &sha[..sha.len().min(12)]
        ));
    }
    let Some((m, paths)) = find_manifest(repo, base, head)? else {
        return Ok(());
    };
    if m == head || failed(&m) {
        return Ok(());
    }
    reject(format!(
        "the branch declares its acceptance test in commit {} ({TRAILER}: {}), and no failed CI run is recorded at it {scope}: push the test commit alone first (push with head {m}), open or record the pull request at it and let CI fail there; then push the rest. Line L-K9: a merge needs CI on the exact head and the acceptance gate, which takes a test-first proof only from a failed CI run at the test commit",
        &m[..12],
        paths.join(", ")
    ))
}

/// The acceptance checker of makiba's own CI, built in: the one place of the rule of what a test
/// commit may change (Issue 361). The gate runs its `--test-commit` mode, never restates it.
const CHECKER: &str = include_str!("../../scripts/acceptance-check.sh");

/// The paths the test commit `msha` may not change, by the rule of `scripts/acceptance-check.sh`
/// (docs/acceptance-check.md, rule 2): every path it changes that is neither under a path it
/// declares nor a test helper, sorted. The checker runs in `repo` with git hooks off.
pub fn test_commit_refusals(repo: &Path, msha: &str) -> Result<Vec<String>, Reject> {
    let (key, value) = HOOKS_OFF.split_once('=').unwrap_or((HOOKS_OFF, ""));
    let out = Command::new("bash")
        .args(["-c", CHECKER, "acceptance-check", "--test-commit", msha])
        .current_dir(repo)
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", key)
        .env("GIT_CONFIG_VALUE_0", value)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| Reject(format!("cannot run bash for the acceptance checker: {e}")))?;
    match out.status.code() {
        Some(0) => Ok(Vec::new()),
        Some(1) => {
            // An empty line is kept: a name made only of newlines prints as one.
            let paths: BTreeSet<String> = String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(String::from)
                .collect();
            if paths.is_empty() {
                return reject(format!(
                    "the acceptance checker refused the test commit {} but named no path",
                    &msha[..msha.len().min(12)]
                ));
            }
            Ok(paths.into_iter().collect())
        }
        _ => reject(format!(
            "the acceptance checker could not judge the test commit {}: {}",
            &msha[..msha.len().min(12)],
            String::from_utf8_lossy(&out.stderr).trim()
        )),
    }
}

/// Every path the commit changes against its parent.
fn changed_by(repo: &Path, sha: &str) -> Result<BTreeSet<String>, Reject> {
    Ok(git(
        repo,
        &[
            "diff",
            "--no-renames",
            "--name-only",
            "-z",
            &format!("{sha}^"),
            sha,
        ],
    )?
    .split('\0')
    .filter(|p| !p.is_empty())
    .map(String::from)
    .collect())
}

/// Condition (a): the protected entries are those of the manifest commit.
fn check_protected(
    repo: &Path,
    msha: &str,
    base: &str,
    head: &str,
    manifest: &[String],
) -> Result<(), Reject> {
    let short = &msha[..12];
    if git(repo, &["rev-list", "--parents", "-n1", msha])?
        .split_whitespace()
        .count()
        != 2
    {
        return reject(format!(
            "manifest commit {short} must have exactly one parent"
        ));
    }
    let mtree = tree(repo, msha)?;
    for p in manifest {
        if !mtree.keys().any(|q| under(q, p)) {
            return reject(format!("manifest path {p:?} does not exist in {short}"));
        }
    }
    let undeclared = test_commit_refusals(repo, msha)?;
    if !undeclared.is_empty() {
        return reject(format!(
            "manifest commit {short} also changes undeclared paths {undeclared:?}: declare them with {TRAILER} or move them to a later commit"
        ));
    }
    // What the test commit changed (its declared paths and test helpers) stays as it left them.
    let touched = changed_by(repo, msha)?;
    let prot = |t: &Tree| -> Tree {
        t.iter()
            .filter(|(p, _)| is_protected(p, manifest) || touched.contains(*p))
            .map(|(p, e)| (p.clone(), e.clone()))
            .collect()
    };
    let pm = prot(&mtree);
    for (p, (mode, _)) in &pm {
        if mode == "120000" || mode == "160000" {
            return reject(format!("protected path {p:?} is a symlink or submodule"));
        }
    }
    let parent = format!("{msha}^");
    let ph = prot(&tree(repo, head)?);
    let (pp, pb) = (tree(repo, &parent)?, tree(repo, base)?);
    let keys: BTreeSet<&String> = pm.keys().chain(ph.keys()).collect();
    let changed: Vec<&str> = keys
        .into_iter()
        .filter(|p| {
            pm.get(*p) != ph.get(*p) && !(pm.get(*p) == pp.get(*p) && ph.get(*p) == pb.get(*p))
        })
        .map(String::as_str)
        .collect();
    if !changed.is_empty() {
        return reject(format!(
            "protected paths changed after the test commit {short}: {changed:?} (rebase so the test commit sits on the new base if the change came from base)"
        ));
    }
    Ok(())
}

/// M is the PR's first own commit: its parent is in the base, every other commit comes after it.
fn check_first(repo: &Path, msha: &str, base: &str, head: &str) -> Result<(), Reject> {
    let parent = git(repo, &["rev-parse", &format!("{msha}^")])?
        .trim()
        .to_string();
    let own: BTreeSet<String> = git(repo, &["rev-list", &format!("{base}..{head}")])?
        .split_whitespace()
        .map(String::from)
        .collect();
    let after: BTreeSet<String> = git(
        repo,
        &["rev-list", "--ancestry-path", &format!("{msha}..{head}")],
    )?
    .split_whitespace()
    .map(String::from)
    .collect();
    let in_base = git_ok(repo, &["merge-base", "--is-ancestor", &parent, base]);
    if !in_base || own.iter().any(|c| !after.contains(c) && c != msha) {
        return reject(format!(
            "the test commit {} must be the first commit of the PR (its parent {} is not in the base, or a commit of the PR does not come after it): put the test commit first; merge only the base, after the test commit",
            &msha[..12],
            &parent[..parent.len().min(12)]
        ));
    }
    Ok(())
}

/// The check named in `.github/acceptance-check` at base, or `None` without the file.
fn repo_check(repo: &Path, base: &str) -> Result<Option<String>, Reject> {
    let Ok(text) = git(repo, &["show", &format!("{base}:{REPO_CHECK}")]) else {
        return Ok(None);
    };
    let names: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if names.len() != 1 {
        return reject(format!(
            "{REPO_CHECK} at base must name exactly one check, found {names:?}"
        ));
    }
    Ok(Some(names[0].to_string()))
}

/// sha256 of the declared paths and their (mode, blob) in the manifest commit, and of the other
/// paths it changed.
fn manifest_digest(repo: &Path, msha: &str, manifest: &[String]) -> Result<String, Reject> {
    let mtree = tree(repo, msha)?;
    let entries: Vec<Value> = mtree
        .iter()
        .filter(|(p, _)| manifest.iter().any(|m| under(p, m)))
        .map(|(p, (mode, obj))| json!([p, mode, obj]))
        .collect();
    let mut sorted = manifest.to_vec();
    sorted.sort();
    // The other paths the test commit changed (its test helpers, Issue 361) are pinned too, a
    // deleted one as null; a test commit that changes only declared paths keeps its old digest.
    let others: Vec<Value> = changed_by(repo, msha)?
        .into_iter()
        .filter(|p| !manifest.iter().any(|m| under(p, m)))
        .map(|p| match mtree.get(&p) {
            Some((mode, obj)) => json!([p, mode, obj]),
            None => json!([p, null]),
        })
        .collect();
    let text = if others.is_empty() {
        json!([sorted, entries]).to_string()
    } else {
        json!([sorted, entries, others]).to_string()
    };
    Ok(Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Every path `base...head` changes is Markdown or under `docs/` (and there is at least one).
pub fn docs_only(repo: &Path, base: &str, head: &str) -> bool {
    // --no-renames: a rename lists its source, so code moved into docs/ is not docs-only.
    // ponytail: git quotes unusual names ("docs/\346..."), which fail the check -> refused.
    match git(
        repo,
        &[
            "diff",
            "--no-renames",
            "--name-only",
            &format!("{base}...{head}"),
        ],
    ) {
        Ok(out) => {
            let paths: Vec<&str> = out.lines().collect();
            !paths.is_empty()
                && paths
                    .iter()
                    .all(|p| p.ends_with(".md") || p.starts_with("docs/"))
        }
        Err(_) => false,
    }
}

/// The own changes of `a` and of `b` against `base` are byte-equal (Issue 263): what each adds to
/// the merge base it has with `base`, blob ids and modes included, no text conversion, renames or
/// hidden submodule change. A join commit of the base into `a`, or a rebase, keeps them; a failed
/// git (an id the clone does not hold) is not equal.
pub fn same_changes(repo: &Path, base: &str, a: &str, b: &str) -> bool {
    let own = |x: &str| -> Option<String> {
        let mb = git(repo, &["merge-base", base, x]).ok()?;
        git(
            repo,
            &[
                "diff",
                "--binary",
                "--no-textconv",
                "--no-ext-diff",
                "--no-renames",
                "--ignore-submodules=none",
                mb.trim(),
                x,
            ],
        )
        .ok()
    };
    matches!((own(a), own(b)), (Some(x), Some(y)) if x == y)
}

/// `head`'s only new commit over `prev` is a clean merge of the base (Issue 358, the fix budget):
/// `head` has two parents, `prev` and a commit of `base`, and its tree is the one git's own merge
/// of those two writes with no conflict (what `update` pushes). A merge that resolved a conflict
/// or carries any other edit is not; a failed git is not.
pub fn pushed_base_merge(repo: &Path, base: &str, prev: &str, head: &str) -> bool {
    let Ok(line) = git(repo, &["rev-list", "--parents", "-n", "1", head]) else {
        return false;
    };
    let ids: Vec<&str> = line.split_whitespace().collect();
    let [_, a, b] = ids[..] else {
        return false;
    };
    let other = match (a == prev, b == prev) {
        (true, false) => b,
        (false, true) => a,
        _ => return false,
    };
    if !git_ok(repo, &["merge-base", "--is-ancestor", other, base]) {
        return false;
    }
    let merged = git(repo, &["merge-tree", "--write-tree", prev, other]);
    let tree = git(repo, &["rev-parse", &format!("{head}^{{tree}}")]);
    match (merged, tree) {
        (Ok(m), Ok(t)) => m.lines().next().map(str::trim) == Some(t.trim()),
        _ => false,
    }
}

/// The commits after the head `read` that a diff review read, up to `head` (Issue 344): `(all,
/// not_merges)`, oldest first. `all` is `read..head` less the commits of the base (which a
/// merge of the base brings), or `base..head` when `read` is not an ancestor of `head` (a rebase,
/// or a head the clone lacks). `not_merges` are those of `all` that
/// are not a clean merge of the base: a commit with two parents, one of them in `base`, whose tree
/// is the one `git merge-tree --write-tree` makes of its parents with no conflict. A git that fails
/// is an error, never "only merges".
pub fn after_review(
    repo: &Path,
    base: &str,
    read: &str,
    head: &str,
) -> Result<(Vec<String>, Vec<String>), Reject> {
    let range = if git_ok(
        repo,
        &[
            "merge-base",
            "--is-ancestor",
            &format!("{read}^{{commit}}"),
            head,
        ],
    ) {
        format!("^{read}")
    } else {
        format!("^{base}")
    };
    let mut all = Vec::new();
    let mut not_merges = Vec::new();
    for line in git(
        repo,
        &[
            "rev-list",
            "--reverse",
            "--parents",
            head,
            &range,
            &format!("^{base}"),
        ],
    )?
    .lines()
    {
        let ids: Vec<&str> = line.split_whitespace().collect();
        let Some((c, parents)) = ids.split_first() else {
            continue;
        };
        all.push(c.to_string());
        if !clean_base_merge(repo, base, c, parents)? {
            not_merges.push(c.to_string());
        }
    }
    Ok((all, not_merges))
}

/// Commit `c` with `parents` merges the base in and resolves nothing (see `after_review`).
fn clean_base_merge(repo: &Path, base: &str, c: &str, parents: &[&str]) -> Result<bool, Reject> {
    let [p1, p2] = parents else {
        return Ok(false);
    };
    let in_base = |p: &str| git_ok(repo, &["merge-base", "--is-ancestor", p, base]);
    if !in_base(p1) && !in_base(p2) {
        return Ok(false);
    }
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["-c", HOOKS_OFF, "merge-tree", "--write-tree", p1, p2])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| Reject(format!("cannot run git: {e}")))?;
    match out.status.code() {
        Some(0) => {}
        // A conflict: the commit resolved one.
        Some(1) => return Ok(false),
        _ => return reject(format!("git merge-tree failed for {p1} {p2}")),
    }
    let made = String::from_utf8_lossy(&out.stdout);
    let tree = git(repo, &["rev-parse", &format!("{c}^{{tree}}")])?;
    Ok(made.lines().next().map(str::trim) == Some(tree.trim()))
}

// ---- GitHub --------------------------------------------------------------------------------------

/// One JSON value per line of `gh api --paginate <path> --jq <jq>`. A failed read is tried again
/// (Issue 512) and, if it still fails, is `Fault::Unavailable`; an answer that is not JSON is
/// `Fault::Refused`.
pub fn gh_lines_read(program: &str, path: &str, jq: &str) -> Result<Vec<Value>, Fault> {
    let what = format!("cannot read {path} from GitHub");
    let out = with_tries(|| gh_try(program, &["api", "--paginate", path, "--jq", jq], &what))?;
    out.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).map_err(|e| Fault::Refused(format!("{what}: {e}"))))
        .collect()
}

/// `gh_lines_read`, either fault as a `Reject`.
pub fn gh_lines(program: &str, path: &str, jq: &str) -> Result<Vec<Value>, Reject> {
    gh_lines_read(program, path, jq).map_err(Reject::from)
}

/// (check suite, workflow, base SHAs) of the latest `pull_request` run per workflow at `sha`.
#[derive(Default)]
pub struct Ci {
    program: String,
    runs: BTreeMap<String, Vec<(i64, i64, Vec<String>)>>,
    /// A read of GitHub that still failed after its tries (Issue 512): the refusal it caused is
    /// not one on the pull request.
    unavailable: Option<String>,
}

impl Ci {
    pub fn new(program: &str) -> Ci {
        Ci {
            program: program.to_string(),
            ..Default::default()
        }
    }

    /// The message of a read of GitHub that still failed after its tries, if one did.
    pub fn unavailable(&self) -> Option<&str> {
        self.unavailable.as_deref()
    }

    /// `gh_lines_read`, keeping a read that stayed unavailable.
    fn lines(&mut self, path: &str, jq: &str) -> Result<Vec<Value>, Reject> {
        gh_lines_read(&self.program, path, jq).map_err(|f| {
            if let Fault::Unavailable(why) = &f {
                self.unavailable = Some(why.clone());
            }
            Reject::from(f)
        })
    }

    /// The latest run per workflow; a latest run that names no base is dropped, not replaced.
    pub fn pr_runs(
        &mut self,
        repo: &str,
        sha: &str,
    ) -> Result<Vec<(i64, i64, Vec<String>)>, Reject> {
        if let Some(r) = self.runs.get(sha) {
            return Ok(r.clone());
        }
        let mut latest: BTreeMap<i64, Value> = BTreeMap::new();
        for r in self.lines(
            &format!("repos/{repo}/actions/runs?head_sha={sha}&event=pull_request&per_page=100"),
            ".workflow_runs[] | {wf: .workflow_id, suite: .check_suite_id, created: .created_at, bases: [.pull_requests[].base.sha]}",
        )? {
            let wf = r["wf"].as_i64().unwrap_or_default();
            let newer = latest
                .get(&wf)
                .is_none_or(|old| r["created"].as_str() > old["created"].as_str());
            if newer {
                latest.insert(wf, r);
            }
        }
        let out: Vec<(i64, i64, Vec<String>)> = latest
            .into_values()
            .map(|r| {
                let bases = r["bases"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|b| b.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                (
                    r["suite"].as_i64().unwrap_or_default(),
                    r["wf"].as_i64().unwrap_or_default(),
                    bases,
                )
            })
            .filter(|(_, _, b): &(i64, i64, Vec<String>)| !b.is_empty())
            .collect();
        self.runs.insert(sha.to_string(), out.clone());
        Ok(out)
    }

    /// (app, check name, workflow) -> conclusion of the latest completed run at `sha`, among the
    /// checks of the runs `pr_runs` selected.
    pub fn check_runs(
        &mut self,
        repo: &str,
        sha: &str,
    ) -> Result<BTreeMap<(i64, String, i64), String>, Reject> {
        let workflow: BTreeMap<i64, i64> = self
            .pr_runs(repo, sha)?
            .into_iter()
            .map(|(suite, wf, _)| (suite, wf))
            .collect();
        let mut latest: BTreeMap<(i64, String, i64), (String, String)> = BTreeMap::new();
        // filter=all: the default keeps one run per name, and a push run can hide the PR's.
        for r in self.lines(
            &format!("repos/{repo}/commits/{sha}/check-runs?filter=all&per_page=100"),
            ".check_runs[] | {app: .app.id, name, conclusion, completed_at, suite: .check_suite.id}",
        )? {
            let suite = r["suite"].as_i64().unwrap_or(-1);
            let (Some(wf), Some(done)) = (workflow.get(&suite), r["completed_at"].as_str()) else {
                continue;
            };
            let key = (
                r["app"].as_i64().unwrap_or_default(),
                r["name"].as_str().unwrap_or_default().to_string(),
                *wf,
            );
            if latest.get(&key).is_none_or(|(at, _)| done > at.as_str()) {
                let c = r["conclusion"].as_str().unwrap_or_default().to_string();
                latest.insert(key, (done.to_string(), c));
            }
        }
        Ok(latest.into_iter().map(|(k, (_, c))| (k, c)).collect())
    }

    /// The base SHAs the runs `pr_runs` selected were made against.
    pub fn run_bases(&mut self, repo: &str, sha: &str) -> Result<BTreeSet<String>, Reject> {
        Ok(self
            .pr_runs(repo, sha)?
            .into_iter()
            .flat_map(|(_, _, b)| b)
            .collect())
    }
}

// ---- the verdict ---------------------------------------------------------------------------------

/// What the gate found; `result` is `passed`, `rejected` or `no_manifest`.
#[derive(Debug, Default, Clone)]
pub struct Verdict {
    pub result: String,
    pub reason: String,
    pub base: String,
    pub head: String,
    pub manifest_commit: Option<String>,
    pub paths: Vec<String>,
    pub manifest_digest: Option<String>,
    /// The tests that were there before the test commit, are not declared and differ at head
    /// (`edited_tests`); the door decides what they mean under the mode.
    pub edited_tests: Vec<String>,
    /// The merge order (Issue 331); the door decides what it means under the mode.
    pub merge_order: MergeOrder,
}

/// A file under a folder `tests`: what the gate reports when a pull request edits it without
/// declaring it. Tests kept elsewhere (inline unit tests) are not seen.
fn is_test(path: &str) -> bool {
    path.starts_with("tests/") || path.contains("/tests/")
}

/// The tests a pull request edited after its test commit (Issue 282, belt.md section 5 line 5):
/// every file under a folder `tests` that exists at `P` and is changed or gone at `head`, where
/// `P` is the parent of the test commit `msha`, or the merge base of `base` and `head` when no
/// test is declared. Left out: the declared paths and every other path the test commit changed,
/// such as a helper it extended (condition (a) holds them; Issue 361), and a file `head`
/// holds exactly as `base` does (the base brought that change, as in `check_protected`). A new
/// test is not an edited one. A moved test is reported at the path it left.
pub fn edited_tests(
    repo: &Path,
    base: &str,
    head: &str,
    msha: Option<&str>,
    manifest: &[String],
) -> Result<Vec<String>, Reject> {
    let (before, by_test_commit) = match msha {
        Some(m) => (
            git(repo, &["rev-parse", &format!("{m}^")])?,
            changed_by(repo, m)?,
        ),
        None => (git(repo, &["merge-base", base, head])?, BTreeSet::new()),
    };
    let (tp, th, tb) = (
        tree(repo, before.trim())?,
        tree(repo, head)?,
        tree(repo, base)?,
    );
    Ok(tp
        .iter()
        .filter(|(p, _)| {
            is_test(p) && !manifest.iter().any(|m| under(p, m)) && !by_test_commit.contains(*p)
        })
        .filter(|(p, e)| th.get(*p) != Some(*e) && th.get(*p) != tb.get(*p))
        .map(|(p, _)| p.clone())
        .collect())
}

// ---- the merge order (Issue 331) ---------------------------------------------------------------

/// The paths the PR's own changes (from the merge base of `base` and `head` to `head`) touch that
/// may add or tighten a check over `tests/` (`landing::CHECKS`); empty: it is not check-adding.
pub fn check_paths(repo: &Path, base: &str, head: &str) -> Result<Vec<String>, Reject> {
    let mb = git(repo, &["merge-base", base, head])?;
    Ok(git(
        repo,
        &["diff", "--no-renames", "--name-only", "-z", mb.trim(), head],
    )?
    .split('\0')
    .filter(|p| !p.is_empty() && super::landing::is_check_path(p))
    .map(String::from)
    .collect())
}

/// An open pull request whose test commit holds a check-adding one.
#[derive(Debug, Clone, PartialEq)]
pub struct Waiting {
    pub pr: i64,
    pub task: String,
    pub test_commit: String,
    pub declared: Vec<String>,
    /// Its own changes touch `landing::CHECKS` too: the two hold each other.
    pub check_adding: bool,
}

/// What the merge order found (docs/design/merge-order.md 3.2): empty `paths`, not check-adding.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct MergeOrder {
    pub paths: Vec<String>,
    pub waits_for: Vec<Waiting>,
    /// `(pr, task, why)` of an open pull request that could not be read: it holds as one waiting.
    pub not_read: Vec<(i64, String, String)>,
    /// A read of GitHub for another pull request that still failed after its tries (Issue 512):
    /// with nothing in `waits_for`, the order is not known, and the gate answers `unavailable`
    /// when no rule on the content refused the pull request.
    pub unavailable: Option<String>,
}

impl MergeOrder {
    pub fn holds(&self) -> bool {
        !self.waits_for.is_empty() || !self.not_read.is_empty()
    }
}

/// The merge order of the PR at `head` on `base` (both in `clone`, `base` the tip of
/// `base_branch`): its check paths and, when it has any, each other open pull request
/// `(task, pr)` of `repo` into the same branch with a test commit. Each other pull request's base
/// branch is read with `gh pr view` (`program`); one into another branch is not named, since the
/// check does not reach it there. Its head and base are fetched as the gate fetches its own, and
/// its test commit and check paths are read against its own fetched base. No project code runs.
pub fn merge_order(
    program: &str,
    clone: &Path,
    repo: &str,
    base: &str,
    base_branch: &str,
    head: &str,
    others: &[(String, i64)],
) -> Result<MergeOrder, Reject> {
    let mut m = MergeOrder {
        paths: check_paths(clone, base, head)?,
        ..Default::default()
    };
    if m.paths.is_empty() {
        return Ok(m);
    }
    for (task, pr) in others {
        let read = pr_view_read(program, repo, *pr).and_then(|(_, branch, _)| {
            if branch != base_branch {
                return Ok(None);
            }
            let (b, h) = fetch_read(clone, repo, *pr, &branch)?;
            Ok(Some((find_manifest(clone, &b, &h)?, b, h)))
        });
        match read {
            Ok(Some((Some((test_commit, declared)), b, h))) => m.waits_for.push(Waiting {
                pr: *pr,
                task: task.clone(),
                test_commit,
                declared,
                check_adding: check_paths(clone, &b, &h).is_ok_and(|p| !p.is_empty()),
            }),
            Ok(_) => {}
            Err(Fault::Refused(why)) => m.not_read.push((*pr, task.clone(), why)),
            Err(Fault::Unavailable(why)) => {
                m.unavailable = Some(format!("#{pr}: {why}"));
                m.not_read.push((*pr, task.clone(), why));
            }
        }
    }
    Ok(m)
}

/// The first earlier verdict for the PR that saw a manifest: its commit, paths and digest.
#[derive(Debug, Clone, PartialEq)]
pub struct Pin {
    pub id: i64,
    pub commit: String,
    pub paths: Value,
    pub digest: String,
}

/// What the gate needs from `state.db`: whether a failed `ci_completed` is recorded at a commit.
pub type FailedAt<'a> = &'a dyn Fn(&str) -> Result<bool, Reject>;

/// The git and CI checks of the gate, `base` and `head` being commit ids in `repo`.
#[allow(clippy::too_many_arguments)]
pub fn evaluate(
    repo: &Path,
    base: &str,
    head: &str,
    slug: &str,
    pin: Option<&Pin>,
    ci: &mut Ci,
    failed_at: FailedAt,
    v: &mut Verdict,
) -> Result<(), Reject> {
    let Some((msha, manifest)) = find_manifest(repo, base, head)? else {
        if let Some(p) = pin {
            return reject(format!(
                "no commit declares {TRAILER} any more, but this PR had one ({}) when the gate first saw it: the test commit was removed - ask the person",
                &p.commit[..p.commit.len().min(12)]
            ));
        }
        v.result = "no_manifest".into();
        v.reason = format!("no commit in base..head has an {TRAILER} trailer");
        return Ok(());
    };
    let digest = manifest_digest(repo, &msha, &manifest)?;
    v.manifest_commit = Some(msha.clone());
    v.paths = manifest.clone();
    v.manifest_digest = Some(digest.clone());
    if let Some(p) = pin
        && p.digest != digest
    {
        return reject(format!(
            "the test commit was rewritten since the gate first saw it ({} -> {}, paths {} -> {:?}) - ask the person",
            &p.commit[..p.commit.len().min(12)],
            &msha[..12],
            p.paths,
            manifest
        ));
    }
    check_protected(repo, &msha, base, head, &manifest)?;
    check_first(repo, &msha, base, head)?;
    let check = repo_check(repo, base)?;
    // The job behind the name is the base's: neither the PR nor an older branch point may swap in
    // another one.
    let workflows = |sha: &str| -> Result<Tree, Reject> {
        Ok(tree(repo, sha)?
            .into_iter()
            .filter(|(p, _)| p.starts_with(WORKFLOWS))
            .collect())
    };
    if let Some(check) = &check
        && workflows(base)? == workflows(head)?
    {
        let at_head: Vec<String> = ci
            .check_runs(slug, head)?
            .into_iter()
            .filter(|(k, _)| &k.1 == check)
            .map(|(_, c)| c)
            .collect();
        if !at_head.is_empty() && at_head.iter().all(|c| c == "success") {
            v.result = "passed".into();
            v.reason = format!(
                "the repository's acceptance check {check:?} ({REPO_CHECK} at base) succeeded at head {}",
                &head[..12]
            );
            return Ok(());
        }
    }
    if !failed_at(&msha)? {
        return reject(format!(
            "no failed CI run recorded at the test commit {}: push the test commit alone first, record it with the pr tool and let CI fail on it",
            &msha[..12]
        ));
    }
    let (at_m, at_head) = (ci.check_runs(slug, &msha)?, ci.check_runs(slug, head)?);
    let flipped: Vec<&(i64, String, i64)> = at_m
        .iter()
        .filter(|(k, c)| *c == "failure" && at_head.get(*k).map(String::as_str) == Some("success"))
        .map(|(k, _)| k)
        .collect();
    if flipped.is_empty() {
        return reject(format!(
            "no CI job failed at the test commit and succeeded at head (test commit: {at_m:?}, head: {at_head:?})"
        ));
    }
    let (bases_m, bases_head) = (ci.run_bases(slug, &msha)?, ci.run_bases(slug, head)?);
    if bases_m.len() != 1 || bases_m != bases_head {
        return reject(format!(
            "the CI runs at the test commit and at head are not on one known base (different bases or unknown: {bases_m:?} vs {bases_head:?}): push the test commit alone again on the current base"
        ));
    }
    let jobs: Vec<&str> = flipped.iter().map(|k| k.1.as_str()).collect();
    v.result = "passed".into();
    v.reason = format!(
        "CI job(s) {} failed at the test commit {} and succeeded at head, on base {}",
        jobs.join(", "),
        &msha[..12],
        &bases_m.iter().next().map_or("", |b| &b[..b.len().min(12)])
    );
    Ok(())
}

// ---- the PR ------------------------------------------------------------------------------------

/// `gh pr view` of the PR: (url, base branch, head). A failed read is tried again (Issue 512) and,
/// if it still fails, is `Fault::Unavailable`; an answer that is not what was asked is
/// `Fault::Refused`.
pub fn pr_view_read(program: &str, repo: &str, pr: i64) -> Result<(String, String, String), Fault> {
    let n = pr.to_string();
    let args = [
        "pr",
        "view",
        &n,
        "--repo",
        repo,
        "--json",
        "number,url,baseRefName,headRefOid",
    ];
    let out = with_tries(|| gh_try(program, &args, "gh pr view failed"))?;
    let v: Value = serde_json::from_str(out.trim())
        .map_err(|e| Fault::Refused(format!("gh pr view answered no JSON: {e}")))?;
    let field = |k: &str| v[k].as_str().map(String::from);
    match (field("url"), field("baseRefName"), field("headRefOid")) {
        (Some(url), Some(base), Some(head)) => Ok((url, base, head)),
        _ => Err(Fault::Refused(format!("gh pr view answered {v}"))),
    }
}

/// `pr_view_read`, either fault as a `Reject`.
pub fn pr_view(program: &str, repo: &str, pr: i64) -> Result<(String, String, String), Reject> {
    pr_view_read(program, repo, pr).map_err(Reject::from)
}

/// Fetch the PR's head and base into `clone` under `refs/makiba/gate/<repo>/<n>/`, through the
/// clone's own `origin` (Issue 198), which must name `repo`; their ids. The configured URL is
/// compared, not the one `url.<x>.insteadOf` rewrites it to; git applies the person's rewrites and
/// ssh configuration when it fetches. Nothing is fetched from an origin that names another one. A
/// failed fetch is tried again (Issue 512) and, if it still fails, is `Fault::Unavailable`.
pub fn fetch_read(
    clone: &Path,
    repo: &str,
    pr: i64,
    base_branch: &str,
) -> Result<(String, String), Fault> {
    let never = AtomicBool::new(false);
    with_tries(|| fetch_try(clone, repo, pr, base_branch, &never))
}

/// `fetch_read`, either fault as a `Reject`.
pub fn fetch(
    clone: &Path,
    repo: &str,
    pr: i64,
    base_branch: &str,
) -> Result<(String, String), Reject> {
    fetch_read(clone, repo, pr, base_branch).map_err(Reject::from)
}

/// As `fetch`, one try; the git child is killed when `cancel` is set (a pass's thread at makibad's
/// stop, Issue 461).
pub fn fetch_with(
    clone: &Path,
    repo: &str,
    pr: i64,
    base_branch: &str,
    cancel: &AtomicBool,
) -> Result<(String, String), Reject> {
    match fetch_try(clone, repo, pr, base_branch, cancel) {
        Try::Done(r) => r.map_err(Reject::from),
        Try::Again(why) => Err(Reject(why)),
    }
}

/// One try of the fetch: a refusal of the clone's origin, or a failed git fetch, which another try
/// may change.
fn fetch_try(
    clone: &Path,
    repo: &str,
    pr: i64,
    base_branch: &str,
    cancel: &AtomicBool,
) -> Try<(String, String)> {
    let origin = match origin_of(clone, repo) {
        Ok(o) => o,
        Err(e) => return Try::Done(Err(e.into())),
    };
    let r = format!("refs/makiba/gate/{repo}/{pr}");
    let dir = clone.display().to_string();
    let fetched = gh::run_gh(
        "git",
        &[
            "-C",
            &dir,
            "-c",
            HOOKS_OFF,
            "fetch",
            "--no-tags",
            "--quiet",
            "--refmap=",
            "origin",
            &format!("+refs/pull/{pr}/head:{r}/head"),
            &format!("+refs/heads/{base_branch}:{r}/base"),
        ],
        cancel,
    );
    if let Err(e) = fetched {
        let (why, again) = gh_failure("git", e);
        let why = format!("git fetch of the PR through origin ({origin}) failed: {why}");
        return if again {
            Try::Again(why)
        } else {
            Try::Done(Err(Fault::Unavailable(why)))
        };
    }
    let id = |end: &str| -> Result<String, Reject> {
        Ok(
            git(clone, &["rev-parse", &format!("{r}/{end}^{{commit}}")])?
                .trim()
                .to_string(),
        )
    };
    Try::Done(
        id("base")
            .and_then(|b| Ok((b, id("head")?)))
            .map_err(Fault::from),
    )
}

/// The clone's one origin URL, when it names `repo` (as configured, before `insteadOf`); the
/// gate fetches only through such an origin, so it can pass no PR of a clone without one.
pub fn origin_of(clone: &Path, repo: &str) -> Result<String, Reject> {
    let urls = git(clone, &["config", "--get-all", "remote.origin.url"]).unwrap_or_default();
    let urls: Vec<&str> = urls
        .lines()
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .collect();
    let [origin] = urls[..] else {
        return reject(format!(
            "the project's clone {} has no single origin URL ({urls:?}); the gate fetches only through an origin that names {repo}",
            clone.display()
        ));
    };
    if super::triage::owner_repo_from_url(origin).as_deref() != Some(repo.to_lowercase().as_str()) {
        return reject(format!(
            "the project's clone {} has origin {origin}, which is not {repo}; the gate fetches only through an origin that names the PR's repository",
            clone.display()
        ));
    }
    Ok(origin.to_string())
}

/// The project repository of `repo` in the repositories folder: an active project whose URL, or
/// else whose clone's `origin`, names it.
pub fn clone_of(
    projects: &[(String, Option<String>)],
    repos: &Path,
    repo: &str,
) -> Option<PathBuf> {
    projects
        .iter()
        .find(|(slug, url)| {
            super::triage::repo_of(slug, url.as_deref(), repos).as_deref() == Some(repo)
        })
        .map(|(slug, _)| repos.join(slug))
}

#[cfg(test)]
mod tests {
    use super::{Fault, Try, with_tries_in};
    use std::cell::Cell;
    use std::time::Duration;

    /// A simulated clock: each read takes `read_takes`, and a sleep of `d` lasts `d * stretch`.
    /// The start time of each try, and the answer.
    fn tried(
        wait: u64,
        window: u64,
        read_takes: u64,
        stretch: u32,
        answers_on: Option<usize>,
    ) -> (Vec<u64>, Result<usize, Fault>) {
        let now = Cell::new(Duration::ZERO);
        let mut starts = Vec::new();
        let r = with_tries_in(
            Duration::from_millis(wait),
            Duration::from_millis(window),
            || now.get(),
            |d| now.set(now.get() + d * stretch),
            || {
                starts.push(now.get().as_millis() as u64);
                now.set(now.get() + Duration::from_millis(read_takes));
                if answers_on == Some(starts.len()) {
                    Try::Done(Ok(starts.len()))
                } else {
                    Try::Again(format!("fail {}", starts.len()))
                }
            },
        );
        (starts, r)
    }

    /// Issue 512: a failed read is tried again up to three times; one that answers on a later try
    /// is its answer.
    #[test]
    fn a_failed_read_is_tried_again_at_most_three_times() {
        let (starts, r) = tried(5000, 60_000, 100, 1, None);
        assert_eq!(starts, [0, 5100, 10200]);
        assert_eq!(r, Err(Fault::Unavailable("fail 3 (after 3 tries)".into())));
        let (starts, r) = tried(5000, 60_000, 100, 1, Some(2));
        assert_eq!(starts, [0, 5100]);
        assert_eq!(r, Ok(2));
    }

    /// Issue 512: no try starts once the window has passed: a wait that would end after it ends
    /// the read instead.
    #[test]
    fn no_try_starts_after_the_window() {
        // A read that times out at 40 s: the wait would end at 45 s, the second read at 85 s.
        let (starts, r) = tried(5000, 60_000, 40_000, 1, None);
        assert_eq!(starts, [0, 45_000]);
        assert_eq!(r, Err(Fault::Unavailable("fail 2 (after 2 tries)".into())));
        let (starts, _) = tried(30_000, 50_000, 1, 1, None);
        assert_eq!(starts, [0, 30_001]);
    }

    /// Issue 512: a wait that lasts longer than asked (a busy or suspended host) and ends after
    /// the window starts no other try.
    #[test]
    fn a_wait_that_overshoots_the_window_starts_no_other_try() {
        let (starts, r) = tried(5000, 60_000, 100, 20, None);
        assert_eq!(starts, [0]);
        assert_eq!(r, Err(Fault::Unavailable("fail 1 (after 1 try)".into())));
    }
}
