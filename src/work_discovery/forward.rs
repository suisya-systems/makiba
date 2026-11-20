//! The keys that say whether two scans found the same thing (Issue 173, carried from
//! claude-org-ja `tools/work_discovery_dedup.py`). Comparing them with the last one sent is the
//! caller's (Issue 40).

use serde_json::Value;
use std::collections::BTreeSet;

/// The scan's keys: equal keys, nothing new to say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keys {
    /// The candidates as `owner/repo#N`, sorted, comma-joined.
    pub candidate_refs: String,
    /// The goal stage's findings as `unset:<repo>` / `error:<repo>` lines, sorted, comma-joined.
    pub diagnostics: String,
}

/// The candidates as `owner/repo#N`, sorted and comma-joined; a candidate shown as home
/// (`repo: null`) is under `fallback_repo`.
pub fn candidate_refs(scan: &Value, fallback_repo: &str) -> String {
    let refs: BTreeSet<String> = scan["candidates"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| {
            let repo = c["repo"].as_str().unwrap_or(fallback_repo);
            format!("{repo}#{}", c["issue"])
        })
        .collect();
    refs.into_iter().collect::<Vec<_>>().join(",")
}

/// Both keys of a scan.
pub fn scan_keys(scan: &Value, fallback_repo: &str) -> Keys {
    let gr = &scan["goal_rank"];
    let lines = |key: &str, tag: &str| -> Vec<String> {
        gr[key]
            .as_array()
            .into_iter()
            .flatten()
            .map(|e| format!("{tag}:{}", e["repo"].as_str().unwrap_or("")))
            .collect()
    };
    let diag: BTreeSet<String> = lines("goal_unset_repos", "unset")
        .into_iter()
        .chain(lines("goal_errors", "error"))
        .collect();
    Keys {
        candidate_refs: candidate_refs(scan, fallback_repo),
        diagnostics: diag.into_iter().collect::<Vec<_>>().join(","),
    }
}
