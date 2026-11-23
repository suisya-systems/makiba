//! Dependency, close and reference notation, carried from claude-org-ja
//! `tools/work_discovery_scan.py` (Issue 173).
//!
//! Precision over recall: a keyword (`Blocked by`, `Depends on`, `Requires`) counts only with a run
//! of refs right after it on the same line, a negated keyword ("not blocked by #5") does not
//! count, and a task-list item counts only when it holds nothing but refs. A wrong exclusion is the
//! worse error, so when in doubt an Issue is not blocked.

use regex::Regex;
use serde_json::Value;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::LazyLock;

macro_rules! re {
    ($name:ident, $pat:expr) => {
        static $name: LazyLock<Regex> = LazyLock::new(|| Regex::new(&$pat).unwrap());
    };
}

/// A ref keyed by its real repository (`None` = the home repository of a single-repo scan) and
/// its number, so `a#60` and `b#60` never collide.
pub type QualRef = (Option<String>, i64);

re!(
    BLOCK_NEG,
    r"(?i)(?:\b(?:not|never|no\s+longer|no\s+more)|n['\x{2019}]t)[\w\s]{0,20}$"
);
re!(
    BLOCK_KEYWORD,
    r"(?i)\b(?:blocked\s+by|depends\s+on|requires)\b"
);
// `(?<![\w/])#(\d+)\b`: the look-behind is `prev_ok` below.
re!(ISSUE_REF, r"#(\d+)\b");
re!(OPEN_TASK_ITEM, r"(?im)^[ \t]*[-*]\s*\[ \]\s*(.+?)\s*$");

const OWNER_REPO: &str = r"[A-Za-z0-9](?:[A-Za-z0-9-]*)/[A-Za-z0-9._-]+";
re!(
    CROSS_REF_TOKEN,
    format!(r"https?://github\.com/({OWNER_REPO})/(?:issues|pull)/(\d+)|({OWNER_REPO})#(\d+)")
);
fn ref_token_any() -> String {
    format!(
        r"(?:https?://github\.com/{OWNER_REPO}/(?:issues|pull)/\d+|{OWNER_REPO}#\d+|(?:pr\s+)?#\d+)"
    )
}
re!(
    LEADING_RUN,
    format!(
        r"(?i)^[\s:]*((?:{}[\s,]*(?:and\s+|&\s*)?)+)",
        ref_token_any()
    )
);
re!(
    PURE_REF_RUN,
    format!(r"(?i)^(?:{}[\s,]*(?:and\s+|&\s*)?)+$", ref_token_any())
);
re!(
    PR_CLOSE_KEYWORD,
    r"(?i)\b(?:closes|close|closed|fixes|fix|fixed|resolves|resolve|resolved)\b"
);
re!(PR_REF_KEYWORD, r"(?i)\b(?:refs|ref|re)\b");

const BLOCK_LABELS: [&str; 3] = ["blocked", "on-hold", "on hold"];

/// The Issue's labels, lowercased and trimmed; gh's `[{"name": ..}]` or plain strings.
pub fn label_names(issue: &Value) -> Vec<String> {
    let Some(labels) = issue.get("labels").and_then(Value::as_array) else {
        return Vec::new();
    };
    labels
        .iter()
        .filter_map(|l| match l {
            Value::Object(o) => o.get("name").and_then(Value::as_str),
            other => other.as_str(),
        })
        .map(|n| n.trim().to_lowercase())
        .collect()
}

fn comment_bodies(issue: &Value) -> Vec<&str> {
    let Some(comments) = issue.get("comments").and_then(Value::as_array) else {
        return Vec::new();
    };
    comments
        .iter()
        .filter_map(|c| match c {
            Value::Object(o) => o.get("body").and_then(Value::as_str),
            other => other.as_str(),
        })
        .collect()
}

/// Body and every comment body, joined by newlines: a blocker added in a comment counts.
pub fn dependency_text(issue: &Value) -> String {
    let mut parts = vec![issue.get("body").and_then(Value::as_str).unwrap_or("")];
    parts.extend(comment_bodies(issue));
    parts.join("\n")
}

fn line_bounds(text: &str, start: usize, end: usize) -> (usize, usize) {
    let line_start = text[..start].rfind('\n').map_or(0, |i| i + 1);
    let line_end = text[end..].find('\n').map_or(text.len(), |i| end + i);
    (line_start, line_end)
}

/// The leading ref run after each keyword hit that is not negated on its line.
fn keyword_lead_runs<'a>(text: &'a str, keyword: &Regex) -> Vec<&'a str> {
    let mut out = Vec::new();
    for m in keyword.find_iter(text) {
        let (line_start, line_end) = line_bounds(text, m.start(), m.end());
        if BLOCK_NEG.is_match(&text[line_start..m.start()]) {
            continue;
        }
        if let Some(c) = LEADING_RUN.captures(&text[m.end()..line_end]) {
            out.push(c.get(1).unwrap().as_str());
        }
    }
    out
}

/// The home `#N` of a run; a `#N` right after a word char or `/`, or inside a cross ref
/// (`owner/repo-#N`, a repository name may end in `-` or `.`), is not one.
fn home_refs(run: &str) -> Vec<i64> {
    let cross: Vec<(usize, usize)> = CROSS_REF_TOKEN
        .find_iter(run)
        .map(|m| (m.start(), m.end()))
        .collect();
    ISSUE_REF
        .captures_iter(run)
        .filter(|c| {
            let start = c.get(0).unwrap().start();
            run[..start]
                .chars()
                .next_back()
                .is_none_or(|p| !(p.is_alphanumeric() || p == '_' || p == '/'))
                && !cross.iter().any(|&(s, e)| s <= start && start < e)
        })
        .filter_map(|c| c[1].parse().ok())
        .collect()
}

fn cross_tokens(run: &str) -> Vec<(String, i64)> {
    CROSS_REF_TOKEN
        .captures_iter(run)
        .filter_map(|c| {
            let (repo, num) = match c.get(1) {
                Some(r) => (r, c.get(2)?),
                None => (c.get(3)?, c.get(4)?),
            };
            Some((repo.as_str().to_string(), num.as_str().parse().ok()?))
        })
        .collect()
}

/// Cross refs (`owner/repo#N`, GitHub URLs) after each non-negated keyword hit.
pub fn cross_keyword_refs(text: &str, close: bool) -> BTreeSet<(String, i64)> {
    let kw = if close {
        &*PR_CLOSE_KEYWORD
    } else {
        &*PR_REF_KEYWORD
    };
    keyword_lead_runs(text, kw)
        .into_iter()
        .flat_map(cross_tokens)
        .collect()
}

/// Home numbers a PR closes (`Closes #N`, `Fixes`, `Resolves`, ...).
pub fn pr_close_refs(text: &str) -> BTreeSet<i64> {
    keyword_lead_runs(text, &PR_CLOSE_KEYWORD)
        .into_iter()
        .flat_map(home_refs)
        .collect()
}

/// Home numbers a PR only references (`Refs #N`, `Ref`, `Re`).
pub fn pr_referenced_refs(text: &str) -> BTreeSet<i64> {
    keyword_lead_runs(text, &PR_REF_KEYWORD)
        .into_iter()
        .flat_map(home_refs)
        .collect()
}

fn pure_task_items(text: &str) -> Vec<&str> {
    OPEN_TASK_ITEM
        .captures_iter(text)
        .map(|c| c.get(1).unwrap().as_str())
        .filter(|item| PURE_REF_RUN.is_match(item))
        .collect()
}

/// Home numbers this Issue is blocked by, sorted. For a non-epic an open task item `- [ ] #N`
/// also counts.
pub fn extract_blocking_refs(text: Option<&str>, is_epic: bool) -> Vec<i64> {
    let text = text.unwrap_or("");
    let mut refs: BTreeSet<i64> = keyword_lead_runs(text, &BLOCK_KEYWORD)
        .into_iter()
        .flat_map(home_refs)
        .collect();
    if !is_epic {
        refs.extend(pure_task_items(text).into_iter().flat_map(home_refs));
    }
    refs.into_iter().collect()
}

/// Cross-repository blockers, sorted, with the same discipline as `extract_blocking_refs`.
pub fn extract_cross_repo_blocking_refs(text: Option<&str>, is_epic: bool) -> Vec<(String, i64)> {
    let text = text.unwrap_or("");
    let mut refs: BTreeSet<(String, i64)> = keyword_lead_runs(text, &BLOCK_KEYWORD)
        .into_iter()
        .flat_map(cross_tokens)
        .collect();
    if !is_epic {
        refs.extend(pure_task_items(text).into_iter().flat_map(cross_tokens));
    }
    refs.into_iter().collect()
}

/// Python's `str.splitlines`.
pub(crate) fn splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        let brk = matches!(
            c,
            '\n' | '\r'
                | '\x0b'
                | '\x0c'
                | '\x1c'
                | '\x1d'
                | '\x1e'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        );
        if brk {
            out.push(&s[start..i]);
            let mut next = i + c.len_utf8();
            if c == '\r' && it.peek().map(|&(_, n)| n) == Some('\n') {
                it.next();
                next += 1;
            }
            start = next;
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// The first line (at most 120 chars) with a non-negated dependency keyword followed by no ref
/// on its line while the next non-empty line opens with one: wording the same-line rule cannot
/// read. A warning only; it never changes what blocks.
pub fn unparsed_dependency_line(text: &str) -> Option<String> {
    for m in BLOCK_KEYWORD.find_iter(text) {
        let (line_start, line_end) = line_bounds(text, m.start(), m.end());
        if BLOCK_NEG.is_match(&text[line_start..m.start()]) {
            continue;
        }
        if LEADING_RUN.is_match(&text[m.end()..line_end]) {
            continue;
        }
        let next = splitlines(&text[line_end..])
            .into_iter()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("");
        if LEADING_RUN.is_match(next.trim_start()) {
            return Some(
                text[line_start..line_end]
                    .trim()
                    .chars()
                    .take(120)
                    .collect(),
            );
        }
    }
    None
}

// A line that starts with a dependency keyword, after an optional quote, heading, list marker,
// task box and any run of emphasis or code marks (Issue 299).
re!(
    DEP_LINE_START,
    r"(?i)^\s*(?:>\s*)*(?:#{1,6}\s+|[-*+]\s+|\d+[.)]\s+)?(?:\[[ xX]\]\s*)?[*_~`]*\s*(?:blocked\s+by|depends\s+on|requires)\b"
);
re!(ANY_NUMBER_REF, r"#\d");

fn shown_line(line: &str) -> String {
    line.trim().chars().take(120).collect()
}

/// The first line (at most 120 chars) of `body` that starts with a dependency keyword and holds
/// no `#<digits>`: a dependency in words, which the range's read (a `#<n>` on a `Depends on`
/// line) cannot read (Issue 299).
pub fn dependency_line_with_no_number(body: &str) -> Option<String> {
    splitlines(body)
        .into_iter()
        .find(|l| DEP_LINE_START.is_match(l) && !ANY_NUMBER_REF.is_match(l))
        .map(shown_line)
}

/// The first line (at most 120 chars) of `body` that starts with a dependency keyword not followed
/// on its line by a ref, while the next non-empty line does not open with one either (that case is
/// `unparsed_dependency_line`'s warning): a dependency the triage grammar reads as none
/// (Issue 299).
pub fn dependency_line_unread(body: &str) -> Option<String> {
    let lines = splitlines(body);
    for (i, line) in lines.iter().enumerate() {
        let Some(m) = DEP_LINE_START.find(line) else {
            continue;
        };
        if LEADING_RUN.is_match(&line[m.end()..]) {
            continue;
        }
        let next = lines[i + 1..]
            .iter()
            .find(|l| !l.trim().is_empty())
            .copied()
            .unwrap_or("");
        if LEADING_RUN.is_match(next.trim_start()) {
            continue;
        }
        return Some(shown_line(line));
    }
    None
}

/// A `blocked` / `on-hold` label makes the Issue blocked whatever its refs say.
pub fn has_block_label(issue: &Value) -> bool {
    label_names(issue)
        .iter()
        .any(|n| BLOCK_LABELS.contains(&n.as_str()))
}

pub(crate) fn ref_sort(refs: &mut Vec<QualRef>) {
    refs.sort_by(|a, b| {
        (a.0.as_deref().unwrap_or(""), a.1).cmp(&(b.0.as_deref().unwrap_or(""), b.1))
    });
    refs.dedup();
}

/// Every blocker of `issue` keyed by repository: home `#N` under `home_repo`, cross refs under
/// their own; a cross ref naming `home_repo` is the same ref as the home one. Sorted.
pub fn issue_blocking_refs_q(issue: &Value, home_repo: Option<&str>) -> Vec<QualRef> {
    let is_epic = label_names(issue).iter().any(|l| l == "epic");
    let text = dependency_text(issue);
    let mut refs: Vec<QualRef> = extract_blocking_refs(Some(&text), is_epic)
        .into_iter()
        .map(|n| (home_repo.map(str::to_string), n))
        .collect();
    refs.extend(
        extract_cross_repo_blocking_refs(Some(&text), is_epic)
            .into_iter()
            .map(|(r, n)| (Some(r), n)),
    );
    ref_sort(&mut refs);
    refs
}

/// Scanned repositories by their lowercased name: GitHub names are case-insensitive, so a ref
/// written `Owner/Repo#5` is the scanned `owner/repo`'s #5.
pub(crate) type Canon = HashMap<String, String>;

pub(crate) fn canon_repo(repo: String, canon: &Canon) -> String {
    canon.get(&repo.to_lowercase()).cloned().unwrap_or(repo)
}

/// `(blocked, open blockers, all blockers)`: a blocker is open when it is in `open`; a closed
/// one, or one in a repository not scanned, is resolved.
pub(crate) fn classify_q(
    issue: &Value,
    home_repo: Option<&str>,
    open: &HashSet<QualRef>,
    canon: &Canon,
) -> (bool, Vec<QualRef>, Vec<QualRef>) {
    let mut all: Vec<QualRef> = issue_blocking_refs_q(issue, home_repo)
        .into_iter()
        .map(|(r, n)| (r.map(|r| canon_repo(r, canon)), n))
        .collect();
    ref_sort(&mut all);
    let open_blocking: Vec<QualRef> = all.iter().filter(|r| open.contains(*r)).cloned().collect();
    if has_block_label(issue) || !open_blocking.is_empty() {
        return (true, open_blocking, all);
    }
    (false, Vec::new(), all)
}

/// Single-repo form: `("resolved" | "blocked", open blocking numbers)`.
pub fn classify_dependency(issue: &Value, open_refs: &[i64]) -> (String, Vec<i64>) {
    let open: HashSet<QualRef> = open_refs.iter().map(|&n| (None, n)).collect();
    let (blocked, open_blocking, _) = classify_q(issue, None, &open, &Canon::new());
    let status = if blocked { "blocked" } else { "resolved" };
    (
        status.to_string(),
        open_blocking.into_iter().map(|(_, n)| n).collect(),
    )
}

/// Whether `repo` is shown as the home repository: always for `None`, and for the one repository
/// of a single-repo scan (`collapse`).
pub(crate) fn is_home_disp(repo: Option<&str>, collapse: Option<&str>) -> bool {
    repo.is_none() || (collapse.is_some() && repo == collapse)
}

/// A ref as JSON: home as a bare number, cross as `"owner/repo#N"`.
pub fn ref_to_json(r: &QualRef, collapse: Option<&str>) -> Value {
    if is_home_disp(r.0.as_deref(), collapse) {
        Value::from(r.1)
    } else {
        Value::from(format!("{}#{}", r.0.as_deref().unwrap_or(""), r.1))
    }
}

/// A ref as text: `#N` (home) or `owner/repo#N`.
pub fn ref_to_disp(r: &QualRef, collapse: Option<&str>) -> String {
    if is_home_disp(r.0.as_deref(), collapse) {
        format!("#{}", r.1)
    } else {
        format!("{}#{}", r.0.as_deref().unwrap_or(""), r.1)
    }
}

#[cfg(test)]
mod tests {
    use super::{dependency_line_unread, dependency_line_with_no_number};

    const WORDS: &str = "Depends on the belt's step 2.";

    /// Every Markdown lead before the keyword is read through, by both readers (Issue 299).
    #[test]
    fn a_dependency_in_words_is_found_behind_any_markdown_lead() {
        for lead in [
            "",
            "  ",
            "> ",
            "## ",
            "- ",
            "* ",
            "+ ",
            "1. ",
            "2) ",
            "- [ ] ",
            "- [x] ",
            "* [X] ",
            "**",
            "***",
            "_",
            "__",
            "`",
            "> - [ ] **",
        ] {
            let body = format!("Intro.\n\n{lead}{WORDS}\n");
            assert!(
                dependency_line_with_no_number(&body).is_some(),
                "range: {body:?}"
            );
            assert!(dependency_line_unread(&body).is_some(), "triage: {body:?}");
        }
    }

    #[test]
    fn a_dependency_naming_an_issue_or_prose_is_not_unreadable() {
        for body in [
            "Depends on #282.",
            "- [ ] Depends on #282",
            "***Blocked by #5***",
            "The row budget depends on the window size.",
            "This requires a restart.",
        ] {
            assert_eq!(
                dependency_line_with_no_number(body),
                None,
                "range: {body:?}"
            );
            assert_eq!(dependency_line_unread(body), None, "triage: {body:?}");
        }
    }

    /// The triage reader keeps the next-line ref as its warning, not as unreadable.
    #[test]
    fn a_ref_on_the_next_line_is_the_triage_warning_not_unreadable() {
        assert_eq!(dependency_line_unread("Depends on\n\n#12\n"), None);
        assert!(dependency_line_with_no_number("Depends on\n#12").is_some());
    }
}
