//! The goal ledger and the goal stage, carried from claude-org-ja
//! `tools/work_discovery_goals.py` without its judge (Issue 173).
//!
//! A ledger (`<goals>/<owner>/<repo>.md`, `docs/goals.md`) lists the project's clauses `G1..Gn`,
//! higher first. A project with no ledger yields no candidates and is reported as unset; a ledger
//! that cannot be read yields none and is reported with the reason. No model is asked which clause
//! a candidate serves: an Issue names its clause itself, under its `## Goal clause` heading, and
//! the candidates are ordered by that clause, those naming none after the others.

use super::{make_recommendation, sort_key};
use regex::Regex;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

static CLAUSE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^##\s+G([1-9][0-9]*)\s+(\S.*)$").unwrap());
// Looks like a clause heading but is not one (`## G1`, `## G01 x`, `## g2 x`): an error, not
// another heading, or a clause the person meant to write would be dropped.
static CLAUSE_LIKE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^##\s+G[0-9]").unwrap());
static UNMET: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^\s*[-*]\s+unmet if:\s*(\S.*)$").unwrap());
static OTHER_HEADING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^#{1,2}(\s|$)").unwrap());
static FENCE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*```").unwrap());
static SLUG_PART: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z0-9_.-]+$").unwrap());
static GOAL_HEADING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^#{1,6}\s+Goal clause\s*$").unwrap());
static ANY_HEADING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^#{1,6}(\s|$)").unwrap());
static NAMED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*[-*]\s+G([1-9][0-9]*)(\s|$)").unwrap());

/// One clause of a ledger.
#[derive(Debug, Clone, PartialEq)]
pub struct Clause {
    /// `G<index>`.
    pub id: String,
    pub index: u32,
    pub heading: String,
    pub body: String,
    pub unmet_if: Vec<String>,
}

impl Clause {
    pub fn to_json(&self) -> Value {
        json!({"id": self.id, "heading": self.heading, "unmet_if": self.unmet_if})
    }
}

/// Parse a ledger into its clauses, in ledger (priority) order. Any violation is an error, so the
/// project is reported instead of guessed at.
pub fn parse_ledger(text: &str) -> Result<Vec<Clause>, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let text = text.replace("\r\n", "\n");
    let mut clauses: Vec<Clause> = Vec::new();
    let mut current: Option<Clause> = None;
    let mut body: Vec<&str> = Vec::new();
    let mut in_fence = false;
    let close = |current: &mut Option<Clause>, body: &mut Vec<&str>, out: &mut Vec<Clause>| {
        if let Some(mut c) = current.take() {
            c.body = body.join("\n").trim().to_string();
            out.push(c);
        }
        body.clear();
    };
    for (i, line) in text.split('\n').enumerate() {
        if FENCE.is_match(line) {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if let Some(m) = CLAUSE.captures(line) {
            close(&mut current, &mut body, &mut clauses);
            let index: u32 = m[1]
                .parse()
                .map_err(|_| format!("line {}: bad number", i + 1))?;
            current = Some(Clause {
                id: format!("G{}", &m[1]),
                index,
                heading: m[2].trim().to_string(),
                body: String::new(),
                unmet_if: Vec::new(),
            });
            continue;
        }
        if CLAUSE_LIKE.is_match(line) {
            return Err(format!(
                "line {}: malformed clause heading: {}",
                i + 1,
                line.trim()
            ));
        }
        if OTHER_HEADING.is_match(line) {
            close(&mut current, &mut body, &mut clauses);
            continue;
        }
        let Some(c) = current.as_mut() else {
            continue;
        };
        match UNMET.captures(line) {
            Some(u) => c.unmet_if.push(u[1].trim().to_string()),
            None => body.push(line),
        }
    }
    close(&mut current, &mut body, &mut clauses);
    if clauses.is_empty() {
        return Err("no clauses".into());
    }
    let n = clauses.len();
    for (pos, c) in clauses.iter().enumerate() {
        if c.index as usize != pos + 1 {
            return Err(format!(
                "clause numbering must be G1..G{n} in order (found {} at position {})",
                c.id,
                pos + 1
            ));
        }
        if c.unmet_if.is_empty() {
            return Err(format!("{}: no 'unmet if:' line", c.id));
        }
    }
    Ok(clauses)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalStatus {
    Ok,
    Unset,
    Error,
}

/// One repository's ledger as loaded.
#[derive(Debug, Clone, PartialEq)]
pub struct Goals {
    /// The slug, lowercased.
    pub repo: String,
    pub status: GoalStatus,
    pub path: Option<PathBuf>,
    pub clauses: Vec<Clause>,
    pub error: Option<String>,
}

pub fn split_slug(slug: &str) -> Option<(String, String)> {
    let lower = slug.to_lowercase();
    let parts: Vec<&str> = lower.split('/').collect();
    let ok = |p: &&str| SLUG_PART.is_match(p) && *p != "." && *p != "..";
    if parts.len() != 2 || !parts.iter().all(ok) {
        return None;
    }
    Some((parts[0].to_string(), parts[1].to_string()))
}

type Entry = (Option<PathBuf>, Option<PathBuf>);

/// (exact match, match that differs only in case) for `name` in `dir`, by listing the folder so
/// a case-insensitive file system cannot let `Repo.md` pass as `repo.md`. Only a missing folder
/// is "nothing there"; any other failure is an error.
fn ci_entry(dir: &Path, name: &str) -> std::io::Result<Entry> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((None, None)),
        Err(e) => return Err(e),
    };
    let names: Vec<(String, PathBuf)> = entries
        .filter_map(Result::ok)
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
        .collect();
    let exact = names
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, p)| p.clone());
    let folded = names
        .iter()
        .find(|(n, _)| n.to_lowercase() == name && n != name)
        .map(|(_, p)| p.clone());
    Ok((exact, folded))
}

/// Load one repository's ledger: `Ok`, `Unset` (no file) or `Error` (with the reason).
pub fn load_goals(goals_dir: &Path, slug: &str) -> Goals {
    let mut out = Goals {
        repo: slug.to_lowercase(),
        status: GoalStatus::Error,
        path: None,
        clauses: Vec::new(),
        error: None,
    };
    let Some((owner, repo)) = split_slug(slug) else {
        out.error = Some(format!("bad repo slug: {slug:?}"));
        return out;
    };
    let fname = format!("{repo}.md");
    let looked = (|| -> std::io::Result<(Option<PathBuf>, Option<PathBuf>)> {
        let (owner_exact, owner_folded) = ci_entry(goals_dir, &owner)?;
        let (file_exact, file_folded) = match &owner_exact {
            Some(d) => ci_entry(d, &fname)?,
            None => (None, None),
        };
        let mut found = None;
        if file_exact.is_none() {
            found = file_folded;
            if found.is_none()
                && let Some(d) = owner_folded
            {
                let (e, f) = ci_entry(&d, &fname)?;
                found = e.or(f);
            }
        }
        Ok((file_exact, found))
    })();
    let (file_exact, found) = match looked {
        Ok(v) => v,
        Err(e) => {
            out.path = Some(goals_dir.join(&owner).join(&fname));
            out.error = Some(format!("goals dir unreadable: {:?}: {e}", e.kind()));
            return out;
        }
    };
    let Some(file) = file_exact else {
        if let Some(f) = found {
            out.error = Some(format!("case mismatch: {}", f.display()));
            out.path = Some(f);
            return out;
        }
        out.status = GoalStatus::Unset;
        out.path = Some(goals_dir.join(&owner).join(&fname));
        return out;
    };
    out.path = Some(file.clone());
    let text = match std::fs::read(&file).map(String::from_utf8) {
        Ok(Ok(t)) => t,
        Ok(Err(e)) => {
            out.error = Some(format!("unreadable: not UTF-8: {e}"));
            return out;
        }
        Err(e) => {
            out.error = Some(format!("unreadable: {:?}: {e}", e.kind()));
            return out;
        }
    };
    match parse_ledger(&text) {
        Ok(c) => {
            out.clauses = c;
            out.status = GoalStatus::Ok;
        }
        Err(e) => out.error = Some(e),
    }
    out
}

/// The clause an Issue names itself: the first list line `- G<n> ...` under its `Goal clause`
/// heading, before the next heading. `G` is case-sensitive, as in the ledger.
pub fn named_clause(body: &str) -> Option<u32> {
    let mut lines = super::refs::splitlines(body).into_iter();
    lines.find(|l| GOAL_HEADING.is_match(l))?;
    for line in lines {
        if ANY_HEADING.is_match(line) {
            return None;
        }
        if let Some(c) = NAMED.captures(line) {
            return c[1].parse().ok();
        }
    }
    None
}

/// What the goal stage returns to the scan.
pub struct GoalOutcome {
    pub candidates: Vec<Map<String, Value>>,
    pub truncated_count: usize,
    pub recommendation: Value,
    pub goal_rank: Value,
}

/// Order the whole resolved `pool` by the clause each candidate's Issue names, before the top-N
/// cut. `repos` are every scanned repository (`None` for one with no known name), so a project
/// whose Issues are all blocked is still reported when it has no ledger.
pub fn goal_rank(
    pool: Vec<Map<String, Value>>,
    repos: &[Option<String>],
    goals_dir: &Path,
    top_n: i64,
    free_slots: Option<i64>,
) -> GoalOutcome {
    let slug = |c: &Map<String, Value>| {
        c.get("_real_repo")
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut unknown = repos.iter().any(Option::is_none);
    for c in &pool {
        match slug(c) {
            Some(s) => *counts.entry(s).or_default() += 1,
            None => unknown = true,
        }
    }
    let all: BTreeSet<String> = repos
        .iter()
        .flatten()
        .map(|r| r.to_lowercase())
        .chain(counts.keys().cloned())
        .collect();

    let mut goals = Vec::new();
    let mut unset = Vec::new();
    let mut errors = Vec::new();
    let mut signals: Vec<String> = Vec::new();
    if unknown {
        errors.push(json!({"repo": null, "path": null, "error": "repository unknown"}));
    }
    let mut ledgers: BTreeMap<String, Vec<Clause>> = BTreeMap::new();
    for s in &all {
        let g = load_goals(goals_dir, s);
        let path = g.path.as_ref().map(|p| p.display().to_string());
        match g.status {
            GoalStatus::Ok => {
                goals.push(json!({
                    "repo": s, "path": path, "clause_count": g.clauses.len(),
                    "clauses": g.clauses.iter().map(Clause::to_json).collect::<Vec<_>>(),
                }));
                ledgers.insert(s.clone(), g.clauses);
            }
            GoalStatus::Unset => unset.push(json!({
                "repo": s, "path": path, "candidate_count": counts.get(s).copied().unwrap_or(0),
            })),
            GoalStatus::Error => errors.push(json!({"repo": s, "path": path, "error": g.error})),
        }
    }

    let mut hits: Vec<(u32, Map<String, Value>)> = Vec::new();
    for mut c in pool {
        let Some(clauses) = slug(&c).and_then(|s| ledgers.get(&s)) else {
            continue;
        };
        let body = c.get("_body").and_then(Value::as_str).unwrap_or("");
        let named = named_clause(body);
        let clause = named.and_then(|n| clauses.get(n as usize - 1));
        if let (Some(n), None) = (named, clause) {
            let note = format!(
                "names G{n}, which the ledger of {} does not have -- ordered as naming none",
                slug(&c).unwrap_or_default()
            );
            if let Some(Value::Array(sig)) = c.get_mut("signals") {
                sig.push(Value::from(note.clone()));
            }
            signals.push(format!(
                "{}#{}: {note}",
                slug(&c).unwrap_or_default(),
                c.get("issue").cloned().unwrap_or_default()
            ));
        }
        c.insert(
            "goal_clause".into(),
            clause.map_or(Value::Null, |k| json!({"id": k.id, "heading": k.heading})),
        );
        hits.push((clause.map_or(u32::MAX, |k| k.index), c));
    }
    hits.sort_by_cached_key(|a| (a.0, sort_key(&a.1, free_slots)));
    let total = hits.len();
    let mut emitted: Vec<Map<String, Value>> = hits.into_iter().map(|(_, c)| c).collect();
    if top_n >= 0 {
        emitted.truncate(top_n as usize);
    }
    for (i, c) in emitted.iter_mut().enumerate() {
        c.insert("rank".into(), json!(i + 1));
    }
    let mut recommendation = make_recommendation(&emitted);
    if let (Some(Value::Object(rec)), Some(Value::Object(k))) = (
        Some(&mut recommendation),
        emitted.first().and_then(|c| c.get("goal_clause")),
    ) {
        let reason = rec
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let id = k.get("id").and_then(Value::as_str).unwrap_or("");
        let heading = k.get("heading").and_then(Value::as_str).unwrap_or("");
        rec.insert(
            "reason".into(),
            json!(format!("{id} \"{heading}\": {reason}")),
        );
    }
    GoalOutcome {
        truncated_count: total - emitted.len(),
        candidates: emitted,
        recommendation,
        goal_rank: json!({
            "goals_dir": goals_dir.display().to_string(),
            "goals": goals,
            "goal_unset_repos": unset,
            "goal_errors": errors,
            "signals": signals,
        }),
    }
}
