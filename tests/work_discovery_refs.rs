//! Two departures from the carried scan, found in review (Issue 173): a repository named in a ref
//! is matched case-insensitively against the scanned ones, and a `#N` inside a cross ref whose
//! repository name ends in `-` or `.` is not read as the home `#N`.

use makiba::work_discovery::{self as wd, Bundle, ScanConfig, refs};
use serde_json::{Value, json};

fn issue(n: i64, body: &str) -> Value {
    json!({"number": n, "title": "t", "body": body, "labels": [], "updatedAt": "2026-06-01T00:00:00Z"})
}

fn bundle(repo: &str, issues: Vec<Value>) -> Bundle {
    Bundle {
        repo: Some(repo.into()),
        issues,
        ..Bundle::default()
    }
}

fn scan(bundles: &[Bundle]) -> Value {
    wd::scan_repos(bundles, &ScanConfig::default(), None, None, None, None)
}

fn candidates(r: &Value) -> Vec<(Value, Value)> {
    r["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["repo"].clone(), c["issue"].clone()))
        .collect()
}

#[test]
fn a_blocker_named_in_other_case_is_the_scanned_repositorys() {
    let r = scan(&[
        bundle("o/a", vec![issue(1, "Blocked by O/B#12")]),
        bundle("o/b", vec![issue(12, "open")]),
    ]);
    assert_eq!(candidates(&r), vec![(json!("o/b"), json!(12))]);
    assert_eq!(r["excluded_blocked"][0]["blocking_refs"], json!(["o/b#12"]));
}

#[test]
fn a_merge_closing_a_ref_in_other_case_unblocks_its_dependent() {
    let mut b = bundle("o/b", vec![]);
    b.recent_merges = vec![json!({"number": 900, "title": "x", "body": "Closes #80"})];
    let r = scan(&[bundle("o/a", vec![issue(7, "Depends on O/B#80")]), b]);
    assert_eq!(r["candidates"][0]["issue"], 7);
    assert_eq!(r["candidates"][0]["unblocked_by_recent_merge"], true);
    assert!(
        !r["candidates"][0]["signals"]
            .to_string()
            .contains("un-scanned")
    );
}

#[test]
fn a_number_inside_a_cross_ref_is_not_a_home_ref() {
    let text = "Blocked by other/repo-#12";
    assert!(refs::extract_blocking_refs(Some(text), false).is_empty());
    assert_eq!(
        refs::extract_cross_repo_blocking_refs(Some(text), false),
        vec![("other/repo-".to_string(), 12)]
    );
    assert!(
        refs::pr_close_refs("Closes other/repo.#12, #3")
            .into_iter()
            .eq([3])
    );
    // The home #12 is open, and the Issue blocked by other/repo-#12 is not blocked by it.
    let r = scan(&[bundle(
        "o/a",
        vec![issue(1, "Blocked by other/repo-#12"), issue(12, "open")],
    )]);
    assert_eq!(r["candidate_count"], 2);
    // A merge into a base branch closing other/repo-#12 does not close the home #12.
    let mut b = bundle("o/a", vec![issue(12, "open")]);
    b.base_branch = Some("develop".into());
    b.base_merges = vec![
        json!({"number": 5, "title": "x", "body": "Closes other/repo-#12",
                                "baseRefName": "develop", "mergedAt": "2026-06-02T00:00:00Z"}),
    ];
    let r = scan(&[b]);
    assert_eq!(r["excluded_merged"], json!([]));
    assert_eq!(r["candidate_count"], 1);
}
