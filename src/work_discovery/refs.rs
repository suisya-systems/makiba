//! Dependency, close and reference notation, carried from claude-org-ja
//! `tools/work_discovery_scan.py` (Issue 173).
//!
//! Precision over recall: a keyword (`Blocked by`, `Depends on`, `Requires`) counts only with a run
//! of refs right after it on the same line, a negated keyword ("not blocked by #5") does not
//! count, and a task-list item counts only when it holds nothing but refs. A wrong exclusion is the
//! worse error, so when in doubt an Issue is not blocked.
//!
//! Dependencies have one parser, `dependency_lines` (Issue 356): the triage's blockers, the
//! guard that leaves out a dependency line it cannot read whole (Issue 299), and the range's
//! read (`src/daemon/belt.rs`) all read through it, so a line either names readable
//! dependencies or its Issue is left out as unreadable.

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

/// Home numbers this Issue is blocked by, sorted: the refs of `dependency_lines`. For a non-epic
/// an open task item `- [ ] #N` also counts.
pub fn extract_blocking_refs(text: Option<&str>, is_epic: bool) -> Vec<i64> {
    let text = text.unwrap_or("");
    let mut refs: BTreeSet<i64> = dependency_lines(text).home.into_iter().collect();
    if !is_epic {
        refs.extend(pure_task_items(text).into_iter().flat_map(home_refs));
    }
    refs.into_iter().collect()
}

/// Cross-repository blockers, sorted, with the same discipline as `extract_blocking_refs`.
pub fn extract_cross_repo_blocking_refs(text: Option<&str>, is_epic: bool) -> Vec<(String, i64)> {
    let text = text.unwrap_or("");
    let mut refs: BTreeSet<(String, i64)> = dependency_lines(text).cross.into_iter().collect();
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

// The one form a dependency is written in (Issue 356), read on a line with its emphasis and code
// marks taken out: an optional quote, heading, list marker and task box, one keyword, an optional
// colon, a run of `#N` or `owner/repo#N` joined by `,`, `and` or `&`, an optional final `.`, and
// nothing else.
re!(
    STRICT_FORM,
    format!(
        r"(?i)^\s*(?:>\s*)*(?:#{{1,6}}\s+|[-+]\s+|\d+[.)]\s+)?(?:\[[ xX]\]\s*)?(?:blocked\s+by|depends\s+on|requires)\s*:?\s*(?P<run>{r}(?:\s*,\s*(?:and\s+)?{r}|\s+and\s+{r}|\s*&\s*{r})*)\s*\.?\s*$",
        r = format!(r"(?:{OWNER_REPO})?#\d+")
    )
);
// A line that starts with a dependency keyword, after an optional quote, heading, list marker and
// task box (Issue 299), its marks taken out.
re!(
    DEP_LINE_START,
    r"(?i)^\s*(?:>\s*)*(?:#{1,6}\s+|[-+]\s+|\d+[.)]\s+)?(?:\[[ xX]\]\s*)?(?:blocked\s+by|depends\s+on|requires)\b"
);
// One ref token (home `#N`, `PR #N`, `owner/repo#N`, a GitHub URL), anywhere on a line.
re!(REF_TOKEN, format!(r"(?i){}", ref_token_any()));

fn shown_line(line: &str) -> String {
    line.trim().chars().take(120).collect()
}

/// `line` with its emphasis and code marks (`*`, `_`, `~`, backtick) taken out, except a mark
/// between two letters or digits (`my_repo`).
fn without_marks(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let inner = |i: usize| {
        i > 0
            && chars[i - 1].is_alphanumeric()
            && chars.get(i + 1).is_some_and(|c| c.is_alphanumeric())
    };
    chars
        .iter()
        .enumerate()
        .filter(|&(i, c)| !matches!(c, '*' | '_' | '~' | '`') || inner(i))
        .map(|(_, c)| *c)
        .collect()
}

/// What `dependency_lines` reads from a text (Issue 356).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct DepLines {
    /// Home `#N` named as dependencies, sorted, each once.
    pub home: Vec<i64>,
    /// `owner/repo#N` and GitHub URLs named as dependencies, sorted, each once.
    pub cross: Vec<(String, i64)>,
    /// The first dependency line (at most 120 chars) not written in the one form.
    pub unreadable: Option<String>,
}

/// The one parser of dependency lines (Issue 356), for the triage, the guard and the range.
///
/// Refs: the triage's reading (each non-negated `Blocked by`, `Depends on` or `Requires` and the
/// run of refs right after it on its line), and the run of each line written in the one form
/// (`STRICT_FORM`), so no blocker read before is lost.
///
/// Readable: a dependency is written in one form only. With its emphasis and code marks taken out,
/// a line that starts with a keyword (after an optional quote, heading, list marker and task box),
/// or that holds a keyword and a ref token anywhere, must be exactly that form: one keyword, an
/// optional colon, a run of refs, an optional final `.`. Any other such line (a dependency in
/// words, its number on the next line, a second keyword, a negation, a URL, prose around the refs)
/// is `unreadable`, and the Issue is left out until its author rewrites the line. A keyword in
/// prose with no ref on its line (`This requires a restart.`) is no dependency line.
pub fn dependency_lines(text: &str) -> DepLines {
    let mut home: BTreeSet<i64> = keyword_lead_runs(text, &BLOCK_KEYWORD)
        .into_iter()
        .flat_map(home_refs)
        .collect();
    let mut cross: BTreeSet<(String, i64)> = keyword_lead_runs(text, &BLOCK_KEYWORD)
        .into_iter()
        .flat_map(cross_tokens)
        .collect();
    let mut unreadable = None;
    for line in splitlines(text) {
        let plain = without_marks(line);
        if let Some(run) = STRICT_FORM.captures(&plain).and_then(|c| c.name("run")) {
            home.extend(home_refs(run.as_str()));
            cross.extend(cross_tokens(run.as_str()));
            continue;
        }
        let dependency_line = DEP_LINE_START.is_match(&plain)
            || (BLOCK_KEYWORD.is_match(&plain) && REF_TOKEN.is_match(&plain));
        if unreadable.is_none() && dependency_line {
            unreadable = Some(shown_line(line));
        }
    }
    DepLines {
        home: home.into_iter().collect(),
        cross: cross.into_iter().collect(),
        unreadable,
    }
}

/// The first dependency line of `body` not written in the one form, if any.
pub fn dependency_line_unreadable(body: &str) -> Option<String> {
    dependency_lines(body).unreadable
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
    use super::{dependency_line_unreadable, dependency_lines};

    const WORDS: &str = "Depends on the belt's step 2.";

    /// The leads of the one form: quote, heading, list marker, task box, and emphasis or code
    /// marks, which are taken out (Issues 299, 356).
    const LEADS: [&str; 18] = [
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
    ];

    /// A dependency in words is unreadable behind every lead; the one form is read behind it.
    #[test]
    fn the_one_form_is_read_behind_every_lead_and_words_are_not() {
        for lead in LEADS {
            let body = format!("Intro.\n\n{lead}{WORDS}\n");
            assert!(dependency_line_unreadable(&body).is_some(), "{body:?}");
            let numbered = format!("Intro.\n\n{lead}Depends on #30\n");
            let d = dependency_lines(&numbered);
            assert_eq!((d.home, d.unreadable), (vec![30], None), "{numbered:?}");
        }
    }

    /// The three keywords, with emphasis or code marks around any part, all read the same.
    #[test]
    fn every_keyword_and_its_marks_read_the_same() {
        for body in [
            "Blocked by #30",
            "Requires #30",
            "- Depends on #30",
            "> **Depends on #30**",
            "_Depends on_ #30",
            "**Requires** #30.",
            "`Blocked by` #30",
            "Depends on: #30",
        ] {
            let d = dependency_lines(body);
            assert_eq!((d.home, d.unreadable), (vec![30], None), "{body:?}");
        }
        for body in [
            "Depends on #3 and #4",
            "Depends on #3, #4 and o/r#5",
            "Blocked by #3, #4",
            "Requires #3 & #4.",
        ] {
            assert_eq!(dependency_line_unreadable(body), None, "{body:?}");
        }
        let d = dependency_lines("Depends on other/my_repo#7 and #8");
        assert_eq!(d.home, vec![8]);
        assert_eq!(d.cross, vec![("other/my_repo".to_string(), 7)]);
        assert_eq!(d.unreadable, None);
    }

    /// A keyword in prose with no ref is no dependency line.
    #[test]
    fn prose_with_no_ref_is_not_a_dependency_line() {
        for body in [
            "The row budget depends on the window size.",
            "This requires a restart.",
        ] {
            assert_eq!(dependency_lines(body), Default::default(), "{body:?}");
        }
    }

    /// Any other line holding a dependency keyword and a ref, or starting with a keyword, is
    /// unreadable; the refs the triage reads from it are still read (Issue 356, the one form).
    #[test]
    fn any_other_form_is_unreadable_and_keeps_its_refs() {
        for (body, home) in [
            ("Depends on\n#12", vec![]),
            ("## Depends on\n\n- #12\n", vec![]),
            ("- Depends on: the tokenizer work, see #80.", vec![]),
            ("Depends on #3, then see #9", vec![3]),
            ("Depends on the parser (#282)", vec![]),
            ("Depends on #3; requires the previous step", vec![3]),
            ("Blocked by #1; depends on #2", vec![1, 2]),
            ("This is blocked by #5 for now.", vec![5]),
            ("This _depends on_ #30", vec![]),
            ("Blocked by PR #55", vec![55]),
            ("Depends on https://github.com/o/r/issues/30", vec![]),
            ("Depends on https://GITHUB.COM/o/r/issues/30", vec![]),
            ("Requires a restart after the upgrade.", vec![]),
        ] {
            let d = dependency_lines(body);
            assert!(d.unreadable.is_some(), "{body:?}");
            assert_eq!(d.home, home, "{body:?}");
        }
    }

    /// A negation is no part of the one form: the line is unreadable, and a negated keyword
    /// names nothing.
    #[test]
    fn a_negated_keyword_is_unreadable_and_names_nothing() {
        for body in [
            "not blocked by #5",
            "Not blocked by #5",
            "no longer depends on #5",
            "not **blocked by** #5",
            "It is _not_ `blocked by` #5",
            "Depends on #3; not blocked by #4",
        ] {
            let d = dependency_lines(body);
            assert!(d.unreadable.is_some(), "{body:?}");
            assert!(!d.home.contains(&5) && !d.home.contains(&4), "{body:?}");
        }
    }

    /// Every line boundary `splitlines` knows separates dependency lines.
    #[test]
    fn every_line_boundary_separates_dependency_lines() {
        for sep in ["\r", "\r\n", "\u{2028}", "\u{85}", "\x0b"] {
            let body = format!("Intro.{sep}Depends on the previous step.");
            assert!(dependency_line_unreadable(&body).is_some(), "{body:?}");
            let numbered = format!("Intro.{sep}- Depends on #30{sep}More.");
            let d = dependency_lines(&numbered);
            assert_eq!((d.home, d.unreadable), (vec![30], None), "{numbered:?}");
        }
    }
}
