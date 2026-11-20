//! The work discovery scan as pure logic (Issue 173).
//!
//! The cases of claude-org-ja's `tests/test_work_discovery_scan.py`, `test_work_discovery_goals.py`
//! and `test_work_discovery_dedup.py` that concern the pure logic, ported. One module per carried
//! test class, the carried test names kept, so each case can be traced to its origin. A case
//! ported with a change says so in a `Changed:` comment; the rest are ported as they are. The last
//! module, `issue_173`, holds the Issue's own assertions: the order by the clause an Issue names,
//! the ledger gate, and the keys.
//!
//! Non-ASCII fixture text is written with `\u{..}` escapes (the same chars at run time).

use makiba::work_discovery::{
    self as wd, Bundle, Fetch, Limits, ScanConfig, Truncation,
    effort::{self, MIN_EFFORT_SAMPLES},
    forward, goals, refs,
};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::path::Path;

// ---- helpers --------------------------------------------------------------------------------

fn issue(n: i64, body: &str, labels: &[&str]) -> Value {
    json!({
        "number": n,
        "title": "t",
        "body": body,
        "labels": labels.iter().map(|l| json!({"name": l})).collect::<Vec<_>>(),
        "updatedAt": "2026-06-01T00:00:00Z",
    })
}

fn with(mut v: Value, key: &str, x: Value) -> Value {
    v[key] = x;
    v
}

fn cfg() -> ScanConfig {
    ScanConfig::default()
}

fn cfg_top(top_n: i64) -> ScanConfig {
    ScanConfig {
        top_n,
        ..ScanConfig::default()
    }
}

/// The carried `scan(issues, open_pr_numbers, recent_merges, ScanConfig())`.
fn scan(issues: Vec<Value>, prs: &[i64], merges: Vec<Value>) -> Value {
    wd::scan(
        issues,
        prs.to_vec(),
        merges,
        &cfg(),
        None,
        None,
        None,
        vec![],
    )
}

fn blocking(text: &str) -> Vec<i64> {
    refs::extract_blocking_refs(Some(text), false)
}

fn issues_of(result: &Value, key: &str) -> Vec<Value> {
    result[key]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["issue"].clone())
        .collect()
}

fn by_issue(result: &Value, key: &str) -> HashMap<i64, Value> {
    result[key]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| (e["issue"].as_i64().unwrap(), e.clone()))
        .collect()
}

fn any_has(signals: &[String], part: &str) -> bool {
    signals.iter().any(|s| s.contains(part))
}

fn signals_of(c: &Value) -> Vec<String> {
    c["signals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
        .collect()
}

fn bundle(repo: &str, issues: Vec<Value>) -> Bundle {
    Bundle {
        repo: Some(repo.into()),
        issues,
        ..Bundle::default()
    }
}

fn scan_repos(bundles: &[Bundle]) -> Value {
    wd::scan_repos(bundles, &cfg(), None, None, None, None)
}

// ---- TestBlockingRefExtraction ---------------------------------------------------------------

mod blocking_ref_extraction {
    use super::*;

    #[test]
    fn blocked_by_immediate_ref() {
        assert_eq!(blocking("Blocked by #482"), vec![482]);
    }

    #[test]
    fn depends_on_multiple_refs() {
        assert_eq!(
            blocking("Depends on #100, #101 and #102"),
            vec![100, 101, 102]
        );
    }

    #[test]
    fn requires_ref() {
        assert_eq!(blocking("Requires #7"), vec![7]);
    }

    #[test]
    fn pr_prefixed_ref() {
        assert_eq!(blocking("Blocked by PR #55"), vec![55]);
    }

    #[test]
    fn prose_depends_on_yields_no_ref() {
        // "Depends on:" describing unfiled commits, mentioning #80 deep in the prose: no ref.
        let body = "- Parent: #80\n- Design: PR #175\n- Depends on: Commit 1 (tokenizer) and \
                    Commit 2 follow-up Issues \u{2014} see follow-up Issues to be filed under #80.\n";
        assert!(blocking(body).is_empty());
    }

    #[test]
    fn non_blocking_notations_ignored() {
        let body = "Parent: #80\nDesign: PR #175\nRefs #500\nCloses #501\n\
                    Discovered while working on #480\nSee #999 for context.\n";
        assert!(blocking(body).is_empty());
    }

    #[test]
    fn task_list_ref_counts_for_non_epic() {
        assert_eq!(blocking("## Subtasks\n- [ ] #11\n- [x] #12\n"), vec![11]);
    }

    #[test]
    fn task_list_pure_ref_run_counts() {
        assert_eq!(
            blocking("- [ ] #11\n- [ ] #12, #13 and #14\n"),
            vec![11, 12, 13, 14]
        );
    }

    #[test]
    fn task_list_prose_after_ref_is_not_a_blocker() {
        // A ref with prose after it is a mention, not a dependency.
        let body = "- [ ] #123 \u{3092}\u{53c2}\u{8003}\u{306b}\u{78ba}\u{8a8d}\u{3059}\u{308b}\n\
                    - [ ] #124 review notes\n";
        assert!(blocking(body).is_empty());
    }

    #[test]
    fn task_list_prose_before_ref_is_not_a_blocker() {
        assert!(blocking("- [ ] Fix #11").is_empty());
    }

    #[test]
    fn task_list_mixed_pure_and_prose() {
        let body = "- [ ] #11\n- [ ] #99 \u{3092}\u{53c2}\u{8003}\u{306b}\n";
        assert_eq!(blocking(body), vec![11]);
    }

    #[test]
    fn task_list_ignored_for_epic() {
        let body = "## Children\n- [ ] #11\n- [ ] #12\n";
        assert!(refs::extract_blocking_refs(Some(body), true).is_empty());
    }

    #[test]
    fn empty_body() {
        assert!(refs::extract_blocking_refs(None, false).is_empty());
        assert!(blocking("").is_empty());
    }

    #[test]
    fn negated_not_blocked_by_yields_no_ref() {
        assert!(blocking("not blocked by #5").is_empty());
    }

    #[test]
    fn negated_no_longer_blocked_by_yields_no_ref() {
        assert!(blocking("no longer blocked by #5").is_empty());
    }

    #[test]
    fn unblocked_by_yields_no_ref() {
        assert!(blocking("unblocked by #5").is_empty());
    }

    #[test]
    fn negated_doesnt_depend_on_yields_no_ref() {
        assert!(blocking("This doesn't depend on #5").is_empty());
    }

    #[test]
    fn negation_only_when_adjacent_to_keyword() {
        assert_eq!(
            blocking("This is not a blocker, but blocked by #5"),
            vec![5]
        );
    }

    #[test]
    fn negation_with_intervening_adverb() {
        for s in [
            "not currently blocked by #5",
            "not yet blocked by #5",
            "doesn't currently depend on #5",
        ] {
            assert!(blocking(s).is_empty(), "{s}");
        }
    }

    #[test]
    fn far_away_negation_with_punctuation_still_blocks() {
        assert_eq!(
            blocking("We fixed it (not the UI). Now blocked by #5"),
            vec![5]
        );
    }

    #[test]
    fn multiple_clauses_on_one_line() {
        assert_eq!(blocking("Blocked by #1; depends on #2"), vec![1, 2]);
        assert_eq!(blocking("Requires #3 and Blocked by #4"), vec![3, 4]);
    }

    #[test]
    fn ref_on_next_line_not_linked_to_keyword() {
        assert!(blocking("Blocked by\n#5").is_empty());
    }
}

// ---- TestClassifyDependency ------------------------------------------------------------------

mod classify_dependency {
    use super::*;

    #[test]
    fn resolved_when_refs_all_closed() {
        let (status, open) = refs::classify_dependency(&issue(10, "Blocked by #5", &[]), &[]);
        assert_eq!(status, "resolved");
        assert!(open.is_empty());
    }

    #[test]
    fn blocked_when_ref_open() {
        let (status, open) = refs::classify_dependency(&issue(10, "Blocked by #5", &[]), &[5]);
        assert_eq!(status, "blocked");
        assert_eq!(open, vec![5]);
    }

    #[test]
    fn block_label_forces_blocked() {
        let (status, open) = refs::classify_dependency(&issue(10, "no refs", &["blocked"]), &[]);
        assert_eq!(status, "blocked");
        assert!(open.is_empty());
    }

    #[test]
    fn on_hold_label_forces_blocked() {
        let (status, _) = refs::classify_dependency(&issue(10, "", &["on-hold"]), &[]);
        assert_eq!(status, "blocked");
    }

    #[test]
    fn unknown_ref_treated_resolved() {
        let (status, _) =
            refs::classify_dependency(&issue(10, "Depends on #9999", &[]), &[1, 2, 3]);
        assert_eq!(status, "resolved");
    }
}

// ---- TestPriority ----------------------------------------------------------------------------

mod priority {
    use super::*;

    #[test]
    fn priority_high_label() {
        assert_eq!(
            effort::compute_priority(&issue(1, "", &["priority:high"])).0,
            "high"
        );
    }

    #[test]
    fn p0_label_high() {
        assert_eq!(effort::compute_priority(&issue(1, "", &["p0"])).0, "high");
    }

    #[test]
    fn backlog_label_low() {
        let (level, sig) = effort::compute_priority(&issue(1, "", &["backlog"]));
        assert_eq!(level, "low");
        assert!(any_has(&sig, "backlog"));
    }

    #[test]
    fn default_medium_without_labels() {
        let (level, sig) = effort::compute_priority(&issue(1, "", &["enhancement"]));
        assert_eq!(level, "medium");
        assert!(any_has(&sig, "default medium"));
    }
}

// ---- TestEffort ------------------------------------------------------------------------------

mod effort_static {
    use super::*;

    #[test]
    fn size_label_not_estimated() {
        let (size, est, sig) = effort::estimate_effort(&issue(1, "", &["size:L"]), None);
        assert_eq!(size, "L");
        assert!(!est);
        assert!(any_has(&sig, "label:size:l"));
    }

    #[test]
    fn bare_size_label() {
        let (size, est, _) = effort::estimate_effort(&issue(1, "", &["M"]), None);
        assert_eq!(size, "M");
        assert!(!est);
    }

    #[test]
    fn heuristic_small() {
        let (size, est, sig) = effort::estimate_effort(&issue(1, "short", &[]), None);
        assert_eq!(size, "S");
        assert!(est);
        assert!(any_has(&sig, "estimated effort"));
    }

    #[test]
    fn heuristic_large_by_length() {
        let (size, est, _) = effort::estimate_effort(&issue(1, &"x".repeat(2500), &[]), None);
        assert_eq!(size, "L");
        assert!(est);
    }

    #[test]
    fn heuristic_large_by_criteria() {
        let body = format!("intro\n{}", ["- [ ] item"; 9].join("\n"));
        let (size, est, _) = effort::estimate_effort(&issue(1, &body, &[]), None);
        assert_eq!(size, "L");
        assert!(est);
    }
}

// ---- TestEffortLearning ----------------------------------------------------------------------

mod effort_learning {
    use super::*;

    fn sample(body_len: i64, lines: i64, reviews: i64) -> Value {
        json!({
            "body_len": body_len, "criteria": 0, "changed_lines": lines,
            "changed_files": 1, "review_rounds": reviews, "hours_to_merge": 0.1,
        })
    }

    fn correlated(n: i64) -> Vec<Value> {
        (1..=n).map(|i| sample(100 * i, 30 * i, 0)).collect()
    }

    fn uncorrelated() -> Vec<Value> {
        let bodies = [100, 200, 300, 400, 500, 600, 700, 800, 900];
        let lines = [900, 100, 700, 200, 500, 300, 800, 50, 400];
        bodies
            .iter()
            .zip(lines)
            .map(|(&b, l)| sample(b, l, 0))
            .collect()
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-7
    }

    #[test]
    fn spearman_perfect_positive() {
        assert!(close(
            effort::spearman(&[1.0, 2.0, 3.0], &[10.0, 20.0, 30.0]),
            1.0
        ));
    }

    #[test]
    fn spearman_perfect_negative() {
        assert!(close(
            effort::spearman(&[1.0, 2.0, 3.0], &[30.0, 20.0, 10.0]),
            -1.0
        ));
    }

    #[test]
    fn spearman_zero_variance_is_zero() {
        assert_eq!(effort::spearman(&[5.0, 5.0, 5.0], &[1.0, 2.0, 3.0]), 0.0);
    }

    #[test]
    fn spearman_degenerate_lengths() {
        assert_eq!(effort::spearman(&[1.0], &[1.0]), 0.0);
        assert_eq!(effort::spearman(&[1.0, 2.0], &[1.0]), 0.0);
    }

    #[test]
    fn spearman_handles_ties() {
        assert!(effort::spearman(&[1.0, 1.0, 2.0, 3.0], &[1.0, 1.0, 2.0, 3.0]) > 0.9);
    }

    #[test]
    fn tertile_cutpoints_none_below_three() {
        assert!(effort::tertile_cutpoints(&[1.0, 2.0]).is_none());
    }

    #[test]
    fn tertile_cutpoints_two_values() {
        let (a, b) = effort::tertile_cutpoints(&[0.0, 30.0, 60.0, 90.0]).unwrap();
        assert!(a < b);
    }

    #[test]
    fn empty_samples_not_applied() {
        let m = effort::learn_effort_model(&[]);
        assert_eq!(m["sample_size"], 0);
        assert_eq!(m["applies"], false);
        assert!(m["reason"].as_str().unwrap().contains("no linked"));
    }

    #[test]
    fn correlated_above_gate_applies() {
        let m = effort::learn_effort_model(&correlated(9));
        assert_eq!(m["applies"], true);
        assert_eq!(m["sample_size"], 9);
        assert!(!m["predictor_cutpoints"].is_null());
        assert!(m["predictor_correlation"].as_f64().unwrap() >= 0.3);
        assert!(
            m["reason"]
                .as_str()
                .unwrap()
                .contains("tracks realized effort")
        );
    }

    #[test]
    fn uncorrelated_declines_even_with_samples() {
        let m = effort::learn_effort_model(&uncorrelated());
        assert!(m["sample_size"].as_u64().unwrap() as usize >= MIN_EFFORT_SAMPLES);
        assert_eq!(m["applies"], false);
        assert!(m["reason"].as_str().unwrap().contains("does not predict"));
    }

    #[test]
    fn insufficient_samples_declines() {
        let m = effort::learn_effort_model(&correlated(4));
        assert_eq!(m["applies"], false);
        assert!(m["reason"].as_str().unwrap().contains("insufficient"));
    }

    #[test]
    fn realized_context_always_reported() {
        let m = effort::learn_effort_model(&uncorrelated());
        assert!(!m["realized_median_lines"].is_null());
        assert!(!m["realized_cutpoints"].is_null());
        assert!(
            m["realized_metric"]
                .as_str()
                .unwrap()
                .starts_with("changed_lines")
        );
    }

    #[test]
    fn degenerate_review_rounds_noted() {
        let m = effort::learn_effort_model(&correlated(9));
        let notes = m["degenerate_signals"].to_string();
        assert!(notes.contains("review_rounds"));
    }

    #[test]
    fn review_rounds_present_not_flagged_degenerate() {
        let samples: Vec<Value> = (1..10).map(|i| sample(100 * i, 30 * i, i % 2)).collect();
        let m = effort::learn_effort_model(&samples);
        assert!(
            !m["degenerate_signals"]
                .to_string()
                .contains("review_rounds")
        );
    }

    #[test]
    fn model_order_independent() {
        let s = correlated(9);
        let mut r = s.clone();
        r.reverse();
        assert_eq!(
            effort::learn_effort_model(&s),
            effort::learn_effort_model(&r)
        );
    }

    #[test]
    fn applied_model_uses_learned_cutpoints() {
        let m = effort::learn_effort_model(&correlated(9));
        let t1 = m["predictor_cutpoints"][0].as_f64().unwrap();
        let t2 = m["predictor_cutpoints"][1].as_f64().unwrap();
        let small =
            effort::estimate_effort(&issue(1, &"x".repeat((t1 - 1.0) as usize), &[]), Some(&m));
        let large =
            effort::estimate_effort(&issue(1, &"x".repeat((t2 + 50.0) as usize), &[]), Some(&m));
        assert_eq!(small.0, "S");
        assert_eq!(large.0, "L");
        assert!(small.1);
        assert!(any_has(&small.2, "learned effort"));
    }

    #[test]
    fn label_authoritative_even_with_model() {
        let m = effort::learn_effort_model(&correlated(9));
        let (size, est, _) = effort::estimate_effort(&issue(1, "", &["size:L"]), Some(&m));
        assert_eq!(size, "L");
        assert!(!est);
    }

    #[test]
    fn declined_model_keeps_static_and_discloses() {
        let m = effort::learn_effort_model(&uncorrelated());
        let (size, est, sig) = effort::estimate_effort(&issue(1, "short", &[]), Some(&m));
        assert_eq!(size, "S");
        assert!(est);
        assert!(any_has(&sig, "estimated effort from body_len"));
        assert!(any_has(&sig, "effort model not applied"));
        assert!(any_has(&sig, "realized context"));
    }

    #[test]
    fn no_model_is_pure_static() {
        let a = effort::estimate_effort(&issue(1, "short", &[]), None);
        let b = effort::estimate_effort(&issue(1, "short", &[]), None);
        assert_eq!(a, b);
    }

    #[test]
    fn learning_signals_ascii_only() {
        let models = [
            effort::learn_effort_model(&[]),
            effort::learn_effort_model(&correlated(9)),
            effort::learn_effort_model(&correlated(4)),
            effort::learn_effort_model(&uncorrelated()),
        ];
        for m in &models {
            assert!(m["reason"].as_str().unwrap().is_ascii());
            for d in m["degenerate_signals"].as_array().unwrap() {
                assert!(d.as_str().unwrap().is_ascii());
            }
            for body in ["short".to_string(), "x".repeat(5000)] {
                let (_, _, sig) = effort::estimate_effort(&issue(1, &body, &[]), Some(m));
                assert!(sig.iter().all(|s| s.is_ascii()), "{sig:?}");
            }
        }
    }

    fn bodies(pairs: &[(i64, &str)]) -> HashMap<i64, String> {
        pairs.iter().map(|(n, b)| (*n, b.to_string())).collect()
    }

    #[test]
    fn build_samples_single_issue_join() {
        let prs = vec![json!({
            "closingIssuesReferences": [{"number": 5}],
            "additions": 10, "deletions": 5, "changedFiles": 2, "reviews": [],
            "createdAt": "2026-06-01T00:00:00Z", "mergedAt": "2026-06-01T02:00:00Z",
        })];
        let s = effort::build_effort_samples(&prs, &bodies(&[(5, "issue body text")]), None);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0]["changed_lines"], 15);
        assert_eq!(s[0]["changed_files"], 2);
        assert_eq!(s[0]["hours_to_merge"], 2.0);
        assert_eq!(s[0]["body_len"], "issue body text".len());
    }

    #[test]
    fn build_samples_skips_multi_issue_pr() {
        let prs = vec![json!({"closingIssuesReferences": [{"number": 5}, {"number": 6}]})];
        let b = bodies(&[(5, "b"), (6, "b")]);
        assert!(effort::build_effort_samples(&prs, &b, None).is_empty());
    }

    #[test]
    fn build_samples_skips_unlinked_and_empty_body() {
        let prs = vec![
            json!({"closingIssuesReferences": []}),
            json!({"closingIssuesReferences": [{"number": 9}]}),
            json!({"closingIssuesReferences": [{"number": 7}]}),
        ];
        assert!(effort::build_effort_samples(&prs, &bodies(&[(7, "")]), None).is_empty());
    }

    fn pr_closing(num: i64, repo: Option<(&str, &str)>) -> Value {
        let mut r = json!({"number": num});
        if let Some((owner, name)) = repo {
            r["repository"] = json!({"name": name, "owner": {"login": owner}});
        }
        json!({
            "closingIssuesReferences": [r],
            "additions": 10, "deletions": 5, "changedFiles": 2, "reviews": [],
            "createdAt": "2026-06-01T00:00:00Z", "mergedAt": "2026-06-01T01:00:00Z",
        })
    }

    #[test]
    fn build_samples_excludes_cross_repo_closing() {
        let prs = vec![pr_closing(
            5,
            Some(("suisya-systems", "claude-org-runtime")),
        )];
        let s = effort::build_effort_samples(
            &prs,
            &bodies(&[(5, "home issue body")]),
            Some("suisya-systems/claude-org-ja"),
        );
        assert!(s.is_empty());
    }

    #[test]
    fn build_samples_same_number_two_repos() {
        let prs = vec![
            pr_closing(5, Some(("suisya-systems", "claude-org-ja"))),
            pr_closing(5, Some(("suisya-systems", "claude-org-runtime"))),
        ];
        let s = effort::build_effort_samples(
            &prs,
            &bodies(&[(5, "home issue body")]),
            Some("suisya-systems/claude-org-ja"),
        );
        assert_eq!(s.len(), 1);
        assert_eq!(s[0]["issue"], 5);
        assert_eq!(s[0]["body_len"], "home issue body".len());
    }

    #[test]
    fn build_samples_same_repo_closing_unchanged() {
        let prs = vec![pr_closing(5, Some(("Owner", "Repo")))];
        let s = effort::build_effort_samples(
            &prs,
            &bodies(&[(5, "issue body text")]),
            Some("owner/repo"),
        );
        assert_eq!(s.len(), 1);
        assert_eq!(s[0]["changed_lines"], 15);
    }

    #[test]
    fn build_samples_missing_repository_field_treated_as_home() {
        let prs = vec![pr_closing(5, None)];
        let s = effort::build_effort_samples(&prs, &bodies(&[(5, "b")]), Some("owner/repo"));
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn single_closing_issue_cross_repo_returns_none() {
        let pr = pr_closing(7, Some(("other", "repo")));
        assert_eq!(effort::single_closing_issue(&pr, Some("owner/repo")), None);
        assert_eq!(effort::single_closing_issue(&pr, None), Some(7));
    }

    #[test]
    fn hours_between_basic_and_bad() {
        assert_eq!(
            effort::hours_between(Some("2026-06-01T00:00:00Z"), Some("2026-06-01T03:30:00Z")),
            Some(3.5)
        );
        assert_eq!(
            effort::hours_between(None, Some("2026-06-01T03:30:00Z")),
            None
        );
        assert_eq!(
            effort::hours_between(Some("garbage"), Some("also-bad")),
            None
        );
    }

    #[test]
    fn scan_echoes_effort_model() {
        let m = effort::learn_effort_model(&uncorrelated());
        let r = wd::scan(
            vec![issue(1, "b", &[])],
            vec![],
            vec![],
            &cfg(),
            None,
            Some(&m),
            None,
            vec![],
        );
        assert_eq!(r["effort_model"]["sample_size"], m["sample_size"]);
    }

    #[test]
    fn scan_effort_model_none_by_default() {
        let r = scan(vec![issue(1, "b", &[])], &[], vec![]);
        assert!(r["effort_model"].is_null());
    }

    #[test]
    fn scan_applied_model_changes_candidate_effort() {
        let m = effort::learn_effort_model(&correlated(9));
        let t2 = m["predictor_cutpoints"][1].as_f64().unwrap();
        let big = issue(1, &"x".repeat((t2 + 100.0) as usize), &[]);
        let r = wd::scan(
            vec![big],
            vec![],
            vec![],
            &cfg(),
            None,
            Some(&m),
            None,
            vec![],
        );
        assert_eq!(r["candidates"][0]["effort"], "L");
    }
}

// ---- TestEstimatedAxes -----------------------------------------------------------------------

mod estimated_axes {
    use super::*;

    #[test]
    fn parallelizable_leaf() {
        let (ok, sig) = wd::estimate_parallelizable(&[]);
        assert!(ok);
        assert!(any_has(&sig, "leaf"));
    }

    #[test]
    fn not_parallelizable_with_open_refs() {
        let (ok, sig) = wd::estimate_parallelizable(&[5, 6]);
        assert!(!ok);
        assert!(any_has(&sig, "#5"));
    }

    #[test]
    fn unblocked_by_recent_merge_via_blocking_ref() {
        let (ok, sig) =
            wd::estimate_unblocked_by_recent_merge(&issue(10, "", &[]), &[200], &[200], &[], &[]);
        assert!(ok);
        assert!(any_has(&sig, "#200"));
    }

    #[test]
    fn unblocked_by_recent_merge_via_pr_link() {
        let (ok, _) =
            wd::estimate_unblocked_by_recent_merge(&issue(10, "", &[]), &[], &[], &[], &[10]);
        assert!(ok);
    }

    #[test]
    fn no_recent_merge_linkage() {
        let (ok, sig) =
            wd::estimate_unblocked_by_recent_merge(&issue(10, "", &[]), &[1], &[], &[], &[]);
        assert!(!ok);
        assert!(any_has(&sig, "no recent-merge"));
    }
}

// ---- TestSummary -----------------------------------------------------------------------------

mod summary {
    use super::*;

    #[test]
    fn skips_headings_and_quotes() {
        let body = "# Title\n\n> quote\n\nThe real first line.\n";
        assert_eq!(
            wd::extract_summary(Some(body), "fallback"),
            "The real first line."
        );
    }

    #[test]
    fn falls_back_to_title() {
        assert_eq!(wd::extract_summary(Some("# only heading\n"), "T"), "T");
    }

    #[test]
    fn truncates_long_line() {
        let out = wd::extract_summary(Some(&"w ".repeat(200)), "t");
        assert!(out.chars().count() <= 121);
        assert!(out.ends_with('\u{2026}'));
    }
}

// ---- TestRankingAndScan ----------------------------------------------------------------------

mod ranking_and_scan {
    use super::*;

    #[test]
    fn priority_beats_effort() {
        let issues = vec![
            issue(1, "short", &["backlog"]),
            issue(2, &"x".repeat(3000), &["priority:high"]),
        ];
        assert_eq!(scan(issues, &[], vec![])["candidates"][0]["issue"], 2);
    }

    #[test]
    fn recent_merge_ranks_above_plain() {
        let issues = vec![issue(1, "plain", &[]), issue(2, "Refs nothing", &[])];
        let merges = vec![json!({"number": 900, "title": "x", "body": "Closes #2"})];
        let r = scan(issues, &[], merges);
        assert_eq!(r["candidates"][0]["issue"], 2);
        assert_eq!(r["candidates"][0]["unblocked_by_recent_merge"], true);
    }

    #[test]
    fn truncation_reported() {
        let issues = (1..8).map(|i| issue(i, "b", &[])).collect();
        let r = wd::scan(
            issues,
            vec![],
            vec![],
            &cfg_top(3),
            None,
            None,
            None,
            vec![],
        );
        assert_eq!(r["candidate_count"], 3);
        assert_eq!(r["truncated_count"], 4);
    }

    #[test]
    fn blocked_excluded_with_reason_not_silent() {
        // Changed: the note is English ("#5 still open").
        let r = scan(
            vec![issue(10, "Blocked by #5", &[]), issue(5, "open dep", &[])],
            &[],
            vec![],
        );
        let excluded = by_issue(&r, "excluded_blocked");
        assert_eq!(excluded[&10]["blocking_refs"], json!([5]));
        assert_eq!(excluded[&10]["note"], "#5 still open");
    }

    #[test]
    fn no_candidates_status() {
        let r = scan(vec![issue(10, "Blocked by #5", &["blocked"])], &[], vec![]);
        assert_eq!(r["status"], "no_candidates");
        assert_eq!(r["candidate_count"], 0);
    }

    #[test]
    fn recommendation_is_rank_1() {
        let r = scan(
            vec![issue(1, "plain", &[]), issue(2, "b", &["priority:high"])],
            &[],
            vec![],
        );
        assert_eq!(r["recommendation"]["issue"], 2);
        assert!(!r["recommendation"]["reason"].as_str().unwrap().is_empty());
    }

    #[test]
    fn estimated_flags_always_present() {
        let r = scan(vec![issue(1, "b", &[])], &[], vec![]);
        let c = &r["candidates"][0];
        for k in [
            "effort_estimated",
            "parallelizable_estimated",
            "unblocked_by_recent_merge_estimated",
        ] {
            assert!(c.get(k).is_some(), "{k}");
        }
        assert_eq!(c["parallelizable_estimated"], true);
        assert!(!signals_of(c).is_empty());
    }

    #[test]
    fn generated_for_propagates() {
        let config = ScanConfig {
            trigger: "post_merge".into(),
            ..cfg()
        };
        let r = wd::scan(
            vec![issue(1, "b", &[])],
            vec![],
            vec![],
            &config,
            None,
            None,
            None,
            vec![],
        );
        assert_eq!(r["generated_for"], "post_merge");
    }

    #[test]
    fn internal_fields_stripped() {
        let r = scan(vec![issue(1, "b", &[])], &[], vec![]);
        assert!(r["candidates"][0].get("_updated_at").is_none());
    }

    #[test]
    fn determinism_same_input_same_output() {
        let issues = vec![
            issue(3, "a", &["priority:high"]),
            issue(1, "b", &[]),
            issue(2, "c", &["backlog"]),
        ];
        assert_eq!(scan(issues.clone(), &[], vec![]), scan(issues, &[], vec![]));
    }

    fn cand(number: i64, parallelizable: bool) -> Map<String, Value> {
        json!({
            "issue": number, "priority": "medium", "effort": "M",
            "parallelizable": parallelizable, "unblocked_by_recent_merge": false,
            "_updated_at": "2026-06-01T00:00:00Z",
        })
        .as_object()
        .unwrap()
        .clone()
    }

    #[test]
    fn free_panes_positive_boosts_parallelizable() {
        let (top, _) = wd::rank_candidates(vec![cand(1, false), cand(2, true)], 3, Some(2));
        assert_eq!(top[0]["issue"], 2);
    }

    #[test]
    fn free_panes_zero_neutralizes_parallel_boost() {
        let (top, _) = wd::rank_candidates(vec![cand(1, false), cand(2, true)], 3, Some(0));
        assert_eq!(top[0]["issue"], 1);
    }

    #[test]
    fn free_panes_none_does_not_boost() {
        let (top, _) = wd::rank_candidates(vec![cand(1, false), cand(2, true)], 3, None);
        assert_eq!(top[0]["issue"], 1);
    }
}

// ---- TestCommentsAndMilestone ----------------------------------------------------------------

mod comments_and_milestone {
    use super::*;

    fn set_of(v: impl IntoIterator<Item = i64>) -> Vec<i64> {
        v.into_iter().collect()
    }

    fn unblocked(r: &Value) -> HashMap<i64, bool> {
        r["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| {
                (
                    c["issue"].as_i64().unwrap(),
                    c["unblocked_by_recent_merge"].as_bool().unwrap(),
                )
            })
            .collect()
    }

    fn merge(body: &str) -> Value {
        json!({"number": 900, "title": "x", "body": body})
    }

    #[test]
    fn blocker_in_comment_detected() {
        let i = json!({
            "number": 10, "title": "t", "body": "no blocker in body", "labels": [],
            "comments": [{"body": "Update: Blocked by #5 now."}],
            "updatedAt": "2026-06-01T00:00:00Z",
        });
        let (status, open) = refs::classify_dependency(&i, &[5]);
        assert_eq!(status, "blocked");
        assert_eq!(open, vec![5]);
    }

    #[test]
    fn comment_blocker_excluded_in_scan() {
        let i = json!({
            "number": 10, "title": "t", "body": "b", "labels": [],
            "comments": [{"body": "Depends on #5"}], "updatedAt": "2026-06-01T00:00:00Z",
        });
        let r = scan(vec![i, issue(5, "open dep", &[])], &[], vec![]);
        assert!(by_issue(&r, "excluded_blocked").contains_key(&10));
    }

    #[test]
    fn unblocked_via_ref_closed_by_recent_merge() {
        let (ok, sig) = wd::estimate_unblocked_by_recent_merge(
            &issue(10, "", &[]),
            &[100],
            &[],
            &[100],
            &[100],
        );
        assert!(ok);
        assert!(any_has(&sig, "#100"));
    }

    #[test]
    fn unblocked_via_closed_ref_in_full_scan() {
        let r = scan(
            vec![issue(10, "Depends on #100", &[])],
            &[],
            vec![merge("Closes #100")],
        );
        assert_eq!(r["candidates"][0]["unblocked_by_recent_merge"], true);
    }

    #[test]
    fn bare_refs_does_not_close_blocking_ref() {
        let (ok, _) =
            wd::estimate_unblocked_by_recent_merge(&issue(10, "", &[]), &[100], &[], &[], &[100]);
        assert!(!ok);
    }

    #[test]
    fn bare_refs_negative_in_full_scan() {
        let r = scan(
            vec![issue(10, "Depends on #100", &[])],
            &[],
            vec![merge("Refs #100")],
        );
        assert_eq!(r["candidates"][0]["unblocked_by_recent_merge"], false);
    }

    fn close(t: &str) -> Vec<i64> {
        set_of(refs::pr_close_refs(t))
    }

    fn referenced(t: &str) -> Vec<i64> {
        set_of(refs::pr_referenced_refs(t))
    }

    #[test]
    fn pr_close_refs_single() {
        assert_eq!(close("Closes #100"), vec![100]);
    }

    #[test]
    fn pr_close_refs_comma_separated() {
        assert_eq!(close("Closes #100, #101"), vec![100, 101]);
    }

    #[test]
    fn pr_close_refs_space_separated() {
        assert_eq!(close("Fixes #100 #101 #102"), vec![100, 101, 102]);
    }

    #[test]
    fn pr_close_refs_and_separated() {
        assert_eq!(close("Resolves #100, #101 and #102"), vec![100, 101, 102]);
    }

    #[test]
    fn pr_close_refs_mixed_case_keywords() {
        assert_eq!(
            close("CLOSED #100, #101\nfix #200 and #201"),
            vec![100, 101, 200, 201]
        );
    }

    #[test]
    fn pr_close_refs_bare_ref_not_captured() {
        assert!(close("Refs #100, #101").is_empty());
    }

    #[test]
    fn pr_close_refs_negated_does_not_close() {
        assert!(close("This does not close #100").is_empty());
        assert!(close("no longer closes #5").is_empty());
    }

    #[test]
    fn pr_close_refs_mixed_affirmed_and_negated() {
        assert_eq!(close("Closes #100 but does not close #101"), vec![100]);
    }

    #[test]
    fn recent_merge_negated_close_not_unblocking() {
        let r = scan(
            vec![issue(10, "Depends on #100", &[])],
            &[],
            vec![merge("This does not close #100")],
        );
        assert_eq!(r["candidates"][0]["unblocked_by_recent_merge"], false);
    }

    #[test]
    fn multi_issue_close_in_full_scan() {
        let r = scan(
            vec![
                issue(10, "Depends on #100", &[]),
                issue(11, "Depends on #101", &[]),
            ],
            &[],
            vec![merge("Closes #100, #101")],
        );
        let u = unblocked(&r);
        assert!(u[&10] && u[&11]);
    }

    #[test]
    fn pr_referenced_refs_comma_separated() {
        assert_eq!(referenced("Refs #10, #11"), vec![10, 11]);
    }

    #[test]
    fn pr_referenced_refs_and_separated() {
        assert_eq!(referenced("Ref #10 and #11 & #12"), vec![10, 11, 12]);
    }

    #[test]
    fn pr_close_refs_colon_form() {
        assert_eq!(close("Closes: #1"), vec![1]);
        assert_eq!(close("Fixes: #1, #2"), vec![1, 2]);
    }

    #[test]
    fn pr_referenced_refs_colon_form() {
        assert_eq!(referenced("Refs: #9"), vec![9]);
    }

    #[test]
    fn multi_ref_in_full_scan() {
        let r = scan(
            vec![issue(10, "x", &[]), issue(11, "y", &[])],
            &[],
            vec![json!({"number": 900, "title": "z", "body": "Refs #10, #11"})],
        );
        let u = unblocked(&r);
        assert!(u[&10] && u[&11]);
    }

    #[test]
    fn milestone_emitted_as_signal() {
        let i = with(issue(1, "b", &[]), "milestone", json!({"title": "v1.0"}));
        let (level, sig) = effort::compute_priority(&i);
        assert_eq!(level, "medium");
        assert!(any_has(&sig, "milestone:v1.0"));
    }

    #[test]
    fn milestone_breaks_tie_above_no_milestone() {
        let b = with(issue(2, "b", &[]), "milestone", json!({"title": "v1.0"}));
        let r = scan(vec![issue(1, "b", &[]), b], &[], vec![]);
        assert_eq!(r["candidates"][0]["issue"], 2);
    }

    #[test]
    fn milestone_internal_field_stripped() {
        let i = with(issue(1, "b", &[]), "milestone", json!({"title": "v1.0"}));
        let r = scan(vec![i], &[], vec![]);
        assert!(r["candidates"][0].get("_has_milestone").is_none());
    }
}

// ---- the fetch seam (TestFetchRecentMerges, TestFetchEffortHistory) --------------------------

/// A `Fetch` that answers from rows per read and records what it was asked.
#[derive(Default)]
struct Stub {
    rows: HashMap<&'static str, Vec<Value>>,
    calls: Vec<(String, String, usize)>,
}

impl Stub {
    fn with(mut self, read: &'static str, rows: Vec<Value>) -> Stub {
        self.rows.insert(read, rows);
        self
    }

    fn answer(&mut self, read: &str, repo: &str, limit: usize) -> Result<Vec<Value>, String> {
        self.calls.push((read.to_string(), repo.to_string(), limit));
        let key = read.split(':').next().unwrap();
        Ok(self.rows.get(key).cloned().unwrap_or_default())
    }

    fn limit_of(&self, read: &str) -> usize {
        self.calls.iter().find(|c| c.0 == read).unwrap().2
    }
}

impl Fetch for Stub {
    fn open_issues(&mut self, repo: &str, limit: usize) -> Result<Vec<Value>, String> {
        self.answer("open_issues", repo, limit)
    }
    fn open_prs(&mut self, repo: &str, limit: usize) -> Result<Vec<Value>, String> {
        self.answer("open_prs", repo, limit)
    }
    fn recent_merges(&mut self, repo: &str, limit: usize) -> Result<Vec<Value>, String> {
        self.answer("recent_merges", repo, limit)
    }
    fn base_merges(&mut self, repo: &str, base: &str, limit: usize) -> Result<Vec<Value>, String> {
        self.answer(&format!("base_merges:{base}"), repo, limit)
    }
    fn effort_history(&mut self, repo: &str, limit: usize) -> Result<Vec<Value>, String> {
        self.answer("effort_history", repo, limit)
    }
    fn closed_issues(&mut self, repo: &str, limit: usize) -> Result<Vec<Value>, String> {
        self.answer("closed_issues", repo, limit)
    }
}

fn numbers(rows: &[Value]) -> Vec<i64> {
    rows.iter().map(|r| r["number"].as_i64().unwrap()).collect()
}

fn monthly_pool() -> Vec<Value> {
    (1..10)
        .map(|n| json!({"number": n, "mergedAt": format!("2026-{n:02}-01T00:00:00Z")}))
        .collect()
}

mod fetch_recent_merges {
    use super::*;

    fn unsorted() -> Vec<Value> {
        vec![
            json!({"number": 1, "title": "a", "body": "", "mergedAt": "2026-01-01T00:00:00Z"}),
            json!({"number": 3, "title": "c", "body": "", "mergedAt": "2026-03-01T00:00:00Z"}),
            json!({"number": 2, "title": "b", "body": "", "mergedAt": "2026-02-01T00:00:00Z"}),
        ]
    }

    fn recent(stub: &mut Stub, k: usize) -> Vec<Value> {
        let limits = Limits {
            recent_merges: k,
            ..Limits::default()
        };
        wd::gather(stub, "owner/repo", None, &limits)
            .unwrap()
            .0
            .recent_merges
    }

    #[test]
    fn server_side_recency_ordering_requested() {
        // Changed: the over-fetch is asserted on the seam's limit; the command line of the
        // read (merged, sorted by update) belongs to the seam (Issue 40).
        let mut stub = Stub::default();
        recent(&mut stub, 7);
        assert_eq!(stub.limit_of("recent_merges"), 21);
    }

    #[test]
    fn overfetched_pool_trimmed_to_requested_k() {
        // Changed: through `gather` and the seam in place of a patched reader.
        let mut stub = Stub::default().with("recent_merges", monthly_pool());
        assert_eq!(numbers(&recent(&mut stub, 3)), vec![9, 8, 7]);
    }

    #[test]
    fn sorted_by_merged_at_desc() {
        // Changed: through `gather` and the seam.
        let mut stub = Stub::default().with("recent_merges", unsorted());
        assert_eq!(numbers(&recent(&mut stub, 10)), vec![3, 2, 1]);
    }

    #[test]
    fn defensive_limit_applied_after_sort() {
        // Changed: through `gather` and the seam.
        let mut stub = Stub::default().with("recent_merges", unsorted());
        assert_eq!(numbers(&recent(&mut stub, 2)), vec![3, 2]);
    }

    #[test]
    fn missing_merged_at_sorts_last() {
        // Changed: through `gather` and the seam.
        let rows = vec![
            json!({"number": 1, "mergedAt": null}),
            json!({"number": 2, "mergedAt": "2026-05-01T00:00:00Z"}),
        ];
        let mut stub = Stub::default().with("recent_merges", rows);
        assert_eq!(numbers(&recent(&mut stub, 10)), vec![2, 1]);
    }
}

mod fetch_effort_history {
    use super::*;

    #[test]
    fn overfetch_and_recency_ordering_requested() {
        // Changed: the over-fetch is asserted on the seam's limit; the command line of the
        // read and its fields belong to the seam (Issue 40).
        let mut stub = Stub::default();
        wd::build_effort_model(&mut stub, "owner/repo", 7).unwrap();
        assert_eq!(stub.limit_of("effort_history"), 21);
    }

    #[test]
    fn trimmed_to_merged_at_top_k() {
        // Changed: through `build_effort_model`: of the nine merges only the three newest are
        // kept, and only their Issues have bodies, so all three are usable samples.
        let pool: Vec<Value> = monthly_pool()
            .into_iter()
            .map(|mut p| {
                p["closingIssuesReferences"] = json!([{"number": p["number"]}]);
                p
            })
            .collect();
        let closed = (7..10).map(|n| json!({"number": n, "body": "b"})).collect();
        let mut stub = Stub::default()
            .with("effort_history", pool)
            .with("closed_issues", closed);
        let m = wd::build_effort_model(&mut stub, "owner/repo", 3).unwrap();
        assert_eq!(m["coverage"]["single_issue_linked_prs"], 3);
        assert_eq!(m["coverage"]["usable_samples"], 3);
    }

    #[test]
    fn closed_issue_fetch_recency_ordered() {
        // Changed: the closed-Issue read goes through the seam with the carried window
        // (`min(max(4 * history, 200), 500)`); its order belongs to the seam.
        let mut stub = Stub::default();
        wd::build_effort_model(&mut stub, "owner/repo", 7).unwrap();
        assert_eq!(stub.limit_of("closed_issues"), 200);
    }
}

// ---- TestEffortLearningCliAndWiring (the pure parts) -----------------------------------------

mod effort_learning_wiring {
    use super::*;

    fn pr(num: i64, repo: Option<(&str, &str)>, lines: (i64, i64, i64)) -> Value {
        let mut r = json!({"number": num});
        if let Some((owner, name)) = repo {
            r["repository"] = json!({"name": name, "owner": {"login": owner}});
        }
        json!({
            "closingIssuesReferences": [r], "additions": lines.0, "deletions": lines.1,
            "changedFiles": lines.2, "reviews": [], "createdAt": "2026-06-01T00:00:00Z",
            "mergedAt": "2026-06-01T01:00:00Z",
        })
    }

    #[test]
    fn build_effort_model_surfaces_coverage() {
        // Changed: the history and the closed Issues come from the seam, not patched readers.
        let prs = vec![pr(5, None, (10, 5, 2)), pr(99, None, (1, 1, 1))];
        let mut stub = Stub::default().with("effort_history", prs).with(
            "closed_issues",
            vec![json!({"number": 5, "body": "issue body"})],
        );
        let m = wd::build_effort_model(&mut stub, "owner/repo", 60).unwrap();
        assert_eq!(m["coverage"]["single_issue_linked_prs"], 2);
        assert_eq!(m["coverage"]["usable_samples"], 1);
        assert_eq!(m["coverage"]["dropped_missing_body"], 1);
        assert_eq!(m["coverage"]["dropped_cross_repo"], 0);
    }

    #[test]
    fn build_effort_model_excludes_cross_repo_closings() {
        // Changed: through the seam.
        let prs = vec![
            pr(5, Some(("owner", "repo")), (10, 5, 2)),
            pr(5, Some(("owner", "other-repo")), (900, 900, 40)),
        ];
        let mut stub = Stub::default().with("effort_history", prs).with(
            "closed_issues",
            vec![json!({"number": 5, "body": "issue body"})],
        );
        let m = wd::build_effort_model(&mut stub, "owner/repo", 60).unwrap();
        assert_eq!(m["coverage"]["single_issue_linked_prs"], 1);
        assert_eq!(m["coverage"]["usable_samples"], 1);
        assert_eq!(m["coverage"]["dropped_missing_body"], 0);
        assert_eq!(m["coverage"]["dropped_cross_repo"], 1);
    }
}

// ---- TestMalformedFieldNormalization ---------------------------------------------------------

mod malformed_field_normalization {
    use super::*;

    #[test]
    fn comments_as_count_does_not_crash() {
        // Changed: comment bodies are read through `dependency_text` (the helper is private).
        let i = json!({"body": "b", "comments": 5});
        assert_eq!(refs::dependency_text(&i), "b");
    }

    #[test]
    fn labels_as_string_does_not_crash() {
        assert!(refs::label_names(&json!({"labels": "blocked"})).is_empty());
    }

    #[test]
    fn scan_survives_non_list_comments_and_labels() {
        let i = json!({"number": 1, "title": "t", "body": "b", "comments": 3, "labels": 0});
        assert_eq!(scan(vec![i], &[], vec![])["candidates"][0]["issue"], 1);
    }

    #[test]
    fn is_int_excludes_bool() {
        // Changed: there is no `_is_int`; an Issue numbered `true`, `"5"` or null is not an open
        // ref, so an Issue blocked by #1 or #5 is resolved.
        for n in [json!(true), json!("5"), Value::Null] {
            let r = scan(
                vec![
                    issue(10, "Blocked by #1, #5", &[]),
                    with(issue(0, "x", &[]), "number", n.clone()),
                ],
                &[],
                vec![],
            );
            assert!(issues_of(&r, "candidates").contains(&json!(10)), "{n}");
        }
    }

    #[test]
    fn bool_pr_number_not_treated_as_issue_one() {
        let issues = vec![json!({"number": 10, "title": "t", "body": "Depends on #1"})];
        let merges = vec![json!({"number": true, "title": "x", "body": "no refs"})];
        let r = scan(issues, &[], merges);
        assert_eq!(r["candidates"][0]["unblocked_by_recent_merge"], false);
    }

    #[test]
    fn candidate_title_always_a_string() {
        let i = json!({"number": 1, "title": null, "body": "body text here"});
        let r = scan(vec![i], &[], vec![]);
        assert_eq!(r["candidates"][0]["title"], "");
        assert!(r["candidates"][0]["summary"].is_string());
    }
}

// ---- cross-repository (TestCrossRepoExtraction .. TestDuplicateRepoBundleDedup) -------------

const JA: &str = "suisya-systems/claude-org-ja";
const RT: &str = "suisya-systems/claude-org-runtime";

fn cross(text: &str, is_epic: bool) -> Vec<(String, i64)> {
    refs::extract_cross_repo_blocking_refs(Some(text), is_epic)
}

fn one(repo: &str, n: i64) -> Vec<(String, i64)> {
    vec![(repo.to_string(), n)]
}

fn keys(result: &Value, key: &str) -> Vec<(Value, Value)> {
    result[key]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["repo"].clone(), c["issue"].clone()))
        .collect()
}

fn find(result: &Value, n: i64) -> Value {
    result["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["issue"] == n)
        .unwrap()
        .clone()
}

fn bundle_m(repo: &str, issues: Vec<Value>, merges: Vec<Value>) -> Bundle {
    Bundle {
        recent_merges: merges,
        ..bundle(repo, issues)
    }
}

fn merge_n(n: i64, body: &str) -> Value {
    json!({"number": n, "title": "x", "body": body})
}

mod cross_repo_extraction {
    use super::*;

    #[test]
    fn owner_repo_blocked_by() {
        assert_eq!(cross(&format!("Blocked by {RT}#60"), false), one(RT, 60));
    }

    #[test]
    fn github_url_blocked_by() {
        assert_eq!(
            cross(
                &format!("Depends on https://github.com/{RT}/issues/42"),
                false
            ),
            one(RT, 42)
        );
    }

    #[test]
    fn github_pull_url() {
        assert_eq!(
            cross(&format!("Requires https://github.com/{JA}/pull/7"), false),
            one(JA, 7)
        );
    }

    #[test]
    fn mixed_home_and_cross_run() {
        let text = format!("Blocked by #1, {RT}#2");
        assert_eq!(cross(&text, false), one(RT, 2));
        assert_eq!(blocking(&text), vec![1]);
    }

    #[test]
    fn epic_notation_not_a_blocker() {
        assert!(cross("Epic: suisya-systems/claude-org-transport-lab#6", false).is_empty());
    }

    #[test]
    fn refs_and_found_by_not_blockers() {
        let body = format!(
            "Refs {JA}#467, suisya-systems/claude-org#264\nFound by Codex during {JA}#467\n\
             Design source: {JA}#443\n"
        );
        assert!(cross(&body, false).is_empty());
    }

    #[test]
    fn negated_cross_ref_not_a_blocker() {
        assert!(cross(&format!("no longer blocked by {RT}#60"), false).is_empty());
    }

    #[test]
    fn prose_before_cross_ref_not_extracted() {
        assert!(cross(&format!("Blocked by the runtime work, see {RT}#60"), false).is_empty());
    }

    #[test]
    fn bare_repo_shorthand_not_resolved() {
        assert!(cross("Blocked by ja#467", false).is_empty());
    }

    #[test]
    fn cross_task_list_pure_ref() {
        let body = format!("- [ ] {RT}#11\n- [ ] {RT}#12 \u{3092}\u{53c2}\u{8003}\u{306b}\n");
        assert_eq!(cross(&body, false), one(RT, 11));
    }

    #[test]
    fn cross_task_list_ignored_for_epic() {
        assert!(cross(&format!("- [ ] {RT}#11"), true).is_empty());
    }
}

mod qualified_ref_helpers {
    use super::*;

    #[test]
    fn ref_to_json_home_is_bare_int() {
        assert_eq!(refs::ref_to_json(&(None, 5), None), json!(5));
    }

    #[test]
    fn ref_to_json_cross_is_string() {
        assert_eq!(
            refs::ref_to_json(&(Some(RT.into()), 5), None),
            json!(format!("{RT}#5"))
        );
    }

    #[test]
    fn issue_blocking_refs_q_unifies_self_ref() {
        let i = json!({"number": 1, "title": "t", "body": format!("Blocked by #2 and {JA}#2")});
        assert_eq!(
            refs::issue_blocking_refs_q(&i, Some(JA)),
            vec![(Some(JA.to_string()), 2)]
        );
    }
}

mod cross_repo_scan {
    use super::*;

    #[test]
    fn blocked_by_open_cross_repo_issue_excluded() {
        let ja = bundle(JA, vec![issue(60, &format!("Blocked by {RT}#60"), &[])]);
        let rt = bundle(RT, vec![issue(60, "open runtime work", &[])]);
        let r = scan_repos(&[ja, rt]);
        let cands = keys(&r, "candidates");
        assert!(!cands.contains(&(json!(JA), json!(60))));
        assert!(cands.contains(&(json!(RT), json!(60))));
        let excl = &r["excluded_blocked"][0];
        assert_eq!((&excl["repo"], &excl["issue"]), (&json!(JA), &json!(60)));
        assert_eq!(excl["blocking_refs"], json!([format!("{RT}#60")]));
    }

    #[test]
    fn blocked_by_closed_cross_repo_issue_resolved() {
        let ja = bundle(JA, vec![issue(60, &format!("Blocked by {RT}#60"), &[])]);
        let r = scan_repos(&[ja, bundle(RT, vec![])]);
        assert!(keys(&r, "candidates").contains(&(json!(JA), json!(60))));
    }

    #[test]
    fn issue_number_collision_across_repos() {
        let ja = bundle(JA, vec![issue(60, "ja work", &[])]);
        let rt = bundle(RT, vec![issue(60, "runtime work", &[])]);
        let mut k = keys(&scan_repos(&[ja, rt]), "candidates");
        k.sort_by_key(|(r, _)| r.to_string());
        assert_eq!(k, vec![(json!(JA), json!(60)), (json!(RT), json!(60))]);
    }

    #[test]
    fn candidates_carry_repo_field() {
        let r = scan_repos(&[bundle(JA, vec![issue(1, "b", &[])])]);
        assert_eq!(r["candidates"][0]["repo"], JA);
        assert_eq!(r["recommendation"]["repo"], JA);
    }

    #[test]
    fn unblocked_by_recent_cross_repo_merge() {
        let ja = bundle(JA, vec![issue(72, &format!("Depends on {RT}#80"), &[])]);
        let rt = bundle_m(RT, vec![], vec![merge_n(900, "Closes #80")]);
        let c = find(&scan_repos(&[ja, rt]), 72);
        assert_eq!(c["unblocked_by_recent_merge"], true);
        assert!(any_has(&signals_of(&c), &format!("{RT}#80")));
    }

    #[test]
    fn recent_merge_qualified_by_repo() {
        let ja = bundle_m(
            JA,
            vec![issue(72, &format!("Depends on {RT}#80"), &[])],
            vec![merge_n(900, "Closes #80")],
        );
        let c = find(&scan_repos(&[ja, bundle(RT, vec![])]), 72);
        assert_eq!(c["unblocked_by_recent_merge"], false);
    }

    #[test]
    fn unscanned_repo_ref_resolved_with_signal() {
        let ja = bundle(JA, vec![issue(71, "Blocked by Shin-sibainu/ccmux#4", &[])]);
        let r = scan_repos(&[ja]);
        assert!(any_has(&signals_of(&r["candidates"][0]), "un-scanned repo"));
    }

    #[test]
    fn cross_repo_merge_closes_other_repo_issue() {
        let ja3 = bundle(JA, vec![issue(7, &format!("Depends on {JA}#999"), &[])]);
        let rt2 = bundle_m(RT, vec![], vec![merge_n(902, &format!("Closes {JA}#999"))]);
        let c = find(&scan_repos(&[ja3, rt2]), 7);
        assert_eq!(c["unblocked_by_recent_merge"], true);
    }

    #[test]
    fn deterministic_order_on_collision() {
        let ja = bundle(JA, vec![issue(60, "b", &[])]);
        let rt = bundle(RT, vec![issue(60, "b", &[])]);
        let r1 = scan_repos(&[ja.clone(), rt.clone()]);
        let r2 = scan_repos(&[rt, ja]);
        assert_eq!(keys(&r1, "candidates"), keys(&r2, "candidates"));
    }
}

mod cross_repo_effort {
    use super::*;

    fn samples() -> Vec<Value> {
        (1..12)
            .map(|i| {
                json!({"body_len": 100 * i, "changed_lines": 50 * i, "changed_files": i,
                       "criteria": 0, "review_rounds": 0, "hours_to_merge": null})
            })
            .collect()
    }

    #[test]
    fn effort_samples_not_applied_in_multi_repo_offline() {
        // Changed: the guard is in `scan_repos` (the carried one was in the command line): a
        // model is not applied across two repositories and `effort_model` is null.
        let m = effort::learn_effort_model(&samples());
        let b = [
            bundle(JA, vec![json!({"number": 1, "title": "a", "body": "b"})]),
            bundle(RT, vec![json!({"number": 1, "title": "c", "body": "d"})]),
        ];
        let r = wd::scan_repos(&b, &cfg(), None, None, Some(&m), None);
        assert_eq!(r["status"], "candidates_found");
        assert!(r["effort_model"].is_null());
    }

    #[test]
    fn effort_samples_applied_in_single_repo_offline() {
        // Changed: through `scan` with the learned model in place of a bundle file.
        let m = effort::learn_effort_model(&samples());
        let r = wd::scan(
            vec![json!({"number": 1, "title": "a", "body": "b"})],
            vec![],
            vec![],
            &cfg(),
            None,
            Some(&m),
            None,
            vec![],
        );
        assert_eq!(r["status"], "candidates_found");
        assert!(!r["effort_model"].is_null());
    }
}

mod cross_repo_order_independence {
    use super::*;

    #[test]
    fn home_ref_after_cross_ref_blocking() {
        let text = format!("Blocked by {RT}#2, #1");
        assert_eq!(blocking(&text), vec![1]);
        assert_eq!(cross(&text, false), one(RT, 2));
    }

    #[test]
    fn home_ref_after_cross_ref_in_scan_excludes() {
        let ja = bundle(
            JA,
            vec![
                issue(9, &format!("Blocked by {RT}#2, #1"), &[]),
                issue(1, "open home dep", &[]),
            ],
        );
        assert!(by_issue(&scan_repos(&[ja]), "excluded_blocked").contains_key(&9));
    }

    #[test]
    fn recent_merge_home_close_after_cross_close() {
        let closed: Vec<i64> = refs::pr_close_refs(&format!("Closes {RT}#81, #80"))
            .into_iter()
            .collect();
        assert_eq!(closed, vec![80]);
    }

    #[test]
    fn recent_merge_home_close_after_cross_in_scan() {
        let b = bundle_m(
            JA,
            vec![issue(7, "Depends on #80", &[])],
            vec![merge_n(900, &format!("Closes {RT}#81, #80"))],
        );
        assert_eq!(
            find(&scan_repos(&[b]), 7)["unblocked_by_recent_merge"],
            true
        );
    }
}

mod single_repo_self_reference_keying {
    use super::*;

    fn collapsed(b: Bundle) -> Value {
        wd::scan_repos(&[b], &cfg(), None, Some(JA), None, None)
    }

    #[test]
    fn self_ref_full_name_excludes_when_open() {
        let b = bundle(
            JA,
            vec![
                issue(10, &format!("Blocked by {JA}#5"), &[]),
                issue(5, "open", &[]),
            ],
        );
        let r = collapsed(b);
        let excl = by_issue(&r, "excluded_blocked");
        assert!(excl[&10]["repo"].is_null());
        assert_eq!(excl[&10]["blocking_refs"], json!([5]));
        assert!(!issues_of(&r, "candidates").contains(&json!(10)));
    }

    #[test]
    fn self_ref_recent_merge_unblocks() {
        let b = bundle_m(
            JA,
            vec![issue(7, &format!("Depends on {JA}#80"), &[])],
            vec![merge_n(900, &format!("Closes {JA}#80"))],
        );
        let c = find(&collapsed(b), 7);
        assert_eq!(c["unblocked_by_recent_merge"], true);
        assert!(c["repo"].is_null());
    }

    #[test]
    fn collapse_does_not_affect_other_repo_ref() {
        let r = collapsed(bundle(
            JA,
            vec![issue(71, &format!("Blocked by {RT}#9"), &[])],
        ));
        let c = &r["candidates"][0];
        assert!(c["repo"].is_null());
        assert_eq!(c["blocking_refs"], json!([format!("{RT}#9")]));
        assert!(any_has(&signals_of(c), "un-scanned"));
    }

    #[test]
    fn multi_repo_not_collapsed() {
        let r = scan_repos(&[
            bundle(JA, vec![issue(1, "b", &[])]),
            bundle(RT, vec![issue(1, "b", &[])]),
        ]);
        let mut repos: Vec<String> = keys(&r, "candidates")
            .into_iter()
            .map(|(r, _)| r.as_str().unwrap().to_string())
            .collect();
        repos.sort();
        assert_eq!(repos, vec![JA.to_string(), RT.to_string()]);
    }
}

mod duplicate_repo_bundle_dedup {
    use super::*;

    #[test]
    fn duplicate_bundle_dedups_candidates() {
        let b = bundle(JA, vec![issue(1, "b", &[]), issue(2, "Blocked by #3", &[])]);
        let dep = bundle(JA, vec![issue(3, "open", &[])]);
        let r = scan_repos(&[b.clone(), b, dep]);
        let k = keys(&r, "candidates");
        assert_eq!(k.iter().filter(|x| **x == (json!(JA), json!(1))).count(), 1);
        assert_eq!(r["candidate_count"], k.len());
        let e = keys(&r, "excluded_blocked");
        assert_eq!(e.iter().filter(|x| **x == (json!(JA), json!(2))).count(), 1);
    }
}

// ---- TestBaseBranchCompletion ----------------------------------------------------------------

const KURA: &str = "aainc/kura-data-aggregator-trial";

/// The shapes behind the carried Issue #830: PR #232 targets `develop` and says `Closes #231`, so
/// GitHub never closes #231. (The fixture's prose is translated; only its refs matter.)
fn kura_issue_231() -> Value {
    json!({
        "number": 231,
        "title": "Set the MCP token TTL back from the interim 8h to the default 900s",
        "body": "Remove the interim TTL once refresh is confirmed in production.",
        "labels": [],
        "createdAt": "2026-08-05T01:20:00Z",
        "updatedAt": "2026-08-06T02:33:45Z",
    })
}

fn kura_pr_232() -> Value {
    json!({
        "number": 232,
        "title": "fix(kura): drop the interim 8 hour MCP access token TTL",
        "body": "## Summary\n\nFollow-up of #226. Refresh (#228) runs in production, so the \
                 interim TTL of #227 goes.\n\nCloses #231\n",
        "baseRefName": "develop",
        "mergedAt": "2026-08-06T02:33:45Z",
    })
}

fn kura_bundle() -> Bundle {
    Bundle {
        repo: Some(KURA.into()),
        issues: vec![kura_issue_231()],
        open_pr_numbers: vec![],
        recent_merges: vec![kura_pr_232()],
        base_branch: Some("develop".into()),
        base_merges: vec![kura_pr_232()],
    }
}

fn base_scan(bundles: &[Bundle], collapse: Option<&str>) -> Value {
    let config = ScanConfig {
        trigger: "post_merge".into(),
        ..cfg()
    };
    wd::scan_repos(bundles, &config, None, collapse, None, None)
}

fn kura_scan(b: Bundle) -> Value {
    base_scan(&[b], Some(KURA))
}

fn candidate_issues(r: &Value) -> Vec<Value> {
    issues_of(r, "candidates")
}

fn sig_list(r: &Value, key: &str) -> Vec<String> {
    r[key]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
        .collect()
}

mod base_branch_completion {
    use super::*;

    #[test]
    fn pre_fix_symptom_without_base_branch() {
        let r = kura_scan(Bundle {
            base_branch: None,
            base_merges: vec![],
            ..kura_bundle()
        });
        assert_eq!(r["status"], "candidates_found");
        let top = &r["candidates"][0];
        assert_eq!(top["issue"], 231);
        assert_eq!(top["rank"], 1);
        assert_eq!(top["unblocked_by_recent_merge"], true);
        assert_eq!(r["excluded_merged"], json!([]));
    }

    #[test]
    fn kura_231_is_excluded_with_a_reason() {
        let r = kura_scan(kura_bundle());
        assert!(candidate_issues(&r).is_empty());
        assert_eq!(r["status"], "no_candidates");
        let e = &r["excluded_merged"][0];
        assert_eq!(r["excluded_merged"].as_array().unwrap().len(), 1);
        assert_eq!(e["issue"], 231);
        assert!(e["repo"].is_null());
        assert_eq!(e["base_branch"], "develop");
        assert_eq!(e["closed_by_pr"], 232);
        assert_eq!(e["merged_at"], "2026-08-06T02:33:45Z");
        let note = e["note"].as_str().unwrap();
        assert!(note.contains("#232") && note.contains("develop"), "{note}");
        assert_eq!(r["excluded_blocked"], json!([]));
    }

    #[test]
    fn excluded_issue_is_not_recommended() {
        assert!(kura_scan(kura_bundle())["recommendation"].is_null());
    }

    #[test]
    fn base_branch_scan_audit_is_emitted() {
        let r = kura_scan(kura_bundle());
        assert_eq!(
            r["base_branch_scan"],
            json!([{"repo": null, "base_branch": "develop", "merged_prs_scanned": 1,
                    "closed_issue_count": 1}])
        );
        assert_eq!(r["base_branch_signals"], json!([]));
    }

    #[test]
    fn repo_without_base_branch_is_unchanged() {
        let b = Bundle {
            recent_merges: vec![json!({"number": 8, "body": "Closes #7"})],
            ..bundle("o/plain", vec![issue(7, "b", &[])])
        };
        let r = base_scan(&[b], Some("o/plain"));
        assert_eq!(candidate_issues(&r), vec![json!(7)]);
        assert_eq!(r["excluded_merged"], json!([]));
        assert_eq!(r["base_branch_scan"], json!([]));
    }

    #[test]
    fn mere_reference_does_not_exclude() {
        let pr = with(kura_pr_232(), "body", json!("Refs #231\n"));
        let r = kura_scan(Bundle {
            base_merges: vec![pr.clone()],
            recent_merges: vec![pr],
            ..kura_bundle()
        });
        assert_eq!(candidate_issues(&r), vec![json!(231)]);
        assert_eq!(r["excluded_merged"], json!([]));
    }

    #[test]
    fn negated_close_keyword_does_not_exclude() {
        let pr = with(
            kura_pr_232(),
            "body",
            json!("This does not close #231 yet.\n"),
        );
        let r = kura_scan(Bundle {
            base_merges: vec![pr],
            recent_merges: vec![],
            ..kura_bundle()
        });
        assert_eq!(candidate_issues(&r), vec![json!(231)]);
        assert_eq!(r["excluded_merged"], json!([]));
    }

    #[test]
    fn merge_targeting_another_branch_is_not_counted() {
        let pr = with(kura_pr_232(), "baseRefName", json!("feature/x"));
        let r = kura_scan(Bundle {
            base_merges: vec![pr],
            recent_merges: vec![],
            ..kura_bundle()
        });
        assert_eq!(candidate_issues(&r), vec![json!(231)]);
        assert_eq!(r["excluded_merged"], json!([]));
        assert!(any_has(
            &sig_list(&r, "base_branch_signals"),
            "target a different branch"
        ));
    }

    #[test]
    fn issue_created_after_the_merge_is_not_excluded() {
        let i = with(kura_issue_231(), "createdAt", json!("2026-08-07T00:00:00Z"));
        let r = kura_scan(Bundle {
            issues: vec![i],
            recent_merges: vec![],
            ..kura_bundle()
        });
        assert_eq!(candidate_issues(&r), vec![json!(231)]);
        assert_eq!(r["excluded_merged"], json!([]));
        assert!(any_has(
            &sig_list(&r, "base_branch_signals"),
            "closing link ignored"
        ));
    }

    #[test]
    fn missing_created_at_still_excludes() {
        let mut i = kura_issue_231();
        i.as_object_mut().unwrap().remove("createdAt");
        let r = kura_scan(Bundle {
            issues: vec![i],
            recent_merges: vec![],
            ..kura_bundle()
        });
        assert_eq!(r["candidates"], json!([]));
        assert_eq!(r["excluded_merged"][0]["issue"], 231);
    }

    #[test]
    fn newest_closing_merge_is_the_one_cited() {
        let older = json!({"number": 100, "title": "t", "body": "Closes #231",
                           "baseRefName": "develop", "mergedAt": "2026-07-01T00:00:00Z"});
        let r = kura_scan(Bundle {
            base_merges: vec![older, kura_pr_232()],
            recent_merges: vec![],
            ..kura_bundle()
        });
        assert_eq!(r["excluded_merged"][0]["closed_by_pr"], 232);
    }

    #[test]
    fn dependent_unblocks_when_its_blocker_was_merged() {
        let r = kura_scan(Bundle {
            issues: vec![kura_issue_231(), issue(240, "Blocked by #231", &[])],
            ..kura_bundle()
        });
        assert_eq!(candidate_issues(&r), vec![json!(240)]);
        assert_eq!(r["excluded_blocked"], json!([]));
        let s = signals_of(&r["candidates"][0]);
        assert!(
            s.iter()
                .any(|x| x.contains("#231") && x.contains("#232") && x.contains("develop")),
            "{s:?}"
        );
    }

    fn other_repo_close(body: &str) -> Value {
        let pr = with(kura_pr_232(), "body", json!(body));
        let kura = Bundle {
            issues: vec![],
            base_merges: vec![pr],
            recent_merges: vec![],
            ..kura_bundle()
        };
        base_scan(&[kura, bundle(JA, vec![issue(77, "b", &[])])], None)
    }

    #[test]
    fn cross_repo_close_from_a_base_branch_merge() {
        let r = other_repo_close(&format!("Closes {JA}#77\n"));
        assert_eq!(r["candidates"], json!([]));
        let e = &r["excluded_merged"][0];
        assert_eq!(e["repo"], JA);
        assert_eq!(e["issue"], 77);
        assert_eq!(e["closed_by_pr"], format!("{KURA}#232"));
    }

    #[test]
    fn cross_repo_close_matches_case_insensitively() {
        let r = other_repo_close("Closes Suisya-Systems/Claude-Org-JA#77\n");
        assert_eq!(r["candidates"], json!([]));
        assert_eq!(r["excluded_merged"][0]["repo"], JA);
    }

    #[test]
    fn origin_prefixed_and_padded_base_branch_cell() {
        let r = kura_scan(Bundle {
            base_branch: Some("  develop  ".into()),
            ..kura_bundle()
        });
        assert_eq!(r["excluded_merged"][0]["issue"], 231);
    }

    #[test]
    fn scan_shim_accepts_base_branch_inputs() {
        let r = wd::scan(
            vec![kura_issue_231()],
            vec![],
            vec![kura_pr_232()],
            &cfg(),
            None,
            None,
            Some("develop"),
            vec![kura_pr_232()],
        );
        assert_eq!(r["candidates"], json!([]));
        assert_eq!(r["excluded_merged"][0]["closed_by_pr"], 232);
    }

    #[test]
    fn scan_is_still_pure() {
        assert_eq!(kura_scan(kura_bundle()), kura_scan(kura_bundle()));
    }
}

// ---- TestFetchBaseBranchMerges, and the truncation flag of TestBaseBranchFromRegistry ------

mod fetch_base_branch_merges {
    use super::*;

    #[test]
    fn overfetches_then_slices_by_merged_at() {
        // Changed: through `gather` and the seam; the read's command line (`--base develop`,
        // sorted by update) belongs to the seam (Issue 40).
        let mut stub = Stub::default().with("base_merges", monthly_pool());
        let limits = Limits {
            base_merges: 3,
            ..Limits::default()
        };
        let (b, t) = wd::gather(&mut stub, KURA, Some("develop"), &limits).unwrap();
        assert_eq!(stub.limit_of("base_merges:develop"), 9);
        assert_eq!(numbers(&b.base_merges), vec![9, 8, 7]);
        assert_eq!(b.base_branch.as_deref(), Some("develop"));
        assert!(t.base_merges);
    }

    #[test]
    fn truncated_base_merge_window_is_flagged() {
        // Changed: the base branch is the caller's (no registry); a full window of merges into
        // it sets `input_truncated.base_merges`.
        let mut stub = Stub::default().with("base_merges", vec![kura_pr_232()]);
        let limits = Limits {
            base_merges: 1,
            ..Limits::default()
        };
        let (b, t) = wd::gather(&mut stub, KURA, Some("develop"), &limits).unwrap();
        let r = wd::scan_repos(&[b], &cfg(), Some(&t), None, None, None);
        assert_eq!(r["input_truncated"]["base_merges"], true);
    }
}

// ---- TestDependencyWarnings ------------------------------------------------------------------

mod dependency_warnings {
    use super::*;

    fn warnings(body: &str) -> Vec<Value> {
        scan(vec![issue(7, body, &[])], &[], vec![])["dependency_warnings"]
            .as_array()
            .unwrap()
            .clone()
    }

    #[test]
    fn ref_on_next_line_warns_once() {
        let w = warnings("## Depends on\nDepends on\n#12\nmore text");
        assert_eq!(w.len(), 1);
        assert_eq!(w[0]["issue"], 7);
        assert!(w[0]["line"].as_str().unwrap().contains("Depends on"));
    }

    #[test]
    fn same_line_ref_does_not_warn() {
        assert!(warnings("Depends on #12").is_empty());
    }

    #[test]
    fn negated_form_does_not_warn() {
        assert!(warnings("not blocked by #5").is_empty());
        assert!(warnings("not blocked by the old flow").is_empty());
    }

    #[test]
    fn line_is_truncated_to_120_chars() {
        let w = warnings(&format!("Requires {}\n#12", "x".repeat(300)));
        assert_eq!(w[0]["line"].as_str().unwrap().chars().count(), 120);
    }

    #[test]
    fn prose_depends_on_does_not_warn() {
        assert!(warnings("The row budget depends on the window size.\nSee the notes.").is_empty());
        assert!(warnings("Requires a restart").is_empty());
    }

    #[test]
    fn heading_form_warns() {
        let w = warnings("## Depends on\n\n#12\n");
        assert_eq!(w.len(), 1);
        assert_eq!(w[0]["line"], "## Depends on");
    }

    #[test]
    fn cross_repo_ref_on_next_line_warns() {
        let w = warnings("This depends on\nowner/repo#9 landing first");
        assert_eq!(w.len(), 1);
        assert_eq!(w[0]["line"], "This depends on");
    }

    #[test]
    fn does_not_change_candidates() {
        let r = scan(vec![issue(7, "Depends on\n#12", &[])], &[], vec![]);
        assert_eq!(r["candidate_count"], 1);
        assert_eq!(r["excluded_blocked"], json!([]));
    }
}

// ---- the goal stage (TestScanReposGoalRanker, TestGoalModeCli, goals TestRanking) -----------

const LEDGER: &str = "# Goals\n\n## G1 Ship login\nUsers can log in.\n- unmet if: login is broken\n\n\
                      ## G2 Fast CI\n- unmet if: CI takes > 10 min\n";

fn write_ledger(goals_dir: &Path, slug: &str, text: &str) -> std::path::PathBuf {
    let p = goals_dir.join(format!("{slug}.md"));
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, text).unwrap();
    p
}

fn goal_scan(bundles: &[Bundle], goals_dir: &Path, top_n: i64) -> Value {
    wd::scan_repos(bundles, &cfg_top(top_n), None, None, None, Some(goals_dir))
}

fn named(n: i64, clause: &str) -> Value {
    let body = if clause.is_empty() {
        "Some work.\n".to_string()
    } else {
        format!("Some work.\n\n## Goal clause\n\n- {clause} the clause\n")
    };
    issue(n, &body, &[])
}

mod scan_repos_goal_ranker {
    use super::*;

    #[test]
    fn legacy_has_fixed_goal_schema() {
        // Changed: the judge's fields (`goal_why`, `goal_request`, `open_points`,
        // `goal_judgement_key`, `excluded_goal`) are not carried; `goal_clause` is.
        let r = scan_repos(&[
            bundle(
                "OwnerA/Alpha",
                vec![issue(1, "b1", &["Bug"]), issue(2, "", &[])],
            ),
            bundle("OwnerC/Gamma", vec![issue(9, "", &[])]),
        ]);
        assert_eq!(r["rank_mode"], "legacy");
        assert!(r["goal_rank"].is_null());
        for c in r["candidates"].as_array().unwrap() {
            assert!(c["goal_clause"].is_null());
            assert!(c.as_object().unwrap().keys().all(|k| !k.starts_with('_')));
        }
    }

    #[test]
    fn goal_ranker_gets_whole_pool_with_internal_fields() {
        // Changed: the goal stage is the clause each Issue names, not a ranker the caller
        // passes. What carries over: it sees the whole pool before the top-N cut (a clause hit
        // the legacy key ranks last still comes first) and the output keeps no internal field.
        let dir = tempfile::tempdir().unwrap();
        write_ledger(dir.path(), "ownera/alpha", LEDGER);
        let b = bundle(
            "OwnerA/Alpha",
            vec![
                issue(1, "b1", &["priority:high"]),
                issue(2, "b2", &["priority:high"]),
                with(named(3, "G1"), "labels", json!([{"name": "backlog"}])),
            ],
        );
        let r = goal_scan(&[b], dir.path(), 1);
        assert_eq!(r["rank_mode"], "goal");
        assert_eq!(r["status"], "candidates_found");
        assert_eq!(r["candidate_count"], 1);
        assert_eq!(r["truncated_count"], 2);
        let c = &r["candidates"][0];
        assert_eq!(c["issue"], 3);
        assert_eq!(
            c["goal_clause"],
            json!({"id": "G1", "heading": "Ship login"})
        );
        assert!(c.as_object().unwrap().keys().all(|k| !k.starts_with('_')));
    }
}

mod goal_mode {
    use super::*;

    fn bundles() -> Vec<Bundle> {
        vec![
            bundle(
                "OwnerA/Alpha",
                vec![
                    with(named(1, "G1"), "title", json!("Goal thing")),
                    with(issue(2, "unrelated", &[]), "title", json!("misc")),
                ],
            ),
            bundle(
                "ownerb/beta",
                vec![with(named(5, "G1"), "title", json!("Goal beta"))],
            ),
        ]
    }

    #[test]
    fn clause_hit_ranked_unset_repo_reported() {
        // Changed: the clause is the one the Issue names (no judge, no cache, no call); an
        // Issue naming none is ordered after, not excluded; the ledger's clauses are reported
        // with their unmet-if lines for the notice.
        let dir = tempfile::tempdir().unwrap();
        write_ledger(
            dir.path(),
            "ownera/alpha",
            "## G1 Ship the goal feature\n- unmet if: not shipped\n",
        );
        let r = goal_scan(&bundles(), dir.path(), 3);
        assert_eq!(r["rank_mode"], "goal");
        let got: Vec<(Value, Value, Value)> = r["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| (c["repo"].clone(), c["issue"].clone(), c["rank"].clone()))
            .collect();
        assert_eq!(
            got,
            vec![
                (json!("OwnerA/Alpha"), json!(1), json!(1)),
                (json!("OwnerA/Alpha"), json!(2), json!(2)),
            ]
        );
        assert_eq!(
            r["candidates"][0]["goal_clause"],
            json!({"id": "G1", "heading": "Ship the goal feature"})
        );
        assert!(r["candidates"][1]["goal_clause"].is_null());
        assert!(
            r["recommendation"]["reason"]
                .as_str()
                .unwrap()
                .starts_with("G1 \"Ship the goal feature\": ")
        );
        let gr = &r["goal_rank"];
        assert_eq!(gr["goal_unset_repos"][0]["repo"], "ownerb/beta");
        assert_eq!(gr["goal_unset_repos"][0]["candidate_count"], 1);
        assert_eq!(gr["goals"][0]["repo"], "ownera/alpha");
        assert_eq!(gr["goals"][0]["clause_count"], 1);
        assert_eq!(
            gr["goals"][0]["clauses"],
            json!([{"id": "G1", "heading": "Ship the goal feature", "unmet_if": ["not shipped"]}])
        );
    }

    #[test]
    fn no_ledgers_no_candidates_no_judge() {
        // Changed: no judge to skip; both projects are reported unset and none yields a
        // candidate.
        let dir = tempfile::tempdir().unwrap();
        let r = goal_scan(&bundles(), dir.path(), 3);
        assert_eq!(r["status"], "no_candidates");
        assert_eq!(r["candidates"], json!([]));
        let unset: Vec<&str> = r["goal_rank"]["goal_unset_repos"]
            .as_array()
            .unwrap()
            .iter()
            .map(|u| u["repo"].as_str().unwrap())
            .collect();
        assert_eq!(unset, vec!["ownera/alpha", "ownerb/beta"]);
    }
}

// ---- goals: TestParseLedger, TestLoadGoals, TestRanking ---------------------------------------

mod parse_ledger {
    use super::*;

    fn err(text: &str) -> String {
        goals::parse_ledger(text).unwrap_err()
    }

    #[test]
    fn basic() {
        let cl = goals::parse_ledger(LEDGER).unwrap();
        let ids: Vec<&str> = cl.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["G1", "G2"]);
        assert_eq!(cl[0].heading, "Ship login");
        assert_eq!(cl[0].body, "Users can log in.");
        assert_eq!(cl[0].unmet_if, vec!["login is broken"]);
        assert_eq!(cl[1].index, 2);
    }

    #[test]
    fn bom_and_crlf() {
        let text = format!("\u{feff}{}", LEDGER.replace('\n', "\r\n"));
        let cl = goals::parse_ledger(&text).unwrap();
        assert_eq!(cl[0].heading, "Ship login");
        assert_eq!(cl[1].unmet_if, vec!["CI takes > 10 min"]);
    }

    #[test]
    fn fenced_block_ignored() {
        let text = "## G1 A\n```\n## G2 fake\n- unmet if: fake\n```\n- unmet if: real\n";
        let cl = goals::parse_ledger(text).unwrap();
        assert_eq!(cl.len(), 1);
        assert_eq!(cl[0].unmet_if, vec!["real"]);
        assert!(!cl[0].body.contains("fake"));
    }

    #[test]
    fn other_heading_ends_clause() {
        let text = "## G1 A\n- unmet if: x\n## Goals overview\nignored text\n- unmet if: y\n";
        let cl = goals::parse_ledger(text).unwrap();
        assert_eq!(cl[0].unmet_if, vec!["x"]);
        assert_eq!(cl[0].body, "");
    }

    #[test]
    fn h1_ends_clause_and_h3_is_body() {
        let text = "## G1 A\n### detail\n- unmet if: x\n# Top\n- unmet if: y\n";
        let cl = goals::parse_ledger(text).unwrap();
        assert_eq!(cl[0].body, "### detail");
        assert_eq!(cl[0].unmet_if, vec!["x"]);
    }

    #[test]
    fn unmet_after_ending_heading_does_not_count() {
        assert!(err("## G1 A\n## Notes\n- unmet if: x\n").contains("unmet"));
    }

    #[test]
    fn empty_title_violation() {
        for bad in [
            "## G1\n- unmet if: x\n",
            "## G1   \n- unmet if: x\n",
            "## G01 A\n- unmet if: x\n",
            "## G1x A\n- unmet if: x\n",
        ] {
            assert!(err(bad).contains("malformed"), "{bad}");
        }
    }

    #[test]
    fn lowercase_g_is_malformed() {
        for bad in [
            "## g1 A\n- unmet if: x\n",
            "## G1 A\n- unmet if: x\n## g2 B\n- unmet if: y\n",
        ] {
            assert!(err(bad).contains("malformed"), "{bad}");
        }
        assert_eq!(
            goals::parse_ledger("## G1 A\n- unmet if: x\n## Goals\n")
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn gap_and_duplicate_and_order() {
        for bad in [
            "## G1 A\n- unmet if: x\n## G3 C\n- unmet if: y\n",
            "## G1 A\n- unmet if: x\n## G1 B\n- unmet if: y\n",
            "## G2 A\n- unmet if: x\n## G1 B\n- unmet if: y\n",
        ] {
            assert!(err(bad).contains("numbering"), "{bad}");
        }
    }

    #[test]
    fn zero_clauses() {
        assert!(err("# Goals\njust prose\n").contains("no clauses"));
    }

    #[test]
    fn missing_unmet() {
        assert!(err("## G1 A\n- unmet if: x\n## G2 B\nprose\n").contains("G2"));
    }

    #[test]
    fn unmet_variants() {
        let cl = goals::parse_ledger("## G1 A\n  * Unmet IF:  spaced  \n- unmet if:\n").unwrap();
        assert_eq!(cl[0].unmet_if, vec!["spaced"]);
        assert_eq!(cl[0].body, "- unmet if:");
    }
}

mod load_goals {
    use super::*;
    use goals::GoalStatus;

    #[test]
    fn ok_and_lowercases_slug() {
        let dir = tempfile::tempdir().unwrap();
        write_ledger(dir.path(), "acme/app", LEDGER);
        let g = goals::load_goals(dir.path(), "Acme/App");
        assert_eq!(
            (g.status, g.repo.as_str(), g.clauses.len()),
            (GoalStatus::Ok, "acme/app", 2)
        );
    }

    #[test]
    fn unset() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            goals::load_goals(dir.path(), "acme/app").status,
            GoalStatus::Unset
        );
        std::fs::create_dir_all(dir.path().join("acme")).unwrap();
        assert_eq!(
            goals::load_goals(dir.path(), "acme/app").status,
            GoalStatus::Unset
        );
    }

    #[test]
    fn missing_goals_dir_is_unset() {
        let dir = tempfile::tempdir().unwrap();
        let g = goals::load_goals(&dir.path().join("nope"), "o/r");
        assert_eq!(g.status, GoalStatus::Unset);
    }

    #[test]
    fn unlistable_goals_dir_is_error_not_unset() {
        // Changed: the permission case is a real folder of mode 000 (no patched listing), and
        // the error names the kind of the failure as Rust does (`PermissionDenied`).
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let not_a_dir = dir.path().join("goals-file");
        std::fs::write(&not_a_dir, "x").unwrap();
        let g = goals::load_goals(&not_a_dir, "o/r");
        assert_eq!(g.status, GoalStatus::Error);
        assert!(g.error.unwrap().contains("goals dir unreadable"));
        let goals_dir = dir.path().join("goals");
        write_ledger(&goals_dir, "o/r", LEDGER);
        std::fs::set_permissions(&goals_dir, std::fs::Permissions::from_mode(0o000)).unwrap();
        let g = goals::load_goals(&goals_dir, "o/r");
        std::fs::set_permissions(&goals_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        // A process that can read a folder of mode 000 (root) sees the ledger instead.
        if std::fs::read_dir(dir.path().join("goals")).is_ok() && g.status == GoalStatus::Ok {
            return;
        }
        assert_eq!(g.status, GoalStatus::Error);
        assert!(g.error.unwrap().contains("PermissionDenied"));
    }

    #[test]
    fn case_mismatch_file() {
        let dir = tempfile::tempdir().unwrap();
        write_ledger(dir.path(), "acme/App", LEDGER);
        let g = goals::load_goals(dir.path(), "acme/app");
        assert_eq!(g.status, GoalStatus::Error);
        let e = g.error.unwrap();
        assert!(e.starts_with("case mismatch: "), "{e}");
        assert!(e.contains("App.md"));
    }

    #[test]
    fn case_mismatch_owner_dir() {
        let dir = tempfile::tempdir().unwrap();
        write_ledger(dir.path(), "Acme/app", LEDGER);
        let g = goals::load_goals(dir.path(), "acme/app");
        assert_eq!(g.status, GoalStatus::Error);
        assert!(g.error.unwrap().contains("case mismatch"));
    }

    #[test]
    fn ledger_error_and_bad_slug() {
        let dir = tempfile::tempdir().unwrap();
        write_ledger(dir.path(), "acme/app", "nothing here\n");
        assert_eq!(
            goals::load_goals(dir.path(), "acme/app").error.as_deref(),
            Some("no clauses")
        );
        for bad in ["noslash", "a/b/c", "../x", "a/.."] {
            assert_eq!(
                goals::load_goals(dir.path(), bad).status,
                GoalStatus::Error,
                "{bad}"
            );
        }
    }

    #[test]
    fn unreadable_utf8() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_ledger(dir.path(), "acme/app", LEDGER);
        std::fs::write(&p, b"## G1 \xff\n").unwrap();
        let g = goals::load_goals(dir.path(), "acme/app");
        assert!(g.error.unwrap().contains("unreadable"));
    }
}

mod goal_ranking {
    use super::*;

    #[test]
    fn two_repos_ranked_by_clause_then_legacy() {
        // Changed: each Issue names its clause (no judge); within a clause the carried legacy
        // key orders (here equal but for the number); the recommendation names the clause.
        let dir = tempfile::tempdir().unwrap();
        write_ledger(dir.path(), "a/x", LEDGER);
        write_ledger(dir.path(), "b/y", LEDGER);
        let r = goal_scan(
            &[
                bundle("a/x", vec![named(1, "G2"), named(5, "G1")]),
                bundle("b/y", vec![named(2, "G1"), named(3, "G2"), named(4, "G1")]),
            ],
            dir.path(),
            3,
        );
        let got: Vec<(Value, Value, Value)> = r["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| (c["repo"].clone(), c["issue"].clone(), c["rank"].clone()))
            .collect();
        assert_eq!(
            got,
            vec![
                (json!("a/x"), json!(5), json!(1)),
                (json!("b/y"), json!(2), json!(2)),
                (json!("b/y"), json!(4), json!(3)),
            ]
        );
        assert_eq!(r["truncated_count"], 2);
        assert_eq!(
            r["candidates"][0]["goal_clause"],
            json!({"id": "G1", "heading": "Ship login"})
        );
        assert_eq!(r["recommendation"]["repo"], "a/x");
        assert_eq!(r["recommendation"]["issue"], 5);
        assert!(
            r["recommendation"]["reason"]
                .as_str()
                .unwrap()
                .starts_with("G1 \"Ship login\": priority medium")
        );
        let ledgered: Vec<&str> = r["goal_rank"]["goals"]
            .as_array()
            .unwrap()
            .iter()
            .map(|g| g["repo"].as_str().unwrap())
            .collect();
        assert_eq!(ledgered, vec!["a/x", "b/y"]);
    }

    #[test]
    fn single_repo_display_and_unset() {
        // Changed: no `no_clause` exclusion (an Issue naming none is ordered last); a project
        // with no ledger is unset with its candidate count, and yields nothing.
        let dir = tempfile::tempdir().unwrap();
        write_ledger(dir.path(), "a/b", LEDGER);
        let r = goal_scan(
            &[
                bundle("a/b", vec![named(1, "")]),
                bundle("c/d", vec![named(7, "G1"), named(8, "G1")]),
            ],
            dir.path(),
            3,
        );
        assert_eq!(issues_of(&r, "candidates"), vec![json!(1)]);
        assert!(r["candidates"][0]["goal_clause"].is_null());
        assert_eq!(r["goal_rank"]["goal_unset_repos"][0]["repo"], "c/d");
        assert_eq!(r["goal_rank"]["goal_unset_repos"][0]["candidate_count"], 2);
    }

    #[test]
    fn ledger_error_repo_yields_nothing() {
        let dir = tempfile::tempdir().unwrap();
        write_ledger(dir.path(), "a/b", "## G1\n");
        let r = goal_scan(&[bundle("a/b", vec![named(1, "G1")])], dir.path(), 3);
        assert_eq!(r["goal_rank"]["goal_errors"][0]["repo"], "a/b");
        assert_eq!(r["candidates"], json!([]));
    }
}

// ---- dedup ------------------------------------------------------------------------------------

mod dedup {
    use super::*;

    fn scan_of(cands: Value) -> Value {
        json!({"status": "candidates_found", "candidates": cands})
    }

    fn sample() -> Value {
        scan_of(json!([
            {"repo": "o/b", "issue": 2}, {"repo": null, "issue": 7}, {"repo": "o/a", "issue": 9},
        ]))
    }

    const REFS: &str = "o/a#9,o/b#2,o/home#7";

    #[test]
    fn refs_sorted_with_repo_fallback() {
        // Changed: the fallback repository is an argument (the carried one came from the
        // command line's `repo_resolution`).
        assert_eq!(forward::candidate_refs(&sample(), "o/home"), REFS);
    }

    #[test]
    fn same_set_as_last_worker_close_is_unchanged() {
        // Changed: two keys compared as values; reading the last one sent from `state.db` is
        // Issue 40.
        let reordered = scan_of(json!([
            {"repo": "o/a", "issue": 9}, {"repo": "o/b", "issue": 2}, {"repo": null, "issue": 7},
        ]));
        assert_eq!(
            forward::scan_keys(&sample(), "o/home"),
            forward::scan_keys(&reordered, "o/home")
        );
    }

    #[test]
    fn changed_set_forwards() {
        // Changed: as above.
        let fewer = scan_of(json!([{"repo": "o/a", "issue": 9}]));
        assert_ne!(
            forward::scan_keys(&sample(), "o/home"),
            forward::scan_keys(&fewer, "o/home")
        );
    }
}

// ---- Issue 173: the order by the named clause, the ledger gate, the keys ---------------------

mod issue_173 {
    use super::*;

    const FIVE: &str = "## G1 One\n- unmet if: a\n## G2 Two\n- unmet if: b\n## G3 Three\n- unmet if: c\n\
                        ## G4 Four\n- unmet if: d\n## G5 Five\n- unmet if: e\n";

    fn order(r: &Value) -> Vec<i64> {
        issues_of(r, "candidates")
            .iter()
            .map(|v| v.as_i64().unwrap())
            .collect()
    }

    #[test]
    fn g2_before_g5_before_none() {
        // The one naming none has the best legacy key, and the order is still by clause.
        let dir = tempfile::tempdir().unwrap();
        write_ledger(dir.path(), "o/r", FIVE);
        let none = with(named(10, ""), "labels", json!([{"name": "priority:high"}]));
        let b = bundle("o/r", vec![named(30, "G5"), none, named(20, "G2")]);
        let r = goal_scan(std::slice::from_ref(&b), dir.path(), 3);
        assert_eq!(order(&r), vec![20, 30, 10]);
        let ids: Vec<Value> = r["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["goal_clause"]["id"].clone())
            .collect();
        assert_eq!(ids, vec![json!("G2"), json!("G5"), Value::Null]);
        assert_eq!(r["recommendation"]["issue"], 20);
    }

    #[test]
    fn the_order_comes_before_the_top_n_cut() {
        let dir = tempfile::tempdir().unwrap();
        write_ledger(dir.path(), "o/r", FIVE);
        let mut issues: Vec<Value> = (1..=5).map(|n| issue(n, "", &["priority:high"])).collect();
        issues.push(with(named(9, "G5"), "labels", json!([{"name": "backlog"}])));
        let r = goal_scan(&[bundle("o/r", issues)], dir.path(), 1);
        assert_eq!(order(&r), vec![9]);
        assert_eq!(r["truncated_count"], 5);
    }

    #[test]
    fn a_clause_the_ledger_lacks_counts_as_none() {
        let dir = tempfile::tempdir().unwrap();
        write_ledger(dir.path(), "o/r", LEDGER);
        let r = goal_scan(
            &[bundle("o/r", vec![named(1, "G7"), named(2, "G2")])],
            dir.path(),
            3,
        );
        assert_eq!(order(&r), vec![2, 1]);
        assert!(r["candidates"][1]["goal_clause"].is_null());
        assert!(any_has(&signals_of(&r["candidates"][1]), "names G7"));
        assert!(r["goal_rank"]["signals"].to_string().contains("o/r#1"));
    }

    #[test]
    fn the_named_clause_is_read_under_its_heading_only() {
        let body = "Mentions G1 here.\n- G1 a list line before the heading\n\n## Goal clause\n\n\
                    Prose first.\n- G3 It does three\n- G1 a second one\n\n## Next\n- G2 later\n";
        assert_eq!(goals::named_clause(body), Some(3));
        assert_eq!(
            goals::named_clause("## Goal clause\n\n## Next\n- G2 x\n"),
            None
        );
        assert_eq!(
            goals::named_clause("## Goal clause\n- g2 lowercase\n"),
            None
        );
        assert_eq!(goals::named_clause("## goal clause\n- G2 x\n"), None);
        assert_eq!(
            goals::named_clause("## Goal clause\r\n\r\n- G4 crlf\r\n"),
            Some(4)
        );
        assert_eq!(goals::named_clause("- G2 no heading\n"), None);
    }

    #[test]
    fn a_project_with_no_ledger_yields_none_and_is_unset() {
        // Also when all of its Issues are blocked: the gate is per project, not per candidate.
        let dir = tempfile::tempdir().unwrap();
        write_ledger(dir.path(), "o/ok", LEDGER);
        let r = goal_scan(
            &[
                bundle("o/ok", vec![named(1, "G1")]),
                bundle("o/none", vec![named(2, "G1")]),
                bundle("o/blocked", vec![issue(3, "", &["blocked"])]),
            ],
            dir.path(),
            3,
        );
        assert_eq!(keys(&r, "candidates"), vec![(json!("o/ok"), json!(1))]);
        assert_eq!(
            r["goal_rank"]["goal_unset_repos"],
            json!([
                {"repo": "o/blocked", "path": dir.path().join("o/blocked.md").display().to_string(),
                 "candidate_count": 0},
                {"repo": "o/none", "path": dir.path().join("o/none.md").display().to_string(),
                 "candidate_count": 1},
            ])
        );
    }

    #[test]
    fn a_broken_ledger_is_reported_with_its_reason() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_ledger(dir.path(), "o/r", "# Goals\n## G1\n- unmet if: x\n");
        let r = goal_scan(&[bundle("o/r", vec![named(1, "G1")])], dir.path(), 3);
        assert_eq!(r["status"], "no_candidates");
        assert_eq!(
            r["goal_rank"]["goal_errors"],
            json!([{"repo": "o/r", "path": path.display().to_string(),
                    "error": "line 2: malformed clause heading: ## G1"}])
        );
    }

    #[test]
    fn two_scans_of_the_same_input_give_the_same_keys() {
        let dir = tempfile::tempdir().unwrap();
        write_ledger(dir.path(), "o/r", LEDGER);
        let input = [
            bundle("o/r", vec![named(1, "G1"), named(2, "")]),
            bundle("o/unset", vec![issue(3, "", &[])]),
        ];
        let a = forward::scan_keys(&goal_scan(&input, dir.path(), 3), "");
        let b = forward::scan_keys(&goal_scan(&input, dir.path(), 3), "");
        assert_eq!(a, b);
        assert_eq!(a.candidate_refs, "o/r#1,o/r#2");
        assert_eq!(a.diagnostics, "unset:o/unset");
    }

    #[test]
    fn a_changed_candidate_set_gives_different_keys() {
        let dir = tempfile::tempdir().unwrap();
        write_ledger(dir.path(), "o/r", LEDGER);
        let before = goal_scan(&[bundle("o/r", vec![named(1, "G1")])], dir.path(), 3);
        let more = goal_scan(
            &[bundle("o/r", vec![named(1, "G1"), named(2, "G2")])],
            dir.path(),
            3,
        );
        let blocked = goal_scan(
            &[bundle(
                "o/r",
                vec![named(1, "G1"), issue(2, "Blocked by #1", &[])],
            )],
            dir.path(),
            3,
        );
        let k = |r: &Value| forward::scan_keys(r, "");
        assert_ne!(k(&before), k(&more));
        assert_eq!(k(&before).candidate_refs, k(&blocked).candidate_refs);
        // A ledger that goes missing changes the diagnostics key.
        std::fs::remove_file(dir.path().join("o/r.md")).unwrap();
        let unset = goal_scan(&[bundle("o/r", vec![named(1, "G1")])], dir.path(), 3);
        assert_eq!(k(&unset).candidate_refs, "");
        assert_ne!(k(&before), k(&unset));
    }

    #[test]
    fn collapsed_scans_of_two_repositories_differ() {
        let a = wd::scan_repos(
            &[bundle("o/a", vec![issue(7, "", &[])])],
            &cfg(),
            None,
            Some("o/a"),
            None,
            None,
        );
        let b = wd::scan_repos(
            &[bundle("o/b", vec![issue(7, "", &[])])],
            &cfg(),
            None,
            Some("o/b"),
            None,
            None,
        );
        assert_eq!(a["candidates"][0]["repo"], Value::Null);
        assert_ne!(forward::scan_keys(&a, "o/a"), forward::scan_keys(&b, "o/b"));
    }

    #[test]
    fn gather_reads_one_repository_through_the_seam() {
        let open: Vec<Value> = (1..=3).map(|n| issue(n, "", &[])).collect();
        let mut stub = Stub::default().with("open_issues", open).with(
            "open_prs",
            vec![json!({"number": 8}), json!({"number": 8}), json!({"x": 1})],
        );
        let limits = Limits {
            open: 3,
            ..Limits::default()
        };
        let (b, t) = wd::gather(&mut stub, "o/r", None, &limits).unwrap();
        assert_eq!(b.repo.as_deref(), Some("o/r"));
        assert_eq!(b.open_pr_numbers, vec![8]);
        assert_eq!(
            t,
            Truncation {
                open_issues: true,
                ..Truncation::default()
            }
        );
        let reads: Vec<&str> = stub.calls.iter().map(|c| c.0.as_str()).collect();
        assert_eq!(reads, vec!["open_issues", "open_prs", "recent_merges"]);
        assert!(stub.calls.iter().all(|c| c.1 == "o/r"));
    }
}
