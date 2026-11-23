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
//!     base, not from the PR;
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
//! Pin: the first verdict that saw a manifest records a digest of the declared files; a later
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
    let prot = |t: &Tree| -> Tree {
        t.iter()
            .filter(|(p, _)| is_protected(p, manifest))
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
    let touched = git(
        repo,
        &["diff", "--no-renames", "--name-only", "-z", &parent, msha],
    )?;
    let undeclared: BTreeSet<&str> = touched
        .split('\0')
        .filter(|p| !p.is_empty() && !is_protected(p, manifest))
        .collect();
    if !undeclared.is_empty() {
        let list: Vec<&str> = undeclared.into_iter().collect();
        return reject(format!(
            "manifest commit {short} also changes undeclared paths {list:?}: declare them with {TRAILER} or move them to a later commit"
        ));
    }
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

/// sha256 of the declared paths and their (mode, blob) in the manifest commit.
fn manifest_digest(repo: &Path, msha: &str, manifest: &[String]) -> Result<String, Reject> {
    let entries: Vec<Value> = tree(repo, msha)?
        .into_iter()
        .filter(|(p, _)| manifest.iter().any(|m| under(p, m)))
        .map(|(p, (mode, obj))| json!([p, mode, obj]))
        .collect();
    let mut sorted = manifest.to_vec();
    sorted.sort();
    let text = json!([sorted, entries]).to_string();
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

/// One JSON value per line of `gh api --paginate <path> --jq <jq>`.
pub fn gh_lines(program: &str, path: &str, jq: &str) -> Result<Vec<Value>, Reject> {
    let never = AtomicBool::new(false);
    let out =
        gh::run_gh(program, &["api", "--paginate", path, "--jq", jq], &never).map_err(|e| {
            Reject(format!(
                "cannot read {path} from GitHub: {}",
                match e {
                    GhError::Missing => format!("no program {program}"),
                    GhError::Cancelled => "cancelled".into(),
                    GhError::Failed(why) => why,
                }
            ))
        })?;
    out.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            serde_json::from_str(l)
                .map_err(|e| Reject(format!("cannot read {path} from GitHub: {e}")))
        })
        .collect()
}

/// (check suite, workflow, base SHAs) of the latest `pull_request` run per workflow at `sha`.
#[derive(Default)]
pub struct Ci {
    program: String,
    runs: BTreeMap<String, Vec<(i64, i64, Vec<String>)>>,
}

impl Ci {
    pub fn new(program: &str) -> Ci {
        Ci {
            program: program.to_string(),
            runs: BTreeMap::new(),
        }
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
        for r in gh_lines(
            &self.program,
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
        for r in gh_lines(
            &self.program,
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
}

/// A file under a folder `tests`: what the gate reports when a pull request edits it without
/// declaring it. Tests kept elsewhere (inline unit tests) are not seen.
fn is_test(path: &str) -> bool {
    path.starts_with("tests/") || path.contains("/tests/")
}

/// The tests a pull request edited after its test commit (Issue 282, belt.md section 5 line 5):
/// every file under a folder `tests` that exists at `P` and is changed or gone at `head`, where
/// `P` is the parent of the test commit `msha`, or the merge base of `base` and `head` when no
/// test is declared. Left out: the declared paths (condition (a) holds them), and a file `head`
/// holds exactly as `base` does (the base brought that change, as in `check_protected`). A new
/// test is not an edited one. A moved test is reported at the path it left.
pub fn edited_tests(
    repo: &Path,
    base: &str,
    head: &str,
    msha: Option<&str>,
    manifest: &[String],
) -> Result<Vec<String>, Reject> {
    let before = match msha {
        Some(m) => git(repo, &["rev-parse", &format!("{m}^")])?,
        None => git(repo, &["merge-base", base, head])?,
    };
    let (tp, th, tb) = (
        tree(repo, before.trim())?,
        tree(repo, head)?,
        tree(repo, base)?,
    );
    Ok(tp
        .iter()
        .filter(|(p, _)| is_test(p) && !manifest.iter().any(|m| under(p, m)))
        .filter(|(p, e)| th.get(*p) != Some(*e) && th.get(*p) != tb.get(*p))
        .map(|(p, _)| p.clone())
        .collect())
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

/// `gh pr view` of the PR: (number, url, base branch, head).
pub fn pr_view(program: &str, repo: &str, pr: i64) -> Result<(String, String, String), Reject> {
    let never = AtomicBool::new(false);
    let n = pr.to_string();
    let out = gh::run_gh(
        program,
        &[
            "pr",
            "view",
            &n,
            "--repo",
            repo,
            "--json",
            "number,url,baseRefName,headRefOid",
        ],
        &never,
    )
    .map_err(|e| {
        Reject(format!(
            "gh pr view failed: {}",
            match e {
                GhError::Missing => format!("no program {program}"),
                GhError::Cancelled => "cancelled".into(),
                GhError::Failed(why) => why,
            }
        ))
    })?;
    let v: Value = serde_json::from_str(out.trim())
        .map_err(|e| Reject(format!("gh pr view answered no JSON: {e}")))?;
    let field = |k: &str| v[k].as_str().map(String::from);
    match (field("url"), field("baseRefName"), field("headRefOid")) {
        (Some(url), Some(base), Some(head)) => Ok((url, base, head)),
        _ => reject(format!("gh pr view answered {v}")),
    }
}

/// Fetch the PR's head and base into `clone` under `refs/makiba/gate/<repo>/<n>/`, through the
/// clone's own `origin` (Issue 198), which must name `repo`; their ids. The configured URL is
/// compared, not the one `url.<x>.insteadOf` rewrites it to; git applies the person's rewrites and
/// ssh configuration when it fetches. Nothing is fetched from an origin that names another one.
pub fn fetch(
    clone: &Path,
    repo: &str,
    pr: i64,
    base_branch: &str,
) -> Result<(String, String), Reject> {
    let origin = origin_of(clone, repo)?;
    let r = format!("refs/makiba/gate/{repo}/{pr}");
    let never = AtomicBool::new(false);
    let dir = clone.display().to_string();
    gh::run_gh(
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
        &never,
    )
    .map_err(|_| {
        Reject(format!(
            "git fetch of the PR through origin ({origin}) failed"
        ))
    })?;
    let id = |end: &str| -> Result<String, Reject> {
        Ok(
            git(clone, &["rev-parse", &format!("{r}/{end}^{{commit}}")])?
                .trim()
                .to_string(),
        )
    };
    Ok((id("base")?, id("head")?))
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
