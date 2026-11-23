//! Work discovery: which open Issues are worth starting next, as pure functions of what was
//! fetched (Issue 173, carried from claude-org-ja `tools/work_discovery_scan.py`).
//!
//! The scan reads open Issues, open PRs, recent merges and merges into a base branch, keyed by
//! `(repository, number)`. An Issue blocked by an open Issue or PR, or by a `blocked` label, is
//! excluded with its reason; one a merge into a non-default base branch already closed is excluded
//! naming that merge. The rest are ranked: by the goal clause their Issue names when a goals
//! folder is given (`goals`), else by the carried key (priority, follow-up of a recent merge,
//! parallelizable when slots are free, small effort, milestone, recency).
//!
//! Nothing here runs `gh`, reads a clock or touches `state.db`: GitHub is read through the
//! caller's `Fetch`, and the goal ledgers are read from the folder the caller names.

pub mod effort;
pub mod forward;
pub mod goals;
pub mod refs;

use effort::{compute_priority, estimate_effort, milestone_title};
use refs::{
    Canon, QualRef, canon_repo, classify_q, cross_keyword_refs, dependency_line_unread,
    dependency_text, has_block_label, is_home_disp, label_names, pr_close_refs, pr_referenced_refs,
    ref_to_disp, ref_to_json, unparsed_dependency_line,
};
use serde_json::{Map, Value, json};
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

/// Rows asked for in the open Issue and open PR reads; a full page sets the truncation flag.
pub const DEFAULT_OPEN_LIMIT: usize = 500;
/// Recent merges read for the follow-up axis.
pub const DEFAULT_RECENT_MERGES: usize = 10;
/// Merges into a base branch read for the completion check.
pub const DEFAULT_BASE_MERGE_LIMIT: usize = 100;
/// Merged PRs read to learn the effort model.
pub const DEFAULT_EFFORT_HISTORY: usize = 60;
/// Merged PRs are read this many times over and then cut to the exact `mergedAt` top-K: gh's
/// order only approximates merge recency.
const OVERFETCH: usize = 3;

/// One repository's fetched input.
#[derive(Debug, Clone, Default)]
pub struct Bundle {
    /// `owner/repo`; `None` only in the single-repo `scan` shim.
    pub repo: Option<String>,
    pub issues: Vec<Value>,
    pub open_pr_numbers: Vec<i64>,
    pub recent_merges: Vec<Value>,
    pub base_branch: Option<String>,
    pub base_merges: Vec<Value>,
}

/// The caller's knobs.
#[derive(Debug, Clone)]
pub struct ScanConfig {
    /// How many candidates to keep; negative keeps all.
    pub top_n: i64,
    /// Free worker slots; a positive number ranks parallelizable candidates up.
    pub free_slots: Option<i64>,
    /// What started the scan, echoed as `generated_for`.
    pub trigger: String,
}

impl Default for ScanConfig {
    fn default() -> Self {
        ScanConfig {
            top_n: 3,
            free_slots: None,
            trigger: "manual".into(),
        }
    }
}

/// A read that hit its row cap, so some rows may be unseen.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Truncation {
    pub open_issues: bool,
    pub open_prs: bool,
    pub base_merges: bool,
}

impl Truncation {
    /// Either one truncated (the flags of several repositories add up).
    pub fn or(self, o: Truncation) -> Truncation {
        Truncation {
            open_issues: self.open_issues || o.open_issues,
            open_prs: self.open_prs || o.open_prs,
            base_merges: self.base_merges || o.base_merges,
        }
    }
}

/// The reads of GitHub the scan needs, supplied by the caller (Issue 40 runs `gh`). Each returns
/// gh's JSON rows, at most `limit` of them.
pub trait Fetch {
    /// Open Issues: `number, title, body, labels, updatedAt, createdAt, milestone, comments`.
    fn open_issues(&mut self, repo: &str, limit: usize) -> Result<Vec<Value>, String>;
    /// Open PRs: `number`.
    fn open_prs(&mut self, repo: &str, limit: usize) -> Result<Vec<Value>, String>;
    /// Merged PRs, most recently updated first: `number, title, body, mergedAt`.
    fn recent_merges(&mut self, repo: &str, limit: usize) -> Result<Vec<Value>, String>;
    /// Merged PRs into `base`, most recently updated first:
    /// `number, title, body, baseRefName, mergedAt`.
    fn base_merges(&mut self, repo: &str, base: &str, limit: usize) -> Result<Vec<Value>, String>;
    /// Merged PRs, most recently updated first: `number, additions, deletions, changedFiles,
    /// reviews, createdAt, mergedAt, closingIssuesReferences`.
    fn effort_history(&mut self, repo: &str, limit: usize) -> Result<Vec<Value>, String>;
    /// Closed Issues, most recently updated first: `number, body`.
    fn closed_issues(&mut self, repo: &str, limit: usize) -> Result<Vec<Value>, String>;
}

/// How many rows each read asks for.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub open: usize,
    pub recent_merges: usize,
    pub base_merges: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            open: DEFAULT_OPEN_LIMIT,
            recent_merges: DEFAULT_RECENT_MERGES,
            base_merges: DEFAULT_BASE_MERGE_LIMIT,
        }
    }
}

/// The newest `limit` by `mergedAt` (a missing one is oldest), of `limit` times `OVERFETCH` read.
fn newest_merges(
    read: impl FnOnce(usize) -> Result<Vec<Value>, String>,
    limit: usize,
) -> Result<Vec<Value>, String> {
    let mut rows = read(limit.max(limit * OVERFETCH))?;
    let at = |p: &Value| {
        p.get("mergedAt")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    rows.sort_by_key(|p| std::cmp::Reverse(at(p)));
    rows.truncate(limit);
    Ok(rows)
}

/// Read one repository through `fetch`. `base_branch` (a project's non-default base) adds the
/// merges into it; `limits.base_merges == 0` turns that off.
pub fn gather(
    fetch: &mut dyn Fetch,
    repo: &str,
    base_branch: Option<&str>,
    limits: &Limits,
) -> Result<(Bundle, Truncation), String> {
    let mut t = Truncation::default();
    let issues = fetch.open_issues(repo, limits.open)?;
    t.open_issues = issues.len() >= limits.open;
    let mut prs: Vec<i64> = fetch
        .open_prs(repo, limits.open)?
        .iter()
        .filter_map(|p| p.get("number").and_then(Value::as_i64))
        .collect();
    prs.sort();
    prs.dedup();
    t.open_prs = prs.len() >= limits.open;
    let recent = newest_merges(|n| fetch.recent_merges(repo, n), limits.recent_merges)?;
    let base = base_branch
        .map(str::trim)
        .filter(|b| !b.is_empty() && limits.base_merges > 0);
    let mut base_merges = Vec::new();
    if let Some(b) = base {
        base_merges = newest_merges(|n| fetch.base_merges(repo, b, n), limits.base_merges)?;
        t.base_merges = base_merges.len() >= limits.base_merges;
    }
    Ok((
        Bundle {
            repo: Some(repo.to_string()),
            issues,
            open_pr_numbers: prs,
            recent_merges: recent,
            base_branch: base.map(str::to_string),
            base_merges,
        },
        t,
    ))
}

/// Learn `repo`'s effort model from its recent merges through `fetch` (carried
/// `build_effort_model`), with the coverage of the learning data.
pub fn build_effort_model(
    fetch: &mut dyn Fetch,
    repo: &str,
    history_limit: usize,
) -> Result<Value, String> {
    let merged = newest_merges(|n| fetch.effort_history(repo, n), history_limit)?;
    let closed_limit = (history_limit * 4).clamp(200, DEFAULT_OPEN_LIMIT);
    let bodies: HashMap<i64, String> = fetch
        .closed_issues(repo, closed_limit)?
        .iter()
        .filter_map(|i| {
            let n = i.get("number").and_then(Value::as_i64)?;
            Some((
                n,
                i.get("body").and_then(Value::as_str).unwrap_or("").into(),
            ))
        })
        .collect();
    let samples = effort::build_effort_samples(&merged, &bodies, Some(repo));
    let mut model = effort::learn_effort_model(&samples);
    let linked = merged
        .iter()
        .filter(|p| effort::single_closing_issue(p, Some(repo)).is_some())
        .count();
    let linked_any = merged
        .iter()
        .filter(|p| effort::single_closing_issue(p, None).is_some())
        .count();
    model["coverage"] = json!({
        "single_issue_linked_prs": linked,
        "usable_samples": samples.len(),
        "dropped_missing_body": linked - samples.len(),
        "dropped_cross_repo": linked_any - linked,
    });
    Ok(model)
}

/// Whether the Issue is a leaf of the dependency graph (no open blocker). An estimate.
pub fn estimate_parallelizable(open_blocking: &[i64]) -> (bool, Vec<String>) {
    if open_blocking.is_empty() {
        return (
            true,
            vec!["leaf in dependency graph (no open dependency refs)".into()],
        );
    }
    let refs: Vec<String> = open_blocking.iter().map(|n| format!("#{n}")).collect();
    (
        false,
        vec![format!("has open dependency refs: {}", refs.join(", "))],
    )
}

fn unblocked_q(
    issue_ref: Option<&QualRef>,
    blocking: &[QualRef],
    merged_prs: &HashSet<QualRef>,
    closed: &HashSet<QualRef>,
    referenced: &HashSet<QualRef>,
    collapse: Option<&str>,
) -> (bool, Vec<String>) {
    let mut signals = Vec::new();
    let mut hit = false;
    for r in blocking {
        if merged_prs.contains(r) {
            signals.push(format!(
                "blocking ref {} was a recently-merged PR",
                ref_to_disp(r, collapse)
            ));
            hit = true;
        } else if closed.contains(r) {
            signals.push(format!(
                "blocking ref {} was closed by a recently-merged PR",
                ref_to_disp(r, collapse)
            ));
            hit = true;
        }
    }
    if issue_ref.is_some_and(|r| referenced.contains(r)) {
        signals.push("referenced by a recently-merged PR".into());
        hit = true;
    }
    if !hit {
        signals.push("no recent-merge linkage detected".into());
    }
    (hit, signals)
}

/// Single-repo form: whether a recent merge was (or closed) one of the Issue's blockers, or
/// references the Issue itself. An estimate.
pub fn estimate_unblocked_by_recent_merge(
    issue: &Value,
    blocking_refs: &[i64],
    recent_merge_pr_numbers: &[i64],
    recent_merge_closed_issues: &[i64],
    recent_merge_referenced_issues: &[i64],
) -> (bool, Vec<String>) {
    let q = |v: &[i64]| -> HashSet<QualRef> { v.iter().map(|&n| (None, n)).collect() };
    let issue_ref = issue
        .get("number")
        .and_then(Value::as_i64)
        .map(|n| (None, n));
    let blocking: Vec<QualRef> = blocking_refs.iter().map(|&n| (None, n)).collect();
    unblocked_q(
        issue_ref.as_ref(),
        &blocking,
        &q(recent_merge_pr_numbers),
        &q(recent_merge_closed_issues),
        &q(recent_merge_referenced_issues),
        None,
    )
}

/// An Issue a merge into a base branch closed.
#[derive(Debug, Clone, PartialEq)]
pub struct Done {
    pub pr: QualRef,
    pub merged_at: String,
    pub base_branch: String,
    key: (String, i64),
}

/// The Issues merges into `base_branch` closed by their own closing keywords (GitHub does not
/// auto-close them off the default branch), the newest merge winning. A PR naming another
/// branch, or without a number, is not counted and says so.
pub fn base_merge_closed_refs(
    repo: Option<&str>,
    base_branch: &str,
    base_merges: &[Value],
) -> (BTreeMap<QualRef, Done>, Vec<String>) {
    let mut out: BTreeMap<QualRef, Done> = BTreeMap::new();
    let (mut mismatched, mut unnamed) = (0, 0);
    for pr in base_merges {
        if !pr.is_object() {
            continue;
        }
        if let Some(b) = pr.get("baseRefName").and_then(Value::as_str)
            && b.trim() != base_branch
        {
            mismatched += 1;
            continue;
        }
        let Some(number) = pr.get("number").and_then(Value::as_i64) else {
            unnamed += 1;
            continue;
        };
        let merged_at = pr
            .get("mergedAt")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let text = pr_text(pr);
        let mut closed: Vec<QualRef> = pr_close_refs(&text)
            .into_iter()
            .map(|n| (repo.map(str::to_string), n))
            .collect();
        closed.extend(
            cross_keyword_refs(&text, true)
                .into_iter()
                .map(|(r, n)| (Some(r), n)),
        );
        for r in closed {
            let key = (merged_at.clone(), number);
            if out.get(&r).is_none_or(|p| key > p.key) {
                out.insert(
                    r,
                    Done {
                        pr: (repo.map(str::to_string), number),
                        merged_at: merged_at.clone(),
                        base_branch: base_branch.to_string(),
                        key,
                    },
                );
            }
        }
    }
    let mut signals = Vec::new();
    if mismatched > 0 {
        signals.push(format!(
            "{mismatched} merged PR(s) supplied for base branch '{base_branch}' target a different branch - not counted"
        ));
    }
    if unnamed > 0 {
        signals.push(format!(
            "{unnamed} merged PR(s) for base branch '{base_branch}' had no integer number - not counted (an exclusion must cite its PR)"
        ));
    }
    (out, signals)
}

fn pr_text(pr: &Value) -> String {
    let s = |k: &str| pr.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    format!("{}\n{}", s("title"), s("body"))
}

/// One line of the body for the notice: the first that is not blank, a heading, a quote, a
/// comment, a table, a rule or a fence; else the title. At most 120 chars, cut at a space.
pub fn extract_summary(body: Option<&str>, title: &str) -> String {
    let mut candidate = String::new();
    for raw in refs::splitlines(body.unwrap_or("")) {
        let line = raw.trim();
        if line.is_empty()
            || ["#", ">", "<!--", "---", "***", "|", "```"]
                .iter()
                .any(|p| line.starts_with(p))
        {
            continue;
        }
        let mut line = line;
        if let Some(rest) = line.strip_prefix(['-', '*', '+'])
            && rest.starts_with(char::is_whitespace)
        {
            line = rest.trim_start();
        }
        let line = line.trim_matches(['*', '_', '`', ' ']);
        if !line.is_empty() {
            candidate = line.to_string();
            break;
        }
    }
    if candidate.is_empty() {
        candidate = title.trim().to_string();
    }
    if candidate.chars().count() > 120 {
        let head: String = candidate.chars().take(120).collect();
        let cut = match head.rfind(' ') {
            Some(i) => &head[..i],
            None => head.as_str(),
        };
        let cut = if cut.is_empty() { head.as_str() } else { cut };
        candidate = format!("{}\u{2026}", cut.trim_end());
    }
    candidate
}

/// A ranking key, smaller first.
pub type SortKey = (i64, i64, i64, i64, i64, NegStr, String, i64);

/// A string ordered later-first: a later timestamp sorts earlier (Python's tuple of negated
/// code points, so at an equal prefix the shorter sorts first).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NegStr(pub String);

impl Ord for NegStr {
    fn cmp(&self, o: &Self) -> Ordering {
        for (a, b) in self.0.chars().zip(o.0.chars()) {
            if a != b {
                return b.cmp(&a);
            }
        }
        self.0.chars().count().cmp(&o.0.chars().count())
    }
}

impl PartialOrd for NegStr {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

/// The carried ranking key: priority, follow-up of a recent merge, parallelizable when slots are
/// free, small effort, milestone, recency, then repository and number.
pub fn sort_key(c: &Map<String, Value>, free_slots: Option<i64>) -> SortKey {
    let s = |k: &str| c.get(k).and_then(Value::as_str).unwrap_or("");
    let b = |k: &str| c.get(k).and_then(Value::as_bool).unwrap_or(false);
    let prio = match s("priority") {
        "high" => 2,
        "low" => 0,
        _ => 1,
    };
    let effort = match s("effort") {
        "S" => 0,
        "L" => 2,
        _ => 1,
    };
    let par = b("parallelizable") && free_slots.unwrap_or(0) > 0;
    (
        -prio,
        -(b("unblocked_by_recent_merge") as i64),
        -(par as i64),
        effort,
        -(b("_has_milestone") as i64),
        NegStr(s("_updated_at").to_string()),
        s("repo").to_string(),
        c.get("issue").and_then(Value::as_i64).unwrap_or(0),
    )
}

/// Sort by `sort_key`, number them from 1 and keep `top_n` (all when negative): `(top, cut)`.
pub fn rank_candidates(
    mut candidates: Vec<Map<String, Value>>,
    top_n: i64,
    free_slots: Option<i64>,
) -> (Vec<Map<String, Value>>, usize) {
    candidates.sort_by_cached_key(|c| sort_key(c, free_slots));
    for (i, c) in candidates.iter_mut().enumerate() {
        c.insert("rank".into(), json!(i + 1));
    }
    let total = candidates.len();
    if top_n >= 0 {
        candidates.truncate(top_n as usize);
    }
    let cut = total - candidates.len();
    (candidates, cut)
}

/// The rank-1 candidate with why it comes first, or null.
pub fn make_recommendation(top: &[Map<String, Value>]) -> Value {
    let Some(best) = top.first() else {
        return Value::Null;
    };
    let s = |k: &str| best.get(k).and_then(Value::as_str).unwrap_or("");
    let b = |k: &str| best.get(k).and_then(Value::as_bool).unwrap_or(false);
    let mut bits = vec![format!("priority {}", s("priority"))];
    if b("unblocked_by_recent_merge") {
        bits.push("follow-up of a recent merge".into());
    }
    if b("parallelizable") {
        bits.push("parallelizable (can fill a free slot)".into());
    }
    let est = if b("effort_estimated") {
        " (estimated)"
    } else {
        ""
    };
    bits.push(format!("effort {}{est}", s("effort")));
    bits.push("dependencies resolved".into());
    json!({
        "repo": best.get("repo").cloned().unwrap_or(Value::Null),
        "issue": best.get("issue").cloned().unwrap_or(Value::Null),
        "reason": bits.join(", "),
    })
}

struct Ctx<'a> {
    open: HashSet<QualRef>,
    merged_prs: HashSet<QualRef>,
    closed: HashSet<QualRef>,
    referenced: HashSet<QualRef>,
    scanned: HashSet<Option<String>>,
    collapse: Option<&'a str>,
    effort_model: Option<&'a Value>,
    base_done: BTreeMap<QualRef, Done>,
    canon: Canon,
}

fn disp_repo(repo: Option<&str>, collapse: Option<&str>) -> Value {
    if is_home_disp(repo, collapse) {
        Value::Null
    } else {
        json!(repo)
    }
}

fn build_candidate(issue: &Value, home: Option<&str>, x: &Ctx) -> Option<Map<String, Value>> {
    let (blocked, open_blocking, all) = classify_q(issue, home, &x.open, &x.canon);
    if blocked {
        return None;
    }
    let number = issue.get("number").cloned().unwrap_or(Value::Null);
    let issue_ref = number.as_i64().map(|n| (home.map(str::to_string), n));
    let (priority, mut signals) = compute_priority(issue);
    let (effort, effort_estimated, effort_signals) = estimate_effort(issue, x.effort_model);
    let open_nums: Vec<i64> = open_blocking.iter().map(|r| r.1).collect();
    let (parallelizable, par_signals) = estimate_parallelizable(&open_nums);
    let (unblocked, merge_signals) = unblocked_q(
        issue_ref.as_ref(),
        &all,
        &x.merged_prs,
        &x.closed,
        &x.referenced,
        x.collapse,
    );
    signals.extend(effort_signals);
    signals.extend(par_signals);
    signals.extend(merge_signals);
    for (repo, num) in &all {
        if repo.is_some() && !x.scanned.contains(repo) {
            signals.push(format!(
                "cross-repo ref {}#{num} to un-scanned repo - treated resolved",
                repo.as_deref().unwrap_or("")
            ));
        }
    }
    for r in all.iter().filter(|r| x.base_done.contains_key(*r)) {
        let d = &x.base_done[r];
        signals.push(format!(
            "blocker {} closed by {} merged into {} - treated resolved",
            ref_to_disp(r, x.collapse),
            ref_to_disp(&d.pr, x.collapse),
            d.base_branch
        ));
    }
    let title = issue.get("title").and_then(Value::as_str).unwrap_or("");
    let body = issue.get("body").and_then(Value::as_str);
    let mut c = Map::new();
    c.insert("repo".into(), disp_repo(home, x.collapse));
    c.insert("issue".into(), number);
    c.insert("title".into(), json!(title));
    c.insert("summary".into(), json!(extract_summary(body, title)));
    c.insert("dependency".into(), json!("resolved"));
    c.insert(
        "blocking_refs".into(),
        json!(
            all.iter()
                .map(|r| ref_to_json(r, x.collapse))
                .collect::<Vec<_>>()
        ),
    );
    c.insert("priority".into(), json!(priority));
    c.insert("effort".into(), json!(effort));
    c.insert("effort_estimated".into(), json!(effort_estimated));
    c.insert("parallelizable".into(), json!(parallelizable));
    c.insert("parallelizable_estimated".into(), json!(true));
    c.insert("unblocked_by_recent_merge".into(), json!(unblocked));
    c.insert("unblocked_by_recent_merge_estimated".into(), json!(true));
    c.insert("rank".into(), Value::Null);
    c.insert("signals".into(), json!(signals));
    c.insert(
        "_updated_at".into(),
        json!(issue.get("updatedAt").and_then(Value::as_str).unwrap_or("")),
    );
    c.insert(
        "_has_milestone".into(),
        json!(milestone_title(issue).is_some()),
    );
    c.insert(
        "_real_repo".into(),
        json!(home.filter(|r| !r.is_empty()).map(str::to_lowercase)),
    );
    c.insert("_body".into(), json!(body.unwrap_or("")));
    c.insert("_labels".into(), json!(label_names(issue)));
    Some(c)
}

fn excluded_note(issue: &Value, open_blocking: &[QualRef], collapse: Option<&str>) -> String {
    if has_block_label(issue) && open_blocking.is_empty() {
        return "blocked/on-hold label".into();
    }
    let refs: Vec<String> = open_blocking
        .iter()
        .map(|r| ref_to_disp(r, collapse))
        .collect();
    format!("{} still open", refs.join(", "))
}

fn merged_note(d: &Done, collapse: Option<&str>) -> String {
    format!(
        "{} merged into {} (a merge outside the default branch fires no GitHub auto-close, so the Issue stays open)",
        ref_to_disp(&d.pr, collapse),
        d.base_branch
    )
}

/// Single-repo form of `scan_repos` (repository unnamed, shown as home): the carried `scan`.
#[allow(clippy::too_many_arguments)]
pub fn scan(
    issues: Vec<Value>,
    open_pr_numbers: Vec<i64>,
    recent_merges: Vec<Value>,
    config: &ScanConfig,
    truncated: Option<&Truncation>,
    effort_model: Option<&Value>,
    base_branch: Option<&str>,
    base_merges: Vec<Value>,
) -> Value {
    let bundle = Bundle {
        repo: None,
        issues,
        open_pr_numbers,
        recent_merges,
        base_branch: base_branch.map(str::to_string),
        base_merges,
    };
    scan_repos(&[bundle], config, truncated, None, effort_model, None)
}

/// The scan of every bundle together, as JSON in the carried shape.
///
/// `collapse_repo` shows that one repository as home (`repo: null`, bare numbers). A learned
/// `effort_model` is applied only when one repository is scanned (its cut points are that
/// repository's). `goals`: the goals folder; with it the goal stage ranks (`rank_mode: "goal"`),
/// without it the carried key does (`"legacy"`).
pub fn scan_repos(
    bundles: &[Bundle],
    config: &ScanConfig,
    truncated: Option<&Truncation>,
    collapse_repo: Option<&str>,
    effort_model: Option<&Value>,
    goals: Option<&Path>,
) -> Value {
    let distinct: HashSet<Option<&str>> = bundles.iter().map(|b| b.repo.as_deref()).collect();
    let effort_model = effort_model.filter(|_| distinct.len() == 1);
    let mut x = Ctx {
        open: HashSet::new(),
        merged_prs: HashSet::new(),
        closed: HashSet::new(),
        referenced: HashSet::new(),
        scanned: HashSet::new(),
        collapse: collapse_repo,
        effort_model,
        base_done: BTreeMap::new(),
        canon: bundles
            .iter()
            .filter_map(|b| b.repo.clone())
            .map(|r| (r.to_lowercase(), r))
            .collect(),
    };
    let mut raw_done: Vec<BTreeMap<QualRef, Done>> = Vec::new();
    let mut base_branch_scan = Vec::new();
    let mut base_signals: Vec<String> = Vec::new();
    let mut issue_by_ref: HashMap<QualRef, &Value> = HashMap::new();

    for b in bundles {
        let repo = b.repo.clone();
        x.scanned.insert(repo.clone());
        for issue in &b.issues {
            if let Some(n) = issue.get("number").and_then(Value::as_i64) {
                x.open.insert((repo.clone(), n));
                issue_by_ref.entry((repo.clone(), n)).or_insert(issue);
            }
        }
        for &n in &b.open_pr_numbers {
            x.open.insert((repo.clone(), n));
        }
        for pr in &b.recent_merges {
            if let Some(n) = pr.get("number").and_then(Value::as_i64) {
                x.merged_prs.insert((repo.clone(), n));
            }
            let text = pr_text(pr);
            let closed = pr_close_refs(&text);
            let referenced = pr_referenced_refs(&text);
            x.closed.extend(closed.iter().map(|&n| (repo.clone(), n)));
            x.referenced
                .extend(closed.union(&referenced).map(|&n| (repo.clone(), n)));
            for (r, n) in cross_keyword_refs(&text, true) {
                let r = canon_repo(r, &x.canon);
                x.closed.insert((Some(r.clone()), n));
                x.referenced.insert((Some(r), n));
            }
            for (r, n) in cross_keyword_refs(&text, false) {
                x.referenced.insert((Some(canon_repo(r, &x.canon)), n));
            }
        }
        let base = b.base_branch.as_deref().map(str::trim).unwrap_or("");
        if base.is_empty() {
            continue;
        }
        let (done, sigs) = base_merge_closed_refs(repo.as_deref(), base, &b.base_merges);
        base_signals.extend(sigs);
        base_branch_scan.push(json!({
            "repo": disp_repo(repo.as_deref(), collapse_repo),
            "base_branch": base,
            "merged_prs_scanned": b.base_merges.len(),
            "closed_issue_count": done.len(),
        }));
        raw_done.push(done);
    }

    // A closing keyword keeps the author's casing; fold it onto the scanned repository it names.
    for done in raw_done {
        for ((repo, n), info) in done {
            let repo = repo.map(|r| canon_repo(r, &x.canon));
            let r = (repo, n);
            if x.base_done.get(&r).is_none_or(|p| info.key > p.key) {
                x.base_done.insert(r, info);
            }
        }
    }
    // An Issue created after the merge that would have closed it was not closed by it.
    let stale: Vec<QualRef> = x
        .base_done
        .iter()
        .filter(|(r, info)| {
            let created = issue_by_ref
                .get(*r)
                .and_then(|i| i.get("createdAt"))
                .and_then(Value::as_str)
                .unwrap_or("");
            !created.is_empty() && !info.merged_at.is_empty() && created > info.merged_at.as_str()
        })
        .map(|(r, _)| r.clone())
        .collect();
    for r in stale {
        let info = x.base_done.remove(&r).unwrap();
        base_signals.push(format!(
            "{} is newer than {} ({}) - closing link ignored",
            ref_to_disp(&r, collapse_repo),
            ref_to_disp(&info.pr, collapse_repo),
            info.merged_at
        ));
    }
    for r in x.base_done.keys() {
        x.open.remove(r);
    }

    let mut candidates = Vec::new();
    let mut excluded_blocked = Vec::new();
    let mut excluded_merged = Vec::new();
    let mut excluded_unreadable = Vec::new();
    let mut warnings = Vec::new();
    let mut seen: HashSet<QualRef> = HashSet::new();
    for b in bundles {
        let repo = b.repo.as_deref();
        for issue in &b.issues {
            let number = issue.get("number").cloned().unwrap_or(Value::Null);
            if let Some(n) = number.as_i64() {
                let key = (repo.map(str::to_string), n);
                if !seen.insert(key.clone()) {
                    continue;
                }
                if let Some(d) = x.base_done.get(&key) {
                    excluded_merged.push(json!({
                        "repo": disp_repo(repo, collapse_repo),
                        "issue": n,
                        "base_branch": d.base_branch,
                        "closed_by_pr": ref_to_json(&d.pr, collapse_repo),
                        "merged_at": d.merged_at,
                        "note": merged_note(d, collapse_repo),
                    }));
                    continue;
                }
            }
            if let Some(line) = unparsed_dependency_line(&dependency_text(issue)) {
                warnings.push(json!({
                    "repo": disp_repo(repo, collapse_repo), "issue": number, "line": line,
                }));
            }
            // A dependency in words (no Issue number on its keyword line) is no candidate.
            if let Some(line) =
                dependency_line_unread(issue.get("body").and_then(Value::as_str).unwrap_or(""))
            {
                excluded_unreadable.push(json!({
                    "repo": disp_repo(repo, collapse_repo), "issue": number, "line": line,
                }));
                continue;
            }
            let (blocked, open_blocking, _) = classify_q(issue, repo, &x.open, &x.canon);
            if blocked {
                excluded_blocked.push(json!({
                    "repo": disp_repo(repo, collapse_repo),
                    "issue": number,
                    "blocking_refs": open_blocking.iter().map(|r| ref_to_json(r, collapse_repo)).collect::<Vec<_>>(),
                    "note": excluded_note(issue, &open_blocking, collapse_repo),
                }));
                continue;
            }
            if let Some(c) = build_candidate(issue, repo, &x) {
                candidates.push(c);
            }
        }
    }

    let (rank_mode, mut top, cut, recommendation, goal_rank) = match goals {
        None => {
            let (top, cut) = rank_candidates(candidates, config.top_n, config.free_slots);
            let rec = make_recommendation(&top);
            ("legacy", top, cut, rec, Value::Null)
        }
        Some(dir) => {
            let repos: Vec<Option<String>> = bundles.iter().map(|b| b.repo.clone()).collect();
            let g = goals::goal_rank(candidates, &repos, dir, config.top_n, config.free_slots);
            (
                "goal",
                g.candidates,
                g.truncated_count,
                g.recommendation,
                g.goal_rank,
            )
        }
    };
    for c in &mut top {
        c.entry("goal_clause").or_insert(Value::Null);
        c.retain(|k, _| !k.starts_with('_'));
    }
    let excl_key = |e: &Value| {
        let issue = &e["issue"];
        (
            e["repo"].as_str().unwrap_or("").to_string(),
            issue.is_null(),
            issue.as_i64().unwrap_or(0),
        )
    };
    excluded_blocked.sort_by_cached_key(excl_key);
    excluded_merged.sort_by_cached_key(excl_key);
    excluded_unreadable.sort_by_cached_key(excl_key);
    let t = truncated.copied().unwrap_or_default();
    json!({
        "status": if top.is_empty() { "no_candidates" } else { "candidates_found" },
        "generated_for": config.trigger,
        "candidate_count": top.len(),
        "truncated_count": cut,
        "input_truncated": {"open_issues": t.open_issues, "open_prs": t.open_prs, "base_merges": t.base_merges},
        "effort_model": effort_model.cloned(),
        "candidates": top,
        "recommendation": recommendation,
        "excluded_blocked": excluded_blocked,
        "excluded_dependency_unreadable": excluded_unreadable,
        "dependency_warnings": warnings,
        "excluded_merged": excluded_merged,
        "base_branch_scan": base_branch_scan,
        "base_branch_signals": base_signals,
        "rank_mode": rank_mode,
        "goal_rank": goal_rank,
    })
}
