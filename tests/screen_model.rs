//! Issue 169, step 1 of docs/design/screen.md section 7: the screen's model. A pure function from
//! what `makiba status --json` prints (and one task's events) to what each part of the screen
//! shows, in the person's words, with every control as a line to type (screen.md section 4).
//! Every status here is built by the helpers below; `now` is 2026-11-24T10:00:00Z.
//! Includes the rules of Issue 415 (a repository registered by several projects).

mod common;

use makiba::screen::{self, Words};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

/// 2026-11-24T10:00:00Z in milliseconds.
const NOW_MS: i64 = 1_795_514_400_000;

fn en() -> Words {
    Words::new("en")
}

fn model(status: &Value) -> Value {
    screen::model(status, NOW_MS, &en())
}

fn n(v: i64) -> (&'static str, String) {
    ("n", v.to_string())
}

fn x(s: &str) -> (&'static str, String) {
    ("x", s.to_string())
}

fn at(min_before: i64) -> String {
    // Minutes before 10:00 on 2026-11-24, at most 600.
    let total = 10 * 60 - min_before;
    format!("2026-11-24T{:02}:{:02}:00.000Z", total / 60, total % 60)
}

fn status() -> Value {
    json!({
        "makibad": {"pid": 4242, "heartbeat_at": "2026-11-24T09:59:57.000Z",
                    "subscribed": true, "running": true},
        "merge_approval_mode": "human",
        "merge_approval_mode_here": "human",
        "organization": "makiba",
        "projects": [],
        "runs": [],
        "terminals": [{"role": "secretary", "name": "secretary", "state": "idle",
                       "since": at(2)}],
        "anomalies": [],
        "pending": [],
        "pr_watches": [],
        "pull_requests": [],
        "ranges": [],
        "belt": [],
        "next_work": null
    })
}

fn project(slug: &str, repo: Option<&str>) -> Value {
    json!({"slug": slug, "repo": repo, "default_branch": "main", "network_hosts": "",
           "goal_ledger": {"state": "ok", "clauses": 3, "placed_at": null, "placed_by": null}})
}

fn run(task: &str, project: &str, status: &str, stage: &str, range_id: Option<i64>) -> Value {
    json!({"task_id": task, "kind": "code", "project": project, "status": status,
           "stage": stage, "failed_reason": null, "waiting": null, "unparked": null,
           "notice": null, "secretary_acted_at": null, "secretary_acted_verb": null,
           "title": format!("Title of task {task}"), "since": at(300), "issue": null,
           "range_id": range_id})
}

fn item(
    issue: i64,
    place: &str,
    task: Option<&str>,
    stage: Option<&str>,
    why: Option<&str>,
) -> Value {
    json!({"issue": issue, "title": format!("Issue {issue} title"), "place": place,
           "why": why, "stage": stage, "since": at(60), "task_id": task, "pr": null,
           "pr_title": null})
}

const PLACES: [&str; 7] = [
    "landed",
    "closed",
    "waiting_person",
    "review",
    "in_work",
    "not_in",
    "skipped",
];

fn range(id: i64, project: &str, status: &str, items: Vec<Value>) -> Value {
    let mut places = serde_json::Map::new();
    for p in PLACES {
        let c = items.iter().filter(|i| i["place"] == p).count();
        places.insert(p.to_string(), json!(c));
    }
    let issues: Vec<Value> = items.iter().map(|i| i["issue"].clone()).collect();
    json!({"id": id, "project": project, "status": status, "ended_reason": null,
           "issues": issues, "takes": null, "brief_head": format!("Brief of range {id}."),
           "at_once": 2, "approval_mode": "human", "approves_under": null, "replaces": null,
           "follow_ups": "inside", "design_ahead": false,
           "budget": {"tasks": 10, "hours": null, "fixes": 3},
           "used": {"tasks": 4, "hours": 5.0}, "approved_by": "person",
           "approved_at": format!("2026-11-2{}T08:00:00.000Z", id.clamp(0, 3)),
           "approved_read_at": null, "read_id": 70 + id, "read_at": at(30),
           "read_complete": true, "skipped": [], "candidates": [], "items": items,
           "places": places, "stops": [], "stops_taken": {}})
}

fn pending(id: i64, task: Option<&str>, status: &str, kind: &str, question: &str) -> Value {
    json!({"id": id, "task_id": task, "kind": kind, "question": question, "status": status,
           "created_at": at(20),
           "context": {"background": null, "if_yes": null, "if_no": null, "recommend": null,
                       "why": null},
           "stop_line": null, "range_id": null, "to": null})
}

fn anomaly(task: Option<&str>, rule: &str, since: String) -> Value {
    json!({"task_id": task, "rule": rule, "since": since, "notice": null})
}

fn next_work(status: &str, count: i64) -> Value {
    json!({"scan_id": 900, "scanned_at": at(19), "trigger": "run_closed", "status": status,
           "error": if status == "error" { json!("gh: timed out") } else { Value::Null },
           "candidate_count": count, "input_truncated": "", "candidates": [],
           "candidates_not_recorded": 0, "left_out": [], "left_out_not_recorded": 0})
}

fn candidate(repo: &str, issue: i64) -> Value {
    json!({"repo": repo, "issue": issue, "title": format!("Candidate {issue}"),
           "clause_id": "G2", "clause_heading": "Goal two", "why": "priority",
           "size": "M", "size_estimated": true, "recommended": false, "forwarded": false})
}

fn left_out(project: Option<&str>, repo: Option<&str>, issue: Option<i64>, kind: &str) -> Value {
    json!({"project": project, "repo": repo, "issue": issue, "title": issue.map(|i| format!("Held {i}")),
           "kind": kind, "reason": "Do the other one first."})
}

fn pull_request(repo: &str, pr: i64, task: &str, ci: Value, state: &str) -> Value {
    json!({"repo": repo, "pr": pr, "task_id": task, "title": format!("PR {pr}"),
           "head_oid": "a".repeat(40), "ci": ci, "state": state, "merged_via": null,
           "merged_by": null, "merge_state": null, "merge_order": []})
}

fn push(status: &mut Value, field: &str, v: Value) {
    status[field].as_array_mut().unwrap().push(v);
}

fn find<'a>(rows: &'a Value, key: &str, want: &Value) -> &'a Value {
    rows.as_array()
        .unwrap()
        .iter()
        .find(|r| &r[key] == want)
        .unwrap_or_else(|| panic!("no row with {key} = {want} in {rows}"))
}

/// The wording file of `lang`, as `(key, text)` lines.
fn table(lang: &str) -> Vec<(String, String)> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("roles/locale/screen/{lang}.tsv"));
    let text = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split('\t').collect();
        assert_eq!(
            parts.len(),
            2,
            "{}:{}: not key<TAB>text: {line:?}",
            p.display(),
            i + 1
        );
        assert!(
            !parts[0].is_empty() && !parts[1].is_empty(),
            "{}:{}",
            p.display(),
            i + 1
        );
        out.push((parts[0].to_string(), parts[1].to_string()));
    }
    out
}

/// Every string found under any of `keys`, at any depth.
fn strings_under(v: &Value, keys: &[&str], out: &mut Vec<String>) {
    fn all(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::String(s) => out.push(s.clone()),
            Value::Array(a) => a.iter().for_each(|e| all(e, out)),
            Value::Object(o) => o.values().for_each(|e| all(e, out)),
            _ => {}
        }
    }
    match v {
        Value::Array(a) => a.iter().for_each(|e| strings_under(e, keys, out)),
        Value::Object(o) => {
            for (k, e) in o {
                if keys.contains(&k.as_str()) {
                    all(e, out);
                } else {
                    strings_under(e, keys, out);
                }
            }
        }
        _ => {}
    }
}

/// The range of T3: nine items, one of each place, two not started, two being worked on, one of
/// which did not start; one escalated question with its whole context, a stop recommending yes.
fn mixed() -> Value {
    let mut s = status();
    push(
        &mut s,
        "projects",
        project("loom", Some("example-org/loom")),
    );
    let items = vec![
        item(51, "landed", Some("r3-51"), None, None),
        item(52, "closed", Some("r3-52"), None, Some("no merge recorded")),
        item(
            53,
            "waiting_person",
            Some("r3-53"),
            None,
            Some("decision: Should the cache be shared?"),
        ),
        item(54, "review", Some("r3-54"), Some("ci"), None),
        item(55, "in_work", Some("r3-55"), Some("at_work"), None),
        item(56, "in_work", Some("r3-56"), Some("launch_failed"), None),
        item(57, "not_in", None, None, Some("no_slot")),
        item(58, "not_in", None, None, Some("budget")),
        item(59, "skipped", None, None, Some("out of scope")),
    ];
    let mut r = range(3, "loom", "active", items);
    r["stops"] = json!([{"id": 14, "task_id": "r3-53", "question": "Should the cache be shared?",
                         "kind": "design", "recommend_answer": "yes", "answer": null,
                         "status": "escalated"}]);
    push(&mut s, "ranges", r);
    push(
        &mut s,
        "runs",
        run("r3-53", "loom", "in_use", "verified", Some(3)),
    );
    push(
        &mut s,
        "runs",
        run("r3-54", "loom", "review", "verified", Some(3)),
    );
    push(
        &mut s,
        "runs",
        run("r3-55", "loom", "in_use", "verified", Some(3)),
    );
    push(
        &mut s,
        "runs",
        run("r3-56", "loom", "failed", "failed", Some(3)),
    );
    let mut p = pending(
        14,
        Some("r3-53"),
        "escalated",
        "question",
        "Should the cache be shared?",
    );
    p["context"] = json!({"background": "B", "if_yes": "Y", "if_no": "N", "recommend": "yes",
                          "why": "W"});
    p["stop_line"] = json!("decision");
    p["range_id"] = json!(3);
    p["to"] = json!("person");
    push(&mut s, "pending", p);
    s
}

#[test]
fn t1_wording_files_parse_and_japanese_falls_back_to_english() {
    let en_t = table("en");
    let ja_t = table("ja");
    let en_keys: Vec<&String> = en_t.iter().map(|(k, _)| k).collect();
    let unique: BTreeSet<&String> = en_keys.iter().copied().collect();
    assert_eq!(unique.len(), en_keys.len(), "a key twice in en.tsv");
    for (k, _) in &ja_t {
        assert!(unique.contains(k), "ja.tsv key {k} is not in en.tsv");
    }
    let listed: BTreeSet<String> = Words::keys("en").iter().map(|k| k.to_string()).collect();
    let file: BTreeSet<String> = en_t.iter().map(|(k, _)| k.clone()).collect();
    assert_eq!(listed, file);
    let ja = Words::new("ja");
    assert_ne!(ja.get("answer.yes"), en().get("answer.yes"));
    assert!(!ja.get("answer.yes").is_ascii());
    // A key the Japanese table lacks is shown in English: the first such key of en.tsv, read
    // from the two tables, so the check does not depend on which key the owner has translated.
    let ja_keys: BTreeSet<&String> = ja_t.iter().map(|(k, _)| k).collect();
    let untranslated = en_keys
        .iter()
        .find(|k| !ja_keys.contains(*k))
        .expect("every key has Japanese: no key left to show the fallback");
    assert_eq!(ja.get(untranslated), en().get(untranslated));
    assert_eq!(en().get("no.such.key"), "no.such.key");
    assert_eq!(ja.get("no.such.key"), "no.such.key");
}

#[test]
fn t2_every_word_is_from_the_table_and_no_word_is_makibas_own() {
    let templates: Vec<regex::Regex> = table("en")
        .iter()
        .map(|(_, t)| {
            let parts: Vec<String> = regex::Regex::new(r"\{[a-z]+\}")
                .unwrap()
                .split(t)
                .map(regex::escape)
                .collect();
            regex::Regex::new(&format!("^{}$", parts.join(".+"))).unwrap()
        })
        .collect();
    let m = model(&mixed());
    let mut found = Vec::new();
    strings_under(
        &m,
        &[
            "words",
            "who_words",
            "status_words",
            "budget_words",
            "mark_words",
            "also_words",
            "heading_words",
        ],
        &mut found,
    );
    assert!(!found.is_empty());
    for s in &found {
        assert!(
            templates.iter().any(|t| t.is_match(s)),
            "not from en.tsv: {s:?}"
        );
    }
    let kinds = [
        en().get("answer.kind_decision"),
        en().get("answer.kind_question"),
        en().get("answer.kind_permission"),
    ];
    for e in m["answers"]["entries"].as_array().unwrap() {
        let h = e["heading"].as_str().unwrap();
        let kind = h.split(" \u{b7} ").next().unwrap();
        assert!(kinds.iter().any(|k| k == kind), "heading kind {kind:?}");
    }
    let banned = regex::Regex::new(
        r"(?i)\b(range|run|place|stage|scan|belt|anomaly|dispatch|launch|brief)\b",
    )
    .unwrap();
    for (k, t) in table("en") {
        assert!(
            !banned.is_match(&t),
            "{k}: {t:?} holds one of makiba's own terms"
        );
    }
}

#[test]
fn t3_a_range_of_mixed_places() {
    let s = mixed();
    let m = model(&s);
    let r = &m["ranges"][0];
    assert_eq!(r["places"], s["ranges"][0]["places"]);
    assert_eq!(r["selected"], true);
    assert_eq!(r["did_not_start"], 1);
    assert_eq!(
        r["band"]["cells"],
        json!([
            "landed",
            "closed",
            "waiting_person",
            "review",
            "in_work",
            "did_not_start",
            "not_in",
            "not_in",
            "skipped"
        ])
    );
    let active: Vec<i64> = r["active"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["issue"].as_i64().unwrap())
        .collect();
    assert_eq!(active, vec![56, 53, 55, 54]);
    let e = &m["answers"]["entries"][0];
    assert_eq!(m["answers"]["entries"].as_array().unwrap().len(), 1);
    for (k, v) in [
        ("background", "B"),
        ("if_yes", "Y"),
        ("if_no", "N"),
        ("recommend", "yes"),
        ("why", "W"),
    ] {
        assert_eq!(e[k], v, "{k}");
    }
    assert_eq!(e["recommended_answer"], "yes");
    let q = "Should the cache be shared?";
    assert_eq!(e["question"], q);
    assert_eq!(
        e["heading"],
        format!("{} \u{b7} Issue 53 title", en().get("answer.kind_decision"))
    );
    assert!(!e["heading"].as_str().unwrap().contains(q));
    assert_eq!(
        e.to_string().matches(q).count(),
        1,
        "the question once: {e}"
    );
    assert_eq!(m["top"]["needs_you"], 1);
    assert_eq!(m["top"]["needs_you_in"], json!(["loom"]));
}

#[test]
fn t4_a_worker_verified_and_then_gone_is_its_anomaly_only() {
    let mut s = status();
    push(
        &mut s,
        "projects",
        project("loom", Some("example-org/loom")),
    );
    push(
        &mut s,
        "ranges",
        range(
            3,
            "loom",
            "active",
            vec![item(
                60,
                "in_work",
                Some("r3-60"),
                Some("launch_failed"),
                None,
            )],
        ),
    );
    push(
        &mut s,
        "runs",
        run("r3-60", "loom", "failed", "verified", Some(3)),
    );
    push(&mut s, "anomalies", anomaly(Some("r3-60"), "gone", at(10)));
    let m = model(&s);
    let rows = m["failures"]["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["kind"], "gone");
    assert_eq!(rows[0]["task_id"], "r3-60");
    assert!(rows.iter().all(|r| r["kind"] != "launch_failed"));
    assert_eq!(m["ranges"][0]["did_not_start"], 0);
    assert_eq!(m["ranges"][0]["items"][0]["words"], en().get("fail.gone"));
}

#[test]
fn t5_a_suspended_run_has_not_gone_wrong_and_a_failed_launch_keeps_its_reason() {
    let mut s = status();
    push(
        &mut s,
        "projects",
        project("loom", Some("example-org/loom")),
    );
    push(
        &mut s,
        "runs",
        run("s1", "loom", "suspended", "failed", None),
    );
    assert_eq!(model(&s)["failures"]["rows"], json!([]));
    let mut f = run("f1", "loom", "failed", "failed", None);
    f["failed_reason"] = json!("no first turn within 120 s");
    push(&mut s, "runs", f);
    let m = model(&s);
    let rows = m["failures"]["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["kind"], "launch_failed");
    assert_eq!(rows[0]["task_id"], "f1");
    assert_eq!(rows[0]["reason"], "no first turn within 120 s");
}

#[test]
fn t6_who_is_on_each_failure() {
    let mut s = status();
    push(
        &mut s,
        "projects",
        project("loom", Some("example-org/loom")),
    );
    for t in ["r-a", "r-b", "r-c", "r-d", "r-e", "r-f"] {
        push(&mut s, "runs", run(t, "loom", "in_use", "verified", None));
    }
    for t in ["r-a", "r-b", "r-c", "r-d", "r-e"] {
        push(&mut s, "anomalies", anomaly(Some(t), "error", at(60)));
    }
    push(
        &mut s,
        "anomalies",
        anomaly(Some("r-f"), "blocked_permission", at(60)),
    );
    push(
        &mut s,
        "pending",
        pending(30, Some("r-a"), "escalated", "question", "Go on?"),
    );
    s["runs"][1]["secretary_acted_at"] = json!(at(50));
    s["runs"][1]["secretary_acted_verb"] = json!("send");
    s["anomalies"][2]["notice"] = json!({"to": "secretary", "state": "delivered",
                                         "written_at": at(60), "delivered_at": at(59)});
    s["anomalies"][3]["notice"] = json!({"to": "secretary", "state": "queued",
                                         "written_at": at(60), "delivered_at": null});
    push(
        &mut s,
        "pending",
        pending(31, Some("r-f"), "open", "prompt", "Allow Bash?"),
    );
    let m = model(&s);
    let rows = &m["failures"]["rows"];
    for (t, who) in [
        ("r-a", "you"),
        ("r-b", "looking"),
        ("r-c", "told"),
        ("r-d", "no_sign"),
        ("r-e", "no_sign"),
        ("r-f", "no_sign"),
    ] {
        let r = find(rows, "task_id", &json!(t));
        assert_eq!(r["who"], who, "{t}");
        assert_eq!(r["unattended"], who == "no_sign", "{t}");
    }
    assert_eq!(m["top"]["unattended"], 3);

    // The secretary still: nobody is on any row.
    let mut s2 = s.clone();
    s2["pending"] = json!([]);
    push(
        &mut s2,
        "anomalies",
        anomaly(None, "secretary_still", at(5)),
    );
    let m2 = model(&s2);
    let rows2 = m2["failures"]["rows"].as_array().unwrap();
    assert_eq!(rows2.len(), 7);
    for r in rows2 {
        assert_eq!(r["who"], "nobody", "{r}");
        assert_eq!(r["unattended"], true);
    }
    assert_eq!(m2["top"]["unattended"], 7);
}

/// Six rows gone wrong: a failed check of GitHub and five reported errors nobody was told of.
fn six() -> Value {
    let mut s = status();
    push(
        &mut s,
        "projects",
        project("loom", Some("example-org/loom")),
    );
    for i in 1..=5 {
        let t = format!("t{i}");
        push(&mut s, "runs", run(&t, "loom", "in_use", "verified", None));
        push(&mut s, "anomalies", anomaly(Some(&t), "error", at(10 * i)));
    }
    s["next_work"] = next_work("error", 0);
    s
}

#[test]
fn t7_failures_are_capped_at_three_and_the_rest_is_counted() {
    let m = model(&six());
    let f = &m["failures"];
    assert_eq!(f["rows"].as_array().unwrap().len(), 6);
    assert_eq!(f["shown"].as_array().unwrap().len(), 3);
    assert_eq!(f["more"], 3);
    assert_eq!(f["more_unattended"], 3);
    let scan = find(&f["shown"], "kind", &json!("scan_failed"));
    assert_eq!(scan["project"], Value::Null);
    assert_eq!(m["top"]["failed"], 6);
    assert_eq!(m["top"]["failed_in"]["organization"], 1);
    assert_eq!(m["top"]["failed_in"]["loom"], 5);
}

#[test]
fn t8_each_state_of_the_check_of_github_says_why_the_list_is_empty() {
    let mut s = status();
    let mut states = Vec::new();
    let mut words = BTreeSet::new();
    let mut not_recorded = next_work("candidates_found", 4);
    not_recorded["candidates"] = Value::Null;
    not_recorded["left_out"] = Value::Null;
    for nw in [
        Value::Null,
        next_work("error", 0),
        next_work("no_candidates", 0),
        not_recorded,
    ] {
        s["next_work"] = nw;
        let m = model(&s);
        states.push(m["ready"]["state"].clone());
        words.insert(m["ready"]["words"].as_str().unwrap().to_string());
    }
    assert_eq!(
        states,
        vec![
            json!("no_check"),
            json!("check_failed"),
            json!("none"),
            json!("not_recorded")
        ]
    );
    assert_eq!(words.len(), 4);
}

#[test]
fn t8b_a_list_cut_short_is_not_an_empty_one() {
    let mut s = status();
    push(
        &mut s,
        "projects",
        project("loom", Some("example-org/loom")),
    );
    push(
        &mut s,
        "ranges",
        range(
            3,
            "loom",
            "active",
            vec![
                item(61, "not_in", None, None, Some("no_slot")),
                item(62, "not_in", None, None, Some("no_slot")),
            ],
        ),
    );
    let mut nw = next_work("candidates_found", 7);
    nw["candidates"] = json!([
        candidate("example-org/loom", 61),
        candidate("example-org/loom", 62)
    ]);
    nw["candidates_not_recorded"] = json!(5);
    nw["left_out_not_recorded"] = json!(3);
    s["next_work"] = nw;
    let m = model(&s);
    assert_eq!(m["ready"]["state"], "listed");
    assert_eq!(m["ready"]["rows"], json!([]));
    assert_eq!(m["ready"]["not_listed"], 5);
    assert_eq!(
        m["ready"]["not_listed_words"],
        en().fill("list.not_recorded", &[n(5)])
    );
    assert_eq!(m["ready"]["left_out_not_listed"], 3);
    s["next_work"]["candidates_not_recorded"] = json!(0);
    s["next_work"]["candidate_count"] = json!(2);
    assert_eq!(model(&s)["ready"]["state"], "all_held");
}

#[test]
fn t9_held_issues_unranked_projects_and_a_repository_no_project_has() {
    let mut s = status();
    push(
        &mut s,
        "projects",
        project("loom", Some("example-org/loom")),
    );
    push(
        &mut s,
        "projects",
        project("quill", Some("example-org/quill")),
    );
    push(&mut s, "projects", project("atlas", None));
    push(
        &mut s,
        "ranges",
        range(
            3,
            "loom",
            "active",
            vec![item(56, "not_in", None, None, Some("no_slot"))],
        ),
    );
    let mut nw = next_work("candidates_found", 3);
    nw["candidates"] = json!([
        candidate("Example-Org/Loom", 56),
        candidate("example-org/quill", 56),
        candidate("example-org/other", 7)
    ]);
    nw["left_out"] = json!([
        left_out(
            Some("loom"),
            Some("example-org/loom"),
            None,
            "no_goal_ledger"
        ),
        left_out(Some("atlas"), None, None, "no_repository"),
        left_out(
            Some("quill"),
            Some("example-org/quill"),
            Some(40),
            "blocked"
        )
    ]);
    s["next_work"] = nw;
    let m = model(&s);
    let refs: Vec<&Value> = m["ready"]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| &r["ref"])
        .collect();
    assert_eq!(
        refs,
        vec![&json!("quill#56"), &json!("example-org/other#7")]
    );
    let held = find(&m["ready"]["held"], "ref", &json!("loom#56"));
    assert_eq!(held["range_id"], 3);
    assert_eq!(held["project"], "loom");
    let unranked = m["ready"]["unranked"].as_array().unwrap();
    assert_eq!(unranked.len(), 2);
    assert_eq!(unranked[0]["projects"], json!(["loom"]));
    assert_eq!(unranked[1]["projects"], json!(["atlas"]));
    let not_ready = m["ready"]["not_ready"].as_array().unwrap();
    assert_eq!(not_ready.len(), 1);
    assert_eq!(not_ready[0]["ref"], "quill#40");
}

#[test]
fn t9b_a_repository_registered_by_two_projects() {
    let mut s = status();
    push(
        &mut s,
        "projects",
        project("harbor", Some("example-org/harbor")),
    );
    push(
        &mut s,
        "projects",
        project("harbor-site", Some("Example-Org/Harbor")),
    );
    let mut nw = next_work("candidates_found", 1);
    nw["candidates"] = json!([candidate("example-org/harbor", 31)]);
    nw["left_out"] = json!([
        left_out(
            Some("harbor"),
            Some("example-org/harbor"),
            Some(33),
            "blocked"
        ),
        left_out(
            Some("harbor-site"),
            Some("example-org/harbor"),
            Some(33),
            "blocked"
        ),
        left_out(Some("p1"), None, None, "no_repository"),
        left_out(Some("p2"), None, None, "no_repository"),
    ]);
    s["next_work"] = nw;
    let m = model(&s);
    let rows = m["ready"]["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    let r = &rows[0];
    assert_eq!(r["projects"], json!(["harbor", "harbor-site"]));
    assert_eq!(r["ref"], "harbor#31");
    assert_eq!(
        r["also_words"],
        en().fill("shared.also", &[x("harbor-site")])
    );
    let lines = r["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 2);
    for (l, slug) in lines.iter().zip(["harbor", "harbor-site"]) {
        assert_eq!(l["project"], slug);
        assert_eq!(l["words"], en().fill("shared.ask_for", &[x(slug)]));
        assert_eq!(
            l["line"],
            format!(
                "makiba send secretary 'Please take up example-org/harbor#31 for the project {slug}.'"
            )
        );
    }
    let not_ready = m["ready"]["not_ready"].as_array().unwrap();
    assert_eq!(not_ready.len(), 1);
    assert_eq!(not_ready[0]["projects"], json!(["harbor", "harbor-site"]));
    assert_eq!(m["ready"]["unranked"].as_array().unwrap().len(), 2);
    let harbor = find(&m["projects"], "slug", &json!("harbor"));
    assert_eq!(
        harbor["heading_words"],
        en().fill(
            "shared.heading",
            &[("repo", "example-org/harbor".to_string()), x("harbor-site")]
        )
    );
}

#[test]
fn t10_every_reference_names_its_project() {
    let mut s = status();
    push(
        &mut s,
        "projects",
        project("loom", Some("example-org/loom")),
    );
    push(
        &mut s,
        "projects",
        project("quill", Some("example-org/quill")),
    );
    push(&mut s, "projects", project("atlas", None));
    push(
        &mut s,
        "ranges",
        range(
            3,
            "loom",
            "active",
            vec![item(
                56,
                "in_work",
                Some("r3-56"),
                Some("launch_failed"),
                None,
            )],
        ),
    );
    push(
        &mut s,
        "ranges",
        range(
            4,
            "atlas",
            "active",
            vec![item(7, "not_in", None, None, None)],
        ),
    );
    let mut r = run("r3-56", "loom", "failed", "failed", Some(3));
    r["issue"] = json!(56);
    push(&mut s, "runs", r);
    let mut q = run("q-1", "quill", "review", "verified", None);
    q["issue"] = json!(12);
    push(&mut s, "runs", q);
    push(
        &mut s,
        "pull_requests",
        pull_request("example-org/quill", 127, "q-1", json!("running"), "open"),
    );
    let mut nw = next_work("candidates_found", 1);
    nw["candidates"] = json!([candidate("example-org/quill", 56)]);
    s["next_work"] = nw;
    let m = model(&s);
    let refs = |v: &Value| -> Vec<String> {
        let mut out = Vec::new();
        strings_under(v, &["ref"], &mut out);
        out
    };
    let shape = regex::Regex::new(r"^[a-z][a-z-]*#[0-9]+$").unwrap();
    for part in ["ranges", "failures", "ready", "pull_requests"] {
        let rs = refs(&m[part]);
        assert!(!rs.is_empty(), "{part}");
        for r in rs {
            assert!(shape.is_match(&r), "{part}: {r}");
        }
    }
    assert_eq!(m["ranges"][0]["items"][0]["ref"], "loom#56");
    assert_eq!(m["ranges"][1]["items"][0]["ref"], "atlas#7");
    assert_eq!(m["failures"]["rows"][0]["ref"], "loom#56");
    assert_eq!(m["pull_requests"][0]["ref"], "quill#127");
    assert_eq!(m["ready"]["rows"][0]["ref"], "quill#56");
}

#[test]
fn t11_ranges_selected_offered_and_whole() {
    let mut s = status();
    for p in ["loom", "quill", "harbor", "atlas"] {
        push(
            &mut s,
            "projects",
            project(p, Some(&format!("example-org/{p}"))),
        );
    }
    let brief: String = "Every task starts from this. ".repeat(100);
    let mut r1 = range(
        1,
        "loom",
        "active",
        vec![item(1, "in_work", Some("r1-1"), Some("at_work"), None)],
    );
    r1["takes"] = json!("label:ready AND no-label:backlog");
    r1["brief_head"] = json!(brief.clone());
    r1["at_once"] = json!(3);
    let r2 = range(
        2,
        "quill",
        "active",
        vec![item(
            2,
            "waiting_person",
            Some("r2-2"),
            None,
            Some("decision: ?"),
        )],
    );
    let r3 = range(
        3,
        "harbor",
        "active",
        vec![item(
            3,
            "in_work",
            Some("r3-3"),
            Some("launch_failed"),
            None,
        )],
    );
    let mut r4 = range(
        4,
        "atlas",
        "proposed",
        vec![item(80, "not_in", None, None, None)],
    );
    let issues: Vec<i64> = (1..=12).collect();
    r4["issues"] = json!(issues);
    r4["takes"] = json!("label:ready AND no-label:backlog AND clause:<=G4");
    r4["brief_head"] = json!(brief.clone());
    r4["read_id"] = json!(77);
    r4["approves_under"] = json!("human");
    r4["candidates"] = json!([{"issue": 80, "title": "Candidate 80", "admitted": true,
                               "left_out_why": null, "design_only_waits": null},
                              {"issue": 81, "title": "Candidate 81", "admitted": false,
                               "left_out_why": "waits_for #80", "design_only_waits": null}]);
    let mut r5 = range(
        5,
        "loom",
        "ended",
        vec![item(33, "in_work", Some("r2-33"), Some("at_work"), None)],
    );
    r5["ended_reason"] = json!("replaced");
    r5["brief_head"] = json!(brief.clone());
    for r in [r1, r2, r3, r4, r5] {
        push(&mut s, "ranges", r);
    }
    push(
        &mut s,
        "runs",
        run("r1-1", "loom", "in_use", "verified", Some(1)),
    );
    push(
        &mut s,
        "runs",
        run("r2-2", "quill", "in_use", "verified", Some(2)),
    );
    push(
        &mut s,
        "runs",
        run("r3-3", "harbor", "failed", "failed", Some(3)),
    );
    push(
        &mut s,
        "runs",
        run("r2-33", "loom", "in_use", "verified", Some(5)),
    );
    push(
        &mut s,
        "pending",
        pending(40, Some("r2-2"), "escalated", "question", "Which?"),
    );
    let m = model(&s);
    let selected: Vec<i64> = m["ranges"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["selected"] == true)
        .map(|r| r["id"].as_i64().unwrap())
        .collect();
    assert_eq!(selected, vec![2]);
    let others = m["others"].as_array().unwrap();
    assert_eq!(others.len(), 3);
    assert_eq!(others[0], 3);
    assert_eq!(m["others_more"], 1);
    let ended = find(&m["ranges"], "id", &json!(5));
    assert_eq!(ended["terms"]["brief_head"], brief);
    assert!(
        m["one_off"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["task_id"] != "r2-33")
    );
    let first = find(&m["ranges"], "id", &json!(1));
    assert_eq!(first["terms"]["takes"], "label:ready AND no-label:backlog");
    assert_eq!(first["terms"]["brief_head"], brief);
    assert_eq!(first["terms"]["at_once"], 3);
    assert_eq!(first["terms"]["issues"], json!([1]));
    let proposed = find(&m["ranges"], "id", &json!(4));
    assert_eq!(proposed["terms"]["issues"], json!(issues));
    assert_eq!(
        proposed["terms"]["takes"],
        "label:ready AND no-label:backlog AND clause:<=G4"
    );
    assert_eq!(proposed["terms"]["brief_head"], brief);
    assert_eq!(proposed["proposal"]["read_id"], 77);

    // A paused range holds its Issues too.
    let mut p = status();
    push(
        &mut p,
        "projects",
        project("loom", Some("example-org/loom")),
    );
    push(
        &mut p,
        "ranges",
        range(
            8,
            "loom",
            "paused",
            vec![item(70, "not_in", None, None, None)],
        ),
    );
    let mut nw = next_work("candidates_found", 1);
    nw["candidates"] = json!([candidate("example-org/loom", 70)]);
    p["next_work"] = nw;
    let mp = model(&p);
    assert_eq!(mp["ready"]["rows"], json!([]));
    assert_eq!(
        find(&mp["ready"]["held"], "ref", &json!("loom#70"))["range_id"],
        8
    );

    // Thirteen items: the band is pieces, in the order of the places.
    let mut t = status();
    push(
        &mut t,
        "projects",
        project("loom", Some("example-org/loom")),
    );
    let mut items = Vec::new();
    let mut k = 100;
    let mut add = |place: &str, stage: Option<&str>, count: usize, items: &mut Vec<Value>| {
        for _ in 0..count {
            k += 1;
            let task = format!("r9-{k}");
            items.push(item(k, place, Some(&task), stage, None));
        }
    };
    add("landed", None, 3, &mut items);
    add("closed", None, 1, &mut items);
    add("waiting_person", None, 1, &mut items);
    add("review", Some("ci"), 2, &mut items);
    add("in_work", Some("at_work"), 2, &mut items);
    add("in_work", Some("launch_failed"), 1, &mut items);
    add("not_in", None, 2, &mut items);
    add("skipped", None, 1, &mut items);
    let failed_task = items[9]["task_id"].as_str().unwrap().to_string();
    push(&mut t, "ranges", range(9, "loom", "active", items));
    push(
        &mut t,
        "runs",
        run(&failed_task, "loom", "failed", "failed", Some(9)),
    );
    let mt = model(&t);
    let r = &mt["ranges"][0];
    assert_eq!(r["place_order"], json!(PLACES));
    assert_eq!(
        r["band"]["pieces"],
        json!([{"place": "landed", "count": 3}, {"place": "closed", "count": 1},
               {"place": "waiting_person", "count": 1}, {"place": "review", "count": 2},
               {"place": "in_work", "count": 2}, {"place": "did_not_start", "count": 1},
               {"place": "not_in", "count": 2}, {"place": "skipped", "count": 1}])
    );
}

#[test]
fn t12_one_off_tasks_stopped_first_and_pull_requests_failed_first() {
    let mut s = status();
    push(
        &mut s,
        "projects",
        project("loom", Some("example-org/loom")),
    );
    push(
        &mut s,
        "runs",
        run("o-a", "loom", "in_use", "verified", None),
    );
    push(&mut s, "runs", run("o-b", "loom", "failed", "failed", None));
    push(
        &mut s,
        "runs",
        run("o-c", "loom", "in_use", "verified", None),
    );
    let mut d = run("o-d", "loom", "in_use", "verified", None);
    d["waiting"] = json!({"for": "person", "pending": [50], "why": "Which one?"});
    push(&mut s, "runs", d);
    push(
        &mut s,
        "runs",
        run("o-e", "loom", "in_use", "verified", None),
    );
    push(
        &mut s,
        "runs",
        run("r9-1", "loom", "in_use", "verified", Some(9)),
    );
    push(&mut s, "anomalies", anomaly(Some("o-c"), "error", at(10)));
    push(
        &mut s,
        "pending",
        pending(50, Some("o-d"), "escalated", "question", "Which one?"),
    );
    for (t, state) in [("o-a", "working"), ("o-e", "blocked")] {
        push(
            &mut s,
            "terminals",
            json!({"role": "worker", "name": format!("w-{t}"),
                                         "state": state, "since": at(30)}),
        );
    }
    push(
        &mut s,
        "pull_requests",
        pull_request("example-org/loom", 1, "o-a", json!("passed"), "merged"),
    );
    push(
        &mut s,
        "pull_requests",
        pull_request("example-org/loom", 2, "o-a", Value::Null, "closed"),
    );
    push(
        &mut s,
        "pull_requests",
        pull_request("example-org/loom", 3, "o-a", Value::Null, "open"),
    );
    push(
        &mut s,
        "pull_requests",
        pull_request("example-org/loom", 4, "o-a", json!("running"), "open"),
    );
    push(
        &mut s,
        "pull_requests",
        pull_request("example-org/loom", 5, "o-a", json!("failed"), "open"),
    );
    let m = model(&s);
    let order: Vec<&str> = m["one_off"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["task_id"].as_str().unwrap())
        .collect();
    assert_eq!(order, vec!["o-b", "o-c", "o-d", "o-e", "o-a"]);
    for r in m["one_off"].as_array().unwrap() {
        assert_eq!(
            r["title"],
            format!("Title of task {}", r["task_id"].as_str().unwrap())
        );
    }
    let prs: Vec<i64> = m["pull_requests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            r["ref"]
                .as_str()
                .unwrap()
                .rsplit('#')
                .next()
                .unwrap()
                .parse()
                .unwrap()
        })
        .collect();
    assert_eq!(prs, vec![5, 4, 3, 2, 1]);
}

/// Every line to type: each string under a key `line`, and each string value of an object under
/// a key `lines`, at any depth; null ones left out.
fn lines(v: &Value) -> Vec<String> {
    fn walk(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::Array(a) => a.iter().for_each(|e| walk(e, out)),
            Value::Object(o) => {
                for (k, e) in o {
                    match (k.as_str(), e) {
                        ("line", Value::String(s)) => out.push(s.clone()),
                        ("lines", Value::Object(named)) => {
                            for l in named.values() {
                                if let Value::String(s) = l {
                                    out.push(s.clone());
                                }
                            }
                        }
                        _ => walk(e, out),
                    }
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(v, &mut out);
    out
}

struct Shell {
    dir: tempfile::TempDir,
}

impl Shell {
    fn new() -> Shell {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("bin")).unwrap();
        common::exe::write(
            &dir.path().join("bin/makiba"),
            "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\" >> \"$REC\"; done\n",
        );
        Shell { dir }
    }

    fn pwned(&self) -> PathBuf {
        self.dir.path().join("pwned")
    }

    /// The arguments the recording `makiba` got when `line` ran under `sh -c`.
    fn run(&self, line: &str) -> Vec<String> {
        let rec = self.dir.path().join("rec");
        let _ = std::fs::remove_file(&rec);
        let path = format!(
            "{}:{}",
            self.dir.path().join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let out = Command::new("sh")
            .arg("-c")
            .arg(line)
            .env("PATH", path)
            .env("REC", &rec)
            .current_dir(self.dir.path())
            .output()
            .unwrap();
        assert!(out.status.success(), "{line}: {out:?}");
        std::fs::read_to_string(&rec)
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
    }
}

#[test]
fn t13_lines_to_type_carry_no_record_text() {
    let sh = Shell::new();
    let evil = format!("Evil \" ' ` $(touch {})", sh.pwned().display());
    let mut s = status();
    s["makibad"]["running"] = json!(false);
    for (slug, repo) in [
        ("loom", "example-org/loom"),
        ("harbor", "example-org/harbor"),
        ("harbor-site", "example-org/harbor"),
        ("bad", "bad repo;x/y"),
        ("-x", "example-org/dash"),
    ] {
        push(&mut s, "projects", project(slug, Some(repo)));
    }
    let mut r4 = range(
        4,
        "loom",
        "proposed",
        vec![item(80, "not_in", None, None, Some(&evil))],
    );
    r4["items"][0]["title"] = json!(evil.clone());
    r4["brief_head"] = json!(evil.clone());
    r4["takes"] = json!(evil.clone());
    r4["read_id"] = json!(77);
    r4["approves_under"] = json!("human");
    r4["candidates"] = json!([{"issue": 80, "title": evil.clone(), "admitted": false,
                               "left_out_why": evil.clone(), "design_only_waits": null}]);
    push(&mut s, "ranges", r4);
    for t in ["r3-56", "a b", "--help"] {
        let mut r = run(t, "loom", "failed", "failed", None);
        r["title"] = json!(evil.clone());
        r["failed_reason"] = json!(evil.clone());
        r["issue"] = json!(56);
        push(&mut s, "runs", r);
    }
    let mut p = pending(21, Some("r3-56"), "escalated", "question", &evil);
    p["context"] = json!({"background": evil.clone(), "if_yes": evil.clone(), "if_no": evil.clone(),
                          "recommend": "yes", "why": evil.clone()});
    push(&mut s, "pending", p);
    let mut nw = next_work("candidates_found", 4);
    let mut cands = Vec::new();
    for (repo, issue) in [
        ("example-org/loom", 56),
        ("example-org/harbor", 31),
        ("bad repo;x/y", 9),
        ("example-org/dash", 5),
    ] {
        let mut c = candidate(repo, issue);
        c["title"] = json!(evil.clone());
        c["why"] = json!(evil.clone());
        cands.push(c);
    }
    nw["candidates"] = json!(cands);
    let mut lo = left_out(Some("loom"), Some("example-org/loom"), Some(90), "blocked");
    lo["reason"] = json!(evil.clone());
    lo["title"] = json!(evil.clone());
    nw["left_out"] = json!([lo]);
    s["next_work"] = nw;

    let m = model(&s);
    let all = lines(&m);
    assert!(!all.is_empty());
    for l in &all {
        for bad in [
            "Evil", "\"", "`", "$(", "pwned", "--yes", "dispatch", "relaunch",
        ] {
            assert!(!l.contains(bad), "{l:?} holds {bad:?}");
        }
        sh.run(l);
        assert!(!sh.pwned().exists(), "{l} made a file");
    }

    let entry = &m["answers"]["entries"][0];
    assert_eq!(
        sh.run(entry["lines"]["yes"].as_str().unwrap()),
        ["answer", "21", "yes"]
    );
    let approve = screen::confirmed(&find(&m["ranges"], "id", &json!(4))["approve"], "4");
    assert_eq!(
        sh.run(approve.as_str().unwrap()),
        ["range", "approve", "4", "--read", "77", "--mode", "human"]
    );
    let loom = find(&m["ready"]["rows"], "ref", &json!("loom#56"));
    assert_eq!(
        sh.run(loom["lines"][0]["line"].as_str().unwrap()),
        ["send", "secretary", "Please take up example-org/loom#56."]
    );
    let harbor = find(&m["ready"]["rows"], "ref", &json!("harbor#31"));
    let site = find(&harbor["lines"], "project", &json!("harbor-site"));
    assert_eq!(
        sh.run(site["line"].as_str().unwrap()),
        [
            "send",
            "secretary",
            "Please take up example-org/harbor#31 for the project harbor-site."
        ]
    );
    let bad = find(&m["ready"]["rows"], "ref", &json!("bad#9"));
    for l in bad["lines"].as_array().unwrap() {
        assert_eq!(l["line"], Value::Null, "{bad}");
    }
    let dash = find(&m["ready"]["rows"], "ref", &json!("-x#5"));
    for l in dash["lines"].as_array().unwrap() {
        assert_eq!(l["line"], Value::Null, "{dash}");
    }

    let rows = &m["failures"]["rows"];
    let control = |task: &str, key: &str| -> Value {
        find(
            &find(rows, "task_id", &json!(task))["controls"],
            "key",
            &json!(key),
        )
        .clone()
    };
    assert_eq!(
        sh.run(control("r3-56", "act.ask_look")["line"].as_str().unwrap()),
        [
            "send",
            "secretary",
            "Please look at launch_failed on task r3-56."
        ]
    );
    let abandon = control("r3-56", "act.abandon");
    assert!(abandon["confirm"].is_object());
    assert_eq!(abandon["line"], Value::Null);
    assert_eq!(
        sh.run(screen::confirmed(&abandon, "r3-56").as_str().unwrap()),
        ["close", "r3-56", "--abandon"]
    );
    for t in ["a b", "--help"] {
        assert_eq!(
            control(t, "act.ask_look")["line"],
            "makiba send secretary 'Please look at launch_failed.'",
            "{t}"
        );
        assert_eq!(control(t, "act.abandon")["line"], Value::Null, "{t}");
        assert_eq!(control(t, "act.abandon")["confirm"], Value::Null, "{t}");
        assert_eq!(
            screen::confirmed(&control(t, "act.abandon"), t),
            Value::Null,
            "{t}"
        );
    }
    let silent = find(rows, "kind", &json!("makibad_silent"));
    let ups: Vec<&Value> = silent["controls"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| &c["line"])
        .collect();
    assert_eq!(ups, vec![&json!("makiba up")]);
}

#[test]
fn t14_views_are_counted_and_marked_from_their_rows() {
    let m = model(&six());
    let v = &m["views"];
    assert_eq!(v["failures"]["mark"], "failed");
    assert_eq!(
        v["failures"]["count"],
        m["failures"]["rows"].as_array().unwrap().len()
    );
    assert_eq!(v["issues"]["mark"], Value::Null);
    assert_eq!(
        v["issues"]["count"],
        m["ready"]["rows"].as_array().unwrap().len()
    );
    assert_eq!(
        v["one_off"]["count"],
        m["one_off"].as_array().unwrap().len()
    );
    assert_eq!(
        v["pull_requests"]["count"],
        m["pull_requests"].as_array().unwrap().len()
    );

    let mut s = status();
    push(
        &mut s,
        "projects",
        project("loom", Some("example-org/loom")),
    );
    let mut d = run("o-d", "loom", "in_use", "verified", None);
    d["waiting"] = json!({"for": "person", "pending": [50], "why": "Which one?"});
    push(&mut s, "runs", d);
    push(
        &mut s,
        "pending",
        pending(50, Some("o-d"), "escalated", "question", "Which one?"),
    );
    let m = model(&s);
    assert_eq!(m["views"]["one_off"]["mark"], "needs_you");
    assert_eq!(m["views"]["one_off"]["count"], 1);
    assert_eq!(m["views"]["failures"]["count"], 0);
    assert_eq!(m["views"]["failures"]["mark"], Value::Null);
}

#[test]
fn t15_history_names_each_event_by_its_kind_and_payload() {
    let ev = |id: i64, kind: &str, payload: Value| {
        json!({"id": id, "occurred_at": at(100 - id), "actor": "makibad", "kind": kind,
               "task_id": "r3-56", "payload": payload})
    };
    let events = vec![
        ev(
            1,
            "belt_dispatched",
            json!({"range_id": 3, "issue": 56, "task_id": "r3-56"}),
        ),
        ev(2, "worker_spawn_verified", json!({})),
        ev(
            3,
            "worker_reported",
            json!({"task": "r3-56", "state": "start", "review": null}),
        ),
        ev(
            4,
            "worker_reported",
            json!({"task": "r3-56", "state": "done",
                                        "review": {"state": "passed", "reader": "codex"}}),
        ),
        ev(
            5,
            "worker_reported",
            json!({"task": "r3-56", "state": "done",
                                        "review": {"state": "stale", "reader": "codex",
                                                   "findings": {"blocker": 0, "major": 2, "minor": 1}}}),
        ),
        ev(6, "ci_completed", json!({"status": "passed"})),
        ev(7, "ci_completed", json!({"status": "failed"})),
        ev(8, "pr_merged", json!({"repo": "example-org/loom", "pr": 9})),
        ev(
            9,
            "run_closed",
            json!({"task_id": "r3-56", "status": "completed"}),
        ),
        ev(
            10,
            "run_closed",
            json!({"task_id": "r3-56", "status": "abandoned"}),
        ),
        ev(11, "zz_new", json!({})),
    ];
    let h = screen::history(&events, &en());
    let rows = h.as_array().unwrap();
    let ids: Vec<i64> = rows.iter().map(|r| r["id"].as_i64().unwrap()).collect();
    assert_eq!(ids, (1..=11).collect::<Vec<_>>());
    let words: Vec<String> = rows
        .iter()
        .map(|r| r["words"].as_str().unwrap().to_string())
        .collect();
    let w = en();
    assert_eq!(
        words,
        vec![
            w.get("hist.started.makiba"),
            w.get("hist.began"),
            w.get("hist.plan"),
            w.get("hist.done"),
            w.get("hist.done"),
            w.get("hist.checks.passed"),
            w.get("hist.checks.failed"),
            w.get("hist.merged"),
            w.get("hist.closed.completed"),
            w.get("hist.closed.abandoned"),
            "zz_new".to_string()
        ]
    );
    assert_eq!(rows[3]["detail"], w.get("hist.review.passed"));
    assert_eq!(rows[4]["detail"], w.fill("hist.review.found", &[n(3)]));
}

/// Issue 570 (P1a of docs/design/screen-acts.md): the two lines that need a typed confirmation,
/// approve and abandon, are not in the model; `screen::confirmed` gives each only when the typed
/// text equals the id shown, approve with `--read` and `--mode` and never `--yes`, and no line
/// carries `--via` or record text.
#[test]
fn t16_confirmed_lines_wait_for_the_typed_id() {
    let sh = Shell::new();
    let evil = format!("Evil \" ' ` $(touch {})", sh.pwned().display());
    let mut s = status();
    push(
        &mut s,
        "projects",
        project("loom", Some("example-org/loom")),
    );
    let mut r4 = range(
        4,
        "loom",
        "proposed",
        vec![item(80, "not_in", None, None, Some(&evil))],
    );
    r4["items"][0]["title"] = json!(evil.clone());
    r4["brief_head"] = json!(evil.clone());
    r4["takes"] = json!(evil.clone());
    r4["read_id"] = json!(77);
    r4["approves_under"] = json!("human");
    r4["candidates"] = json!([{"issue": 80, "title": evil.clone(), "admitted": false,
                               "left_out_why": evil.clone(), "design_only_waits": null}]);
    push(&mut s, "ranges", r4);
    for (id, read, mode, status) in [
        (5, json!(78), json!("acceptance"), "proposed"),
        (6, json!(79), Value::Null, "proposed"),
        (7, Value::Null, json!("human"), "proposed"),
        (8, json!(80), Value::Null, "active"),
    ] {
        let mut r = range(
            id,
            "loom",
            status,
            vec![item(90 + id, "not_in", None, None, None)],
        );
        r["read_id"] = read;
        r["approves_under"] = mode;
        push(&mut s, "ranges", r);
    }
    for t in ["r3-56", "a b"] {
        let mut r = run(t, "loom", "failed", "failed", None);
        r["title"] = json!(evil.clone());
        r["failed_reason"] = json!(evil.clone());
        push(&mut s, "runs", r);
    }
    let m = model(&s);
    let w = en();

    // 1. Neither line is in the model.
    for l in lines(&m) {
        assert!(!l.contains("range approve"), "{l:?}");
        assert!(!l.contains("--abandon"), "{l:?}");
    }
    let r4 = find(&m["ranges"], "id", &json!(4));
    assert!(r4["lines"].get("approve").is_none(), "{}", r4["lines"]);
    let approve = &r4["approve"];
    assert_eq!(approve["line"], Value::Null);
    let rows = &m["failures"]["rows"];
    let abandon = |task: &str| -> Value {
        find(
            &find(rows, "task_id", &json!(task))["controls"],
            "key",
            &json!("act.abandon"),
        )
        .clone()
    };
    let ab = abandon("r3-56");
    assert_eq!(ab["line"], Value::Null);

    let mut given = Vec::new();
    // 2. Approve: only the exact range id gives its line.
    for t in ["", "3", "4 ", " 4", "04", "44", "77", "r3-56", "4\n"] {
        assert_eq!(screen::confirmed(approve, t), Value::Null, "{t:?}");
    }
    let l4 = screen::confirmed(approve, "4");
    let l4 = l4.as_str().expect("the approve line once 4 is typed");
    assert_eq!(
        sh.run(l4),
        ["range", "approve", "4", "--read", "77", "--mode", "human"]
    );
    given.push(l4.to_string());
    // 5. What the person is asked to type.
    assert_eq!(
        approve["confirm"]["type_words"],
        w.fill("act.type_id", &[("id", "4".to_string())])
    );

    // 3. The mode the screen showed, and no line without a read or a known mode.
    let r5 = &find(&m["ranges"], "id", &json!(5))["approve"];
    assert_eq!(screen::confirmed(r5, "4"), Value::Null);
    let l5 = screen::confirmed(r5, "5");
    let l5 = l5.as_str().expect("the approve line of range 5");
    assert_eq!(
        sh.run(l5),
        [
            "range",
            "approve",
            "5",
            "--read",
            "78",
            "--mode",
            "acceptance"
        ]
    );
    given.push(l5.to_string());
    for id in [6, 7, 8] {
        assert_eq!(
            find(&m["ranges"], "id", &json!(id))["approve"],
            Value::Null,
            "range {id}"
        );
    }

    // 4. Abandon: under its sentence, only the exact task id gives its line.
    assert_eq!(ab["confirm"]["words"], w.get("act.abandon_confirm"));
    assert_eq!(
        ab["confirm"]["type_words"],
        w.fill("act.type_id", &[("id", "r3-56".to_string())])
    );
    for t in ["", "r3-5", "R3-56", "r3-56 ", "4"] {
        assert_eq!(screen::confirmed(&ab, t), Value::Null, "{t:?}");
    }
    assert_eq!(screen::confirmed(approve, "r3-56"), Value::Null);
    let la = screen::confirmed(&ab, "r3-56");
    let la = la.as_str().expect("the abandon line once r3-56 is typed");
    assert_eq!(sh.run(la), ["close", "r3-56", "--abandon"]);
    given.push(la.to_string());
    let bad = abandon("a b");
    assert_eq!(bad["confirm"], Value::Null);
    assert_eq!(screen::confirmed(&bad, "a b"), Value::Null);

    // 6. A control the model did not give yields nothing, even typed to match.
    let mut t = approve.clone();
    t["confirm"]["id"] = json!(evil.clone());
    assert_eq!(screen::confirmed(&t, &evil), Value::Null);
    let mut t = approve.clone();
    t["confirm"]["mode"] = json!("human; touch x");
    assert_eq!(screen::confirmed(&t, "4"), Value::Null);
    let mut t = approve.clone();
    t["confirm"]["read_id"] = json!("77");
    assert_eq!(screen::confirmed(&t, "4"), Value::Null);
    let mut t = approve.clone();
    t["confirm"]["act"] = json!("dispatch");
    assert_eq!(screen::confirmed(&t, "4"), Value::Null);
    let mut t = ab.clone();
    t["confirm"]["id"] = json!("--help");
    assert_eq!(screen::confirmed(&t, "--help"), Value::Null);

    // 7. No line given carries --yes, --via or record text, and none runs anything else.
    given.extend(lines(&m));
    assert!(given.len() > 3);
    for l in &given {
        for bad in ["--yes", "--via", "Evil", "\"", "`", "$(", "pwned"] {
            assert!(!l.contains(bad), "{l:?} holds {bad:?}");
        }
        sh.run(l);
        assert!(!sh.pwned().exists(), "{l} made a file");
    }
}
