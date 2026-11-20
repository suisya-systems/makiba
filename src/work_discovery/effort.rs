//! Priority and effort, carried from claude-org-ja `tools/work_discovery_scan.py` (Issue 173).
//!
//! Priority comes from labels (label > milestone > recency, the level only from labels). Effort
//! comes from a size label, else from a learned model when its gate fires, else from the static
//! heuristic over body length and acceptance criteria. Every estimate says it is one.

use super::refs::label_names;
use regex::Regex;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::sync::LazyLock;

static PRIORITY_LABEL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:priority[:/\s-]*)?(high|medium|med|low|p0|p1|p2)$").unwrap()
});
static SIZE_LABEL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(?:size|effort)[:/\s-]*(s|m|l|xs|xl)$").unwrap());
static BARE_SIZE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(xs|s|m|l|xl)$").unwrap());
static CRITERIA: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?im)^[ \t]*[-*]\s*\[[ xX]\]").unwrap());

const LOW_PRIORITY_LABELS: [&str; 2] = ["backlog", "wontfix"];

/// Training pairs below this never override the static heuristic.
pub const MIN_EFFORT_SAMPLES: usize = 8;
/// Spearman correlation of body length with realized effort the model needs to override.
pub const MIN_EFFORT_CORRELATION: f64 = 0.3;
/// A changed file counts as this many changed lines in the realized-effort composite.
pub const EFFORT_FILE_WEIGHT: i64 = 20;

/// Python's `repr` of a float: `2.0`, not `2`.
pub(crate) fn py_float(x: f64) -> String {
    if x.is_finite() && x.fract() == 0.0 && x.abs() < 1e16 {
        format!("{x:.1}")
    } else {
        format!("{x}")
    }
}

/// A JSON value as Python's f-string would print it (`None`, `3`, `2.0`).
pub(crate) fn py_str(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => "None".into(),
        Some(Value::Number(n)) if n.is_f64() => py_float(n.as_f64().unwrap()),
        Some(Value::Bool(b)) => if *b { "True" } else { "False" }.into(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

/// Python's `round(x, nd)` (correctly rounded decimal).
pub(crate) fn py_round(x: f64, nd: usize) -> f64 {
    format!("{x:.nd$}").parse().unwrap_or(x)
}

/// `(level, signals)`: a priority label, else `backlog` / `wontfix` -> low, else medium. A
/// milestone does not change the level; it is a signal and a tie-break.
pub fn compute_priority(issue: &Value) -> (String, Vec<String>) {
    let labels = label_names(issue);
    for name in &labels {
        let Some(c) = PRIORITY_LABEL.captures(name) else {
            continue;
        };
        let level = match c[1].to_lowercase().as_str() {
            "high" | "p0" => "high",
            "medium" | "med" | "p1" => "medium",
            _ => "low",
        };
        return (level.into(), vec![format!("label:{name}")]);
    }
    if let Some(name) = labels
        .iter()
        .find(|n| LOW_PRIORITY_LABELS.contains(&n.as_str()))
    {
        return ("low".into(), vec![format!("label:{name}")]);
    }
    if let Some(ms) = milestone_title(issue) {
        return (
            "medium".into(),
            vec![format!("milestone:{ms} (level not promoted)")],
        );
    }
    (
        "medium".into(),
        vec!["no priority label/milestone -> default medium".into()],
    )
}

/// The milestone's title: gh's `{"title": ..}` or a plain string.
pub fn milestone_title(issue: &Value) -> Option<String> {
    match issue.get("milestone") {
        Some(Value::Object(o)) => o
            .get("title")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
            .map(str::to_string),
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

fn count_acceptance_criteria(body: &str) -> usize {
    CRITERIA.find_iter(body).count()
}

/// `(size S|M|L, estimated, signals)`. A size label is authoritative (`estimated` false).
pub fn estimate_effort(issue: &Value, model: Option<&Value>) -> (String, bool, Vec<String>) {
    for name in label_names(issue) {
        let Some(c) = SIZE_LABEL
            .captures(&name)
            .or_else(|| BARE_SIZE.captures(&name))
        else {
            continue;
        };
        let size = match c[1].to_uppercase().as_str() {
            "XS" | "S" => "S",
            "M" => "M",
            _ => "L",
        };
        return (size.into(), false, vec![format!("label:{name}")]);
    }
    let body = issue.get("body").and_then(Value::as_str).unwrap_or("");
    let length = body.chars().count();
    let criteria = count_acceptance_criteria(body);

    let applies = model.is_some_and(|m| m.get("applies") == Some(&Value::Bool(true)));
    let cuts = model
        .and_then(|m| m.get("predictor_cutpoints"))
        .and_then(Value::as_array)
        .filter(|c| c.len() == 2);
    if let (true, Some(cuts), Some(m)) = (applies, cuts, model) {
        let t1 = cuts[0].as_f64().unwrap_or(0.0);
        let t2 = cuts[1].as_f64().unwrap_or(0.0);
        let len = length as f64;
        let size = if len <= t1 {
            "S"
        } else if len <= t2 {
            "M"
        } else {
            "L"
        };
        let mut signals = vec![format!(
            "learned effort: body_len={length} vs repo-calibrated cutpoints S<={}<M<={}<L (n={}, rho={})",
            t1.round_ties_even(),
            t2.round_ties_even(),
            py_str(m.get("sample_size")),
            py_str(m.get("predictor_correlation")),
        )];
        if !m.get("realized_median_lines").is_none_or(Value::is_null) {
            signals.push(format!(
                "realized basis: recent merged tasks median {} changed lines / {} files",
                py_str(m.get("realized_median_lines")),
                py_str(m.get("realized_median_files")),
            ));
        }
        return (size.into(), true, signals);
    }

    let size = if length >= 2000 || criteria >= 8 {
        "L"
    } else if length < 600 && criteria <= 2 {
        "S"
    } else {
        "M"
    };
    let mut signals = vec![format!(
        "estimated effort from body_len={length}, acceptance_criteria={criteria}"
    )];
    if let Some(m) = model.filter(|_| !applies) {
        if let Some(reason) = m
            .get("reason")
            .and_then(Value::as_str)
            .filter(|r| !r.is_empty())
        {
            signals.push(format!("effort model not applied -- {reason}"));
        }
        if !m.get("realized_median_lines").is_none_or(Value::is_null) {
            signals.push(format!(
                "realized context: recent merged tasks median {} changed lines / {} files (n={})",
                py_str(m.get("realized_median_lines")),
                py_str(m.get("realized_median_files")),
                py_str(m.get("sample_size")),
            ));
        }
        for note in m
            .get("degenerate_signals")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            signals.push(format!("signal note: {}", py_str(Some(note))));
        }
    }
    (size.into(), true, signals)
}

/// Spearman rank correlation with average ranks for ties; 0 for degenerate input.
pub fn spearman(xs: &[f64], ys: &[f64]) -> f64 {
    let n = xs.len();
    if n < 2 || ys.len() != n {
        return 0.0;
    }
    let ranks = |vals: &[f64]| {
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by(|&a, &b| vals[a].total_cmp(&vals[b]));
        let mut ranks = vec![0.0; n];
        let mut i = 0;
        while i < n {
            let mut j = i;
            while j + 1 < n && vals[order[j + 1]] == vals[order[i]] {
                j += 1;
            }
            for k in i..=j {
                ranks[order[k]] = (i + j) as f64 / 2.0;
            }
            i = j + 1;
        }
        ranks
    };
    let (rx, ry) = (ranks(xs), ranks(ys));
    let nf = n as f64;
    let (mx, my) = (rx.iter().sum::<f64>() / nf, ry.iter().sum::<f64>() / nf);
    let cov: f64 = (0..n).map(|i| (rx[i] - mx) * (ry[i] - my)).sum();
    let vx = rx.iter().map(|r| (r - mx).powi(2)).sum::<f64>().sqrt();
    let vy = ry.iter().map(|r| (r - my).powi(2)).sum::<f64>().sqrt();
    if vx == 0.0 || vy == 0.0 {
        return 0.0;
    }
    cov / (vx * vy)
}

/// Two cut points splitting `values` into thirds (Python `statistics.quantiles(n=3)`,
/// exclusive method); `None` under three values.
pub fn tertile_cutpoints(values: &[f64]) -> Option<(f64, f64)> {
    let ld = values.len();
    if ld < 3 {
        return None;
    }
    let mut data = values.to_vec();
    data.sort_by(f64::total_cmp);
    let (n, m) = (3usize, ld + 1);
    let cut = |i: usize| {
        let j = (i * m / n).clamp(1, ld - 1);
        let delta = (i * m - j * n) as f64;
        (data[j - 1] * (n as f64 - delta) + data[j] * delta) / n as f64
    };
    Some((cut(1), cut(2)))
}

fn median(values: &[i64]) -> Value {
    let mut d = values.to_vec();
    d.sort();
    let n = d.len();
    if n % 2 == 1 {
        json!(d[n / 2])
    } else {
        json!((d[n / 2 - 1] + d[n / 2]) as f64 / 2.0)
    }
}

fn median_f(values: &[f64]) -> f64 {
    let mut d = values.to_vec();
    d.sort_by(f64::total_cmp);
    let n = d.len();
    if n % 2 == 1 {
        d[n / 2]
    } else {
        (d[n / 2 - 1] + d[n / 2]) / 2.0
    }
}

/// An effort model of the full shape that learned nothing and says why.
pub fn empty_effort_model(reason: &str) -> Value {
    json!({
        "sample_size": 0,
        "predictor": "issue_body_length",
        "predictor_correlation": null,
        "predictor_cutpoints": null,
        "realized_metric": format!("changed_lines + {EFFORT_FILE_WEIGHT}*changed_files"),
        "realized_cutpoints": null,
        "realized_median_lines": null,
        "realized_median_files": null,
        "realized_median_hours": null,
        "degenerate_signals": [],
        "coverage": null,
        "applies": false,
        "reason": reason,
    })
}

fn int_field(s: &Value, key: &str) -> i64 {
    s.get(key).and_then(Value::as_i64).unwrap_or(0)
}

/// Learn the repository's effort model from training pairs (`body_len`, `changed_lines`,
/// `changed_files`, `review_rounds`, `hours_to_merge`). `applies` only with enough pairs and a
/// body length that tracks realized effort. Independent of the order of `samples`.
pub fn learn_effort_model(samples: &[Value]) -> Value {
    let n = samples.len();
    if n == 0 {
        return empty_effort_model(
            "no linked (issue <-> merged-PR) training samples -> static heuristic",
        );
    }
    let body_lens: Vec<f64> = samples
        .iter()
        .map(|s| int_field(s, "body_len") as f64)
        .collect();
    let lines: Vec<i64> = samples
        .iter()
        .map(|s| int_field(s, "changed_lines"))
        .collect();
    let files: Vec<i64> = samples
        .iter()
        .map(|s| int_field(s, "changed_files"))
        .collect();
    let composites: Vec<f64> = (0..n)
        .map(|i| (lines[i] + EFFORT_FILE_WEIGHT * files[i]) as f64)
        .collect();
    let any_review = samples.iter().any(|s| int_field(s, "review_rounds") != 0);
    let hours: Vec<f64> = samples
        .iter()
        .filter_map(|s| s.get("hours_to_merge").and_then(Value::as_f64))
        .collect();

    let mut degenerate = Vec::new();
    if !any_review {
        degenerate.push(
            "review_rounds: all zero across samples (no GitHub reviews -- Codex local review) -> excluded from composite",
        );
    }
    degenerate.push(
        "time-to-merge: captured as context only, excluded from composite (dominated by queueing, not effort)",
    );

    let realized = tertile_cutpoints(&composites);
    let predictor = tertile_cutpoints(&body_lens);
    let rho = spearman(&body_lens, &composites);
    let min = MIN_EFFORT_CORRELATION;
    let applies =
        n >= MIN_EFFORT_SAMPLES && realized.is_some() && predictor.is_some() && rho >= min;
    let r2 = py_float(py_round(rho, 2));
    let reason = if applies {
        format!(
            "body length tracks realized effort (rho={r2} >= {min}, n={n}) -> learned cutpoints applied"
        )
    } else if n < MIN_EFFORT_SAMPLES {
        format!(
            "insufficient training pairs (n={n} < {MIN_EFFORT_SAMPLES}) -> static heuristic retained"
        )
    } else if realized.is_none() || predictor.is_none() {
        format!("could not form cutpoints (n={n}) -> static heuristic retained")
    } else {
        format!(
            "body length does not predict realized effort (rho={r2} < {min}, n={n}) -> static heuristic retained"
        )
    };
    let pair = |c: Option<(f64, f64)>| match c {
        Some((a, b)) => json!([py_round(a, 1), py_round(b, 1)]),
        None => Value::Null,
    };
    json!({
        "sample_size": n,
        "predictor": "issue_body_length",
        "predictor_correlation": py_round(rho, 3),
        "predictor_cutpoints": pair(predictor),
        "realized_metric": format!("changed_lines + {EFFORT_FILE_WEIGHT}*changed_files"),
        "realized_cutpoints": pair(realized),
        "realized_median_lines": median(&lines),
        "realized_median_files": median(&files),
        "realized_median_hours": if hours.is_empty() { Value::Null } else { json!(py_round(median_f(&hours), 2)) },
        "degenerate_signals": degenerate,
        "coverage": null,
        "applies": applies,
        "reason": reason,
    })
}

fn epoch_seconds(s: &str) -> Option<(f64, Option<i64>)> {
    let b = s.as_bytes();
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let t = s.get(r)?;
        t.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| t.parse().ok())?
    };
    if b.len() < 16
        || b[4] != b'-'
        || b[7] != b'-'
        || !matches!(b[10], b'T' | b' ')
        || b[13] != b':'
    {
        return None;
    }
    let (y, mo, d, h, mi) = (
        num(0..4)?,
        num(5..7)?,
        num(8..10)?,
        num(11..13)?,
        num(14..16)?,
    );
    let mut rest = &s[16..];
    let mut sec = 0.0;
    if let Some(r) = rest.strip_prefix(':') {
        let end = r
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .unwrap_or(r.len());
        sec = r[..end].parse().ok()?;
        rest = &r[end..];
    }
    let offset = match rest {
        "" => None,
        "Z" => Some(0),
        _ => {
            let sign = match rest.as_bytes()[0] {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let hh: i64 = rest.get(1..3)?.parse().ok()?;
            let mm: i64 = rest.get(4..6)?.parse().ok()?;
            Some(sign * (hh * 3600 + mm * 60))
        }
    };
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec >= 60.0 {
        return None;
    }
    // Days from the civil date (Howard Hinnant's algorithm).
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * (if mo > 2 { mo - 3 } else { mo + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    let t = (days * 86400 + h * 3600 + mi * 60) as f64 + sec - offset.unwrap_or(0) as f64;
    Some((t, offset))
}

/// Hours from `created` to `merged` (ISO 8601), at least 0; `None` when either is missing or
/// unreadable. Context only: it is not in the effort composite.
pub fn hours_between(created: Option<&str>, merged: Option<&str>) -> Option<f64> {
    let (c, co) = epoch_seconds(created.filter(|s| !s.is_empty())?)?;
    let (m, mo) = epoch_seconds(merged.filter(|s| !s.is_empty())?)?;
    if co.is_some() != mo.is_some() {
        return None;
    }
    Some(((m - c) / 3600.0).max(0.0))
}

fn closing_ref_repo(r: &Value) -> Option<String> {
    let rep = r.get("repository")?.as_object()?;
    let name = rep.get("name")?.as_str().filter(|s| !s.is_empty())?;
    let login = rep
        .get("owner")?
        .get("login")?
        .as_str()
        .filter(|s| !s.is_empty())?;
    Some(format!("{login}/{name}").to_lowercase())
}

/// The Issue a PR closes when it closes exactly one, in `home_repo` when that is given (a
/// reference with no repository counts as home).
pub fn single_closing_issue(pr: &Value, home_repo: Option<&str>) -> Option<i64> {
    let refs = pr.get("closingIssuesReferences")?.as_array()?;
    let [r] = refs.as_slice() else {
        return None;
    };
    r.as_object()?;
    if let (Some(home), Some(repo)) = (home_repo, closing_ref_repo(r))
        && repo != home.to_lowercase()
    {
        return None;
    }
    r.get("number").and_then(Value::as_i64)
}

/// Training pairs: merged PRs closing one home Issue, joined with that Issue's body. A PR whose
/// Issue body was not fetched or is empty is left out.
pub fn build_effort_samples(
    merged: &[Value],
    closed_bodies: &HashMap<i64, String>,
    home_repo: Option<&str>,
) -> Vec<Value> {
    let mut out = Vec::new();
    for pr in merged {
        let Some(num) = single_closing_issue(pr, home_repo) else {
            continue;
        };
        let Some(body) = closed_bodies.get(&num).filter(|b| !b.is_empty()) else {
            continue;
        };
        let int = |k: &str| pr.get(k).and_then(Value::as_i64).unwrap_or(0);
        let mut s = Map::new();
        s.insert("issue".into(), json!(num));
        s.insert("body_len".into(), json!(body.chars().count()));
        s.insert("criteria".into(), json!(count_acceptance_criteria(body)));
        s.insert(
            "changed_lines".into(),
            json!(int("additions") + int("deletions")),
        );
        s.insert("changed_files".into(), json!(int("changedFiles")));
        s.insert(
            "review_rounds".into(),
            json!(
                pr.get("reviews")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len)
            ),
        );
        s.insert(
            "hours_to_merge".into(),
            json!(hours_between(
                pr.get("createdAt").and_then(Value::as_str),
                pr.get("mergedAt").and_then(Value::as_str),
            )),
        );
        out.push(Value::Object(s));
    }
    out
}
