//! The screen's model (Issue 169, step 1 of docs/design/screen.md section 7): the read surface in
//! the person's words. A pure function from what `makiba status --json` prints, and from one
//! task's events as `makiba events --json --task` prints them, to what each region of the screen
//! shows. It reads no table and no file, holds nothing between calls and reads no clock (the
//! caller passes `now`); `status_local` stays the one reader of `state.db`, and this is a client
//! of its output, as the screen is (Issue 169: "it opens no table").
//!
//! Every word comes from the wording files `roles/locale/screen/<lang>.tsv` (screen.md 2.2, 2.4).
//! Every control is a line to type (screen.md section 4), built only from a verb, numbers, a
//! repository and a task id that pass their checks and the screen's own fixed sentences; never
//! from a title, a question, a reason or any other text of a record. Where a repository is
//! registered by several projects, the rules of screen.md section 2, "After Issue 415", hold.
//!
//! The two acts that need a typed confirmation, "Approve exactly this" and "Abandon and close…"
//! (docs/design/screen-acts.md section 5, Issue 570), have no line in the model: their control
//! carries the id to type, and `confirmed` gives the line only when the typed text equals it.

use crate::daemon::prwatch::parse_time;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

const EN: &str = include_str!("../roles/locale/screen/en.tsv");
const JA: &str = include_str!("../roles/locale/screen/ja.tsv");

/// The places of a range's items, in the order the band and the counts show them (screen.md 3.3).
pub const PLACE_ORDER: [&str; 7] = [
    "landed",
    "closed",
    "waiting_person",
    "review",
    "in_work",
    "not_in",
    "skipped",
];

/// The anomaly rules the screen has words for, and so the kinds a fixed sentence may name.
const RULES: [(&str, &str); 10] = [
    ("no_status", "fail.silent"),
    ("gone", "fail.gone"),
    ("error", "fail.error"),
    ("blocked_question", "fail.stuck_question"),
    ("blocked_auth", "fail.stuck_login"),
    ("blocked_permission", "fail.stuck_permission"),
    ("done_no_report", "fail.no_report"),
    ("secretary_gone", "fail.secretary_gone"),
    ("secretary_still", "fail.secretary_still"),
    ("binary_replaced", "fail.replaced"),
];

fn parse(text: &'static str) -> Vec<(&'static str, &'static str)> {
    text.lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once('\t'))
        .collect()
}

/// The wording of one display language over English.
pub struct Words {
    chosen: Vec<(&'static str, &'static str)>,
    en: Vec<(&'static str, &'static str)>,
}

impl Words {
    /// `ja`: the Japanese table over English; any other code: English.
    pub fn new(lang: &str) -> Words {
        let chosen = if lang == "ja" { parse(JA) } else { Vec::new() };
        Words {
            chosen,
            en: parse(EN),
        }
    }

    /// The keys of the table of `lang` (English for any code but `ja`).
    pub fn keys(lang: &str) -> Vec<&'static str> {
        let t = if lang == "ja" { parse(JA) } else { parse(EN) };
        t.into_iter().map(|(k, _)| k).collect()
    }

    /// The wording of `key`: the chosen language's, else English, else the key itself.
    pub fn get(&self, key: &str) -> String {
        self.chosen
            .iter()
            .chain(self.en.iter())
            .find(|(k, _)| *k == key)
            .map_or_else(|| key.to_string(), |(_, t)| t.to_string())
    }

    /// `get(key)` with each `{name}` replaced by its value.
    pub fn fill(&self, key: &str, vals: &[(&str, String)]) -> String {
        let mut s = self.get(key);
        for (name, v) in vals {
            s = s.replace(&format!("{{{name}}}"), v);
        }
        s
    }
}

fn n(v: impl ToString) -> (&'static str, String) {
    ("n", v.to_string())
}

fn str_of(v: &Value) -> Option<&str> {
    v.as_str()
}

fn arr(v: &Value) -> &[Value] {
    v.as_array().map_or(&[], |a| a.as_slice())
}

/// The first character a letter or digit, the rest letters, digits, `.`, `_` or `-`.
fn word_ok(s: &str, more: &[char]) -> bool {
    let mut c = s.chars();
    matches!(c.next(), Some(f) if f.is_ascii_alphanumeric())
        && c.all(|ch| ch.is_ascii_alphanumeric() || more.contains(&ch))
}

fn task_ok(t: &str) -> bool {
    word_ok(t, &['.', '_', '-'])
}

fn slug_ok(s: &str) -> bool {
    word_ok(s, &['.', '_', '-'])
}

fn repo_ok(r: &str) -> bool {
    match r.split_once('/') {
        Some((o, name)) => word_ok(o, &['.', '_', '-']) && word_ok(name, &['.', '_', '-']),
        None => false,
    }
}

/// One shell word: single-quoted, `'` written as `'\''`.
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn send_line(sentence: &str) -> String {
    format!("makiba send secretary {}", quote(sentence))
}

fn ask_to_start(repo: &str, issue: i64, slug: Option<&str>) -> Value {
    if !repo_ok(repo) {
        return Value::Null;
    }
    match slug {
        None => json!(send_line(&format!("Please take up {repo}#{issue}."))),
        Some(s) if slug_ok(s) => json!(send_line(&format!(
            "Please take up {repo}#{issue} for the project {s}."
        ))),
        Some(_) => Value::Null,
    }
}

fn rule_key(rule: &str) -> Option<&'static str> {
    RULES.iter().find(|(r, _)| *r == rule).map(|(_, k)| *k)
}

fn eq_repo(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

struct Ctx<'a> {
    st: &'a Value,
    now: i64,
    w: &'a Words,
    projects: Vec<(String, Option<String>)>,
}

impl<'a> Ctx<'a> {
    fn age_s(&self, at: &Value) -> Value {
        match at.as_str().and_then(parse_time) {
            Some(t) => json!((self.now - t).max(0) / 1000),
            None => Value::Null,
        }
    }

    fn ms(&self, at: &Value) -> Option<i64> {
        at.as_str().and_then(parse_time)
    }

    fn run(&self, task: &str) -> Option<&'a Value> {
        arr(&self.st["runs"]).iter().find(|r| r["task_id"] == task)
    }

    fn range(&self, id: &Value) -> Option<&'a Value> {
        arr(&self.st["ranges"]).iter().find(|r| &r["id"] == id)
    }

    /// The range item naming `task`, with its range.
    fn item(&self, task: &str) -> Option<(&'a Value, &'a Value)> {
        arr(&self.st["ranges"]).iter().find_map(|r| {
            arr(&r["items"])
                .iter()
                .find(|i| i["task_id"] == task)
                .map(|i| (r, i))
        })
    }

    fn anomaly(&self, task: &str) -> Option<&'a Value> {
        arr(&self.st["anomalies"])
            .iter()
            .find(|a| a["task_id"] == task)
    }

    fn escalated(&self, task: &str) -> Option<&'a Value> {
        arr(&self.st["pending"])
            .iter()
            .find(|p| p["task_id"] == task && p["status"] == "escalated")
    }

    fn launch_failed(&self, task: &str) -> bool {
        self.run(task)
            .is_some_and(|r| r["status"] == "failed" && r["stage"] == "failed")
    }

    fn repo_of(&self, slug: &str) -> Option<&str> {
        self.projects
            .iter()
            .find(|(s, _)| s == slug)
            .and_then(|(_, r)| r.as_deref())
    }

    /// Every project of `repo`: those that register it (case aside) and those a left-out entry
    /// of it names, by name.
    fn projects_of_repo(&self, repo: &str) -> Vec<String> {
        let mut out: Vec<String> = self
            .projects
            .iter()
            .filter(|(_, r)| r.as_deref().is_some_and(|r| eq_repo(r, repo)))
            .map(|(s, _)| s.clone())
            .collect();
        for e in arr(&self.st["next_work"]["left_out"]) {
            if str_of(&e["repo"]).is_some_and(|r| eq_repo(r, repo))
                && let Some(p) = str_of(&e["project"])
            {
                out.push(p.to_string());
            }
        }
        out.sort();
        out.dedup();
        out
    }

    /// The project of a task: its run's, else its range's.
    fn project_of_task(&self, task: &str) -> Option<String> {
        if let Some(p) = self.run(task).and_then(|r| str_of(&r["project"])) {
            return Some(p.to_string());
        }
        self.item(task)
            .and_then(|(r, _)| str_of(&r["project"]))
            .map(String::from)
    }

    /// The project of a pending entry: its task's, else its range's (a stop of a whole range).
    fn project_of_pending(&self, p: &Value) -> Option<String> {
        if let Some(t) = str_of(&p["task_id"]) {
            return self.project_of_task(t);
        }
        self.range(&p["range_id"])
            .and_then(|r| str_of(&r["project"]))
            .map(String::from)
    }

    fn title_of_task(&self, task: &str) -> Value {
        if let Some((_, i)) = self.item(task) {
            return i["title"].clone();
        }
        self.run(task).map_or(Value::Null, |r| r["title"].clone())
    }

    fn issue_of_task(&self, task: &str) -> Value {
        if let Some(i) = self.run(task).map(|r| &r["issue"]).filter(|i| i.is_i64()) {
            return i.clone();
        }
        self.item(task)
            .map_or(Value::Null, |(_, i)| i["issue"].clone())
    }

    fn reference(&self, project: &Option<String>, issue: &Value) -> Value {
        match (project, issue.as_i64()) {
            (Some(p), Some(i)) => json!(format!("{p}#{i}")),
            _ => Value::Null,
        }
    }

    fn anomaly_words(&self, rule: &str) -> String {
        rule_key(rule).map_or_else(|| rule.to_string(), |k| self.w.get(k))
    }
}

/// The screen's model of one read of `makiba status --json` at `now_ms` (milliseconds since 1970).
pub fn model(status: &Value, now_ms: i64, words: &Words) -> Value {
    let projects = arr(&status["projects"])
        .iter()
        .filter_map(|p| {
            let slug = str_of(&p["slug"])?.to_string();
            Some((slug, str_of(&p["repo"]).map(String::from)))
        })
        .collect();
    let c = Ctx {
        st: status,
        now: now_ms,
        w: words,
        projects,
    };
    let failures = failures(&c);
    let (ranges, others, others_more) = ranges(&c);
    let ready = ready(&c);
    let one_off = one_off(&c);
    let pull_requests = pull_requests(&c);
    let answers = answers(&c);
    let views = views(&c, &failures, &ready, &one_off, &pull_requests);
    let top = top(&c, &failures);
    json!({
        "top": top,
        "answers": answers,
        "failures": failures,
        "ranges": ranges,
        "others": others,
        "others_more": others_more,
        "ready": ready,
        "one_off": one_off,
        "pull_requests": pull_requests,
        "views": views,
        "projects": project_rows(&c),
    })
}

fn project_rows(c: &Ctx) -> Value {
    let rows: Vec<Value> = c
        .projects
        .iter()
        .map(|(slug, repo)| {
            let others: Vec<String> = repo
                .as_deref()
                .map(|r| {
                    c.projects
                        .iter()
                        .filter(|(s, o)| s != slug && o.as_deref().is_some_and(|o| eq_repo(o, r)))
                        .map(|(s, _)| s.clone())
                        .collect()
                })
                .unwrap_or_default();
            let heading = match (repo, others.is_empty()) {
                (Some(r), false) => json!(c.w.fill(
                    "shared.heading",
                    &[("repo", r.clone()), ("x", others.join(", "))]
                )),
                _ => Value::Null,
            };
            json!({"slug": slug, "repo": repo, "shared_with": others, "heading_words": heading})
        })
        .collect();
    json!(rows)
}

fn top(c: &Ctx, failures: &Value) -> Value {
    let needs: Vec<&Value> = arr(&c.st["pending"])
        .iter()
        .filter(|p| p["status"] == "escalated")
        .collect();
    let mut needs_in: Vec<String> = needs
        .iter()
        .filter_map(|p| c.project_of_pending(p))
        .collect();
    needs_in.sort();
    needs_in.dedup();
    let rows = arr(&failures["rows"]);
    let mut failed_in = Map::new();
    for r in rows {
        let key = str_of(&r["project"]).unwrap_or("organization").to_string();
        let v = failed_in.entry(key).or_insert(json!(0));
        *v = json!(v.as_i64().unwrap_or(0) + 1);
    }
    let unattended = rows.iter().filter(|r| r["unattended"] == true).count();
    let running = c.st["makibad"]["running"] == true;
    let hb = c.age_s(&c.st["makibad"]["heartbeat_at"]);
    let mut w = Map::new();
    w.insert(
        "needs_you".into(),
        json!(c.w.fill("top.needs_you", &[n(needs.len())])),
    );
    w.insert(
        "failed".into(),
        json!(c.w.fill("top.failed", &[n(rows.len())])),
    );
    if unattended > 0 {
        w.insert(
            "unattended".into(),
            json!(c.w.fill("top.unattended", &[n(unattended)])),
        );
    }
    let state = if running {
        c.w.fill("top.running", &[n(hb.as_i64().unwrap_or(0))])
    } else {
        c.w.fill("top.stopped", &[n(hb.as_i64().unwrap_or(0) / 60)])
    };
    w.insert("running".into(), json!(state));
    json!({
        "organization": c.st["organization"],
        "needs_you": needs.len(),
        "needs_you_in": needs_in,
        "failed": rows.len(),
        "failed_in": failed_in,
        "unattended": unattended,
        "running": running,
        "heartbeat_age_s": hb,
        "words": Value::Object(w),
    })
}

fn answers(c: &Ctx) -> Value {
    let mut entries: Vec<&Value> = arr(&c.st["pending"])
        .iter()
        .filter(|p| p["status"] == "escalated")
        .collect();
    entries.sort_by_key(|p| {
        (
            c.ms(&p["created_at"]).unwrap_or(0),
            p["id"].as_i64().unwrap_or(0),
        )
    });
    let rows: Vec<Value> = entries
        .iter()
        .map(|p| {
            let stop = arr(&c.st["ranges"])
                .iter()
                .flat_map(|r| arr(&r["stops"]))
                .find(|s| s["id"] == p["id"]);
            let kind = if stop.is_some() {
                "answer.kind_decision"
            } else if p["kind"] == "prompt" {
                "answer.kind_permission"
            } else {
                "answer.kind_question"
            };
            let title = str_of(&p["task_id"]).map_or(Value::Null, |t| c.title_of_task(t));
            let heading = match title.as_str() {
                Some(t) => format!("{} \u{b7} {t}", c.w.get(kind)),
                None => c.w.get(kind),
            };
            let recommended = stop
                .map(|s| &s["recommend_answer"])
                .filter(|a| *a == "yes" || *a == "no")
                .cloned()
                .unwrap_or(Value::Null);
            let lines = match p["id"].as_i64() {
                Some(id) => json!({"yes": format!("makiba answer {id} yes"),
                                   "no": format!("makiba answer {id} no")}),
                None => json!({"yes": null, "no": null}),
            };
            let ctx = &p["context"];
            json!({
                "id": p["id"], "task_id": p["task_id"], "project": c.project_of_pending(p),
                "heading": heading, "question": p["question"],
                "background": ctx["background"], "if_yes": ctx["if_yes"], "if_no": ctx["if_no"],
                "recommend": ctx["recommend"], "why": ctx["why"],
                "recommended_answer": recommended, "age_s": c.age_s(&p["created_at"]),
                "range_id": p["range_id"], "stop_line": p["stop_line"],
                "stop_kind": stop.map_or(Value::Null, |s| s["kind"].clone()),
                "lines": lines,
            })
        })
        .collect();
    let open = arr(&c.st["pending"])
        .iter()
        .filter(|p| p["status"] == "open")
        .count();
    json!({"entries": rows, "with_secretary": open})
}

fn control(c: &Ctx, key: &str, line: Value) -> Value {
    json!({"key": key, "words": c.w.get(key), "line": line, "confirm": null})
}

/// A control whose line waits for the person to type `id` (screen-acts.md section 5): no line,
/// and in `confirm` what `confirmed` builds it from and what the person is asked to type.
fn typed_control(c: &Ctx, key: &str, mut confirm: Value) -> Value {
    let id = confirm["id"].as_str().unwrap_or_default().to_string();
    confirm["type_words"] = json!(c.w.fill("act.type_id", &[("id", id)]));
    let mut ctl = control(c, key, Value::Null);
    ctl["confirm"] = confirm;
    ctl
}

/// The line of a control behind a typed confirmation (Issue 570), given only when `typed` equals
/// the id the control shows, exactly: `makiba range approve <id> --read <read> --mode <mode>`
/// (without `--yes`: the command prints the draft and asks) or `makiba close <task> --abandon`.
/// It checks the syntax of what it builds from and nothing more: it cannot know that the control
/// came from the model, so the caller passes the control it showed. Otherwise null.
pub fn confirmed(control: &Value, typed: &str) -> Value {
    let k = &control["confirm"];
    let Some(id) = k["id"].as_str() else {
        return Value::Null;
    };
    if typed != id {
        return Value::Null;
    }
    match str_of(&k["act"]) {
        Some("approve") => {
            let digits = !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit());
            let range = if digits { id.parse::<i64>().ok() } else { None };
            match (range, k["read_id"].as_i64(), str_of(&k["mode"])) {
                (Some(id), Some(read), Some(m @ ("human" | "acceptance"))) => {
                    json!(format!(
                        "makiba range approve {id} --read {read} --mode {m}"
                    ))
                }
                _ => Value::Null,
            }
        }
        Some("abandon") if task_ok(id) => json!(format!("makiba close {id} --abandon")),
        _ => Value::Null,
    }
}

fn failures(c: &Ctx) -> Value {
    let st = c.st;
    let secretary_down = arr(&st["anomalies"])
        .iter()
        .any(|a| a["rule"] == "secretary_gone" || a["rule"] == "secretary_still");
    // (kind, task, since, notice, reason, words)
    let mut raw: Vec<(String, Option<String>, Value, Value, Value, String)> = Vec::new();
    if st["makibad"]["running"] != true {
        let mins = c
            .age_s(&st["makibad"]["heartbeat_at"])
            .as_i64()
            .unwrap_or(0)
            / 60;
        raw.push((
            "makibad_silent".into(),
            None,
            st["makibad"]["heartbeat_at"].clone(),
            Value::Null,
            Value::Null,
            c.w.fill("top.stopped", &[n(mins)]),
        ));
    }
    for a in arr(&st["anomalies"]) {
        let rule = str_of(&a["rule"]).unwrap_or("").to_string();
        raw.push((
            rule.clone(),
            str_of(&a["task_id"]).map(String::from),
            a["since"].clone(),
            a["notice"].clone(),
            Value::Null,
            c.anomaly_words(&rule),
        ));
    }
    for r in arr(&st["runs"]) {
        if r["status"] == "failed" && r["stage"] == "failed" {
            let since = if r["notice"]["written_at"].is_string() {
                r["notice"]["written_at"].clone()
            } else {
                r["since"].clone()
            };
            raw.push((
                "launch_failed".into(),
                str_of(&r["task_id"]).map(String::from),
                since,
                r["notice"].clone(),
                r["failed_reason"].clone(),
                c.w.get("fail.did_not_start"),
            ));
        }
    }
    if st["next_work"]["status"] == "error" {
        raw.push((
            "scan_failed".into(),
            None,
            st["next_work"]["scanned_at"].clone(),
            Value::Null,
            st["next_work"]["error"].clone(),
            c.w.get("ready.check_failed"),
        ));
    }
    let mut rows: Vec<(i64, i64, Value)> = raw
        .into_iter()
        .map(|(kind, task, since, notice, reason, words)| {
            let project = task.as_deref().and_then(|t| c.project_of_task(t));
            let since_ms = c.ms(&since).unwrap_or(0);
            let pending = task.as_deref().and_then(|t| c.escalated(t));
            let acted = task
                .as_deref()
                .and_then(|t| c.run(t))
                .and_then(|r| c.ms(&r["secretary_acted_at"]));
            let (who, who_words): (&str, Vec<String>) = if pending.is_some() {
                ("you", vec![c.w.get("who.you")])
            } else if kind == "makibad_silent" || secretary_down {
                ("nobody", vec![c.w.get("who.nobody")])
            } else if acted.is_some_and(|a| a > since_ms) {
                (
                    "looking",
                    vec![c.w.get("who.secretary"), c.w.get("who.looking")],
                )
            } else if notice["state"] == "delivered" {
                let mins = c.age_s(&notice["delivered_at"]).as_i64().unwrap_or(0) / 60;
                (
                    "told",
                    vec![c.w.get("who.secretary"), c.w.fill("who.told", &[n(mins)])],
                )
            } else {
                ("no_sign", vec![c.w.get("who.no_sign")])
            };
            let unattended = who == "nobody" || who == "no_sign";
            let mut controls = Vec::new();
            if kind == "makibad_silent" {
                let mut up = control(c, "act.in_shell", json!("makiba up"));
                up["key"] = json!("act.up");
                controls.push(up);
            } else {
                if let Some(p) = pending {
                    let mut show = control(c, "act.show_question", Value::Null);
                    show["pending_id"] = p["id"].clone();
                    controls.push(show);
                }
                let fixed =
                    kind == "launch_failed" || kind == "scan_failed" || rule_key(&kind).is_some();
                let look = if !fixed {
                    Value::Null
                } else {
                    match task.as_deref().filter(|t| task_ok(t)) {
                        Some(t) => json!(send_line(&format!("Please look at {kind} on task {t}."))),
                        None => json!(send_line(&format!("Please look at {kind}."))),
                    }
                };
                controls.push(control(c, "act.ask_look", look));
                if task.is_some() {
                    controls.push(control(c, "act.history", Value::Null));
                }
                if kind == "launch_failed" {
                    let ab = match task.as_deref().filter(|t| task_ok(t)) {
                        Some(t) => typed_control(
                            c,
                            "act.abandon",
                            json!({"act": "abandon", "id": t,
                                   "words": c.w.get("act.abandon_confirm")}),
                        ),
                        None => control(c, "act.abandon", Value::Null),
                    };
                    controls.push(ab);
                }
            }
            let (title, reference) = match task.as_deref() {
                Some(t) => (
                    c.title_of_task(t),
                    c.reference(&project, &c.issue_of_task(t)),
                ),
                None => (Value::Null, Value::Null),
            };
            let group = if project.is_none() {
                if kind == "makibad_silent" { 0 } else { 1 }
            } else if unattended {
                2
            } else {
                3
            };
            let row = json!({
                "kind": kind, "project": project, "ref": reference, "task_id": task,
                "title": title, "words": words, "reason": reason, "who": who,
                "who_words": who_words, "unattended": unattended, "since": since,
                "age_s": c.age_s(&since), "controls": controls,
            });
            (group, -since_ms, row)
        })
        .collect();
    rows.sort_by_key(|(g, s, _)| (*g, *s));
    let rows: Vec<Value> = rows.into_iter().map(|(_, _, r)| r).collect();
    let shown: Vec<Value> = rows.iter().take(3).cloned().collect();
    let rest = &rows[shown.len()..];
    json!({
        "rows": rows,
        "shown": shown,
        "more": rest.len(),
        "more_unattended": rest.iter().filter(|r| r["unattended"] == true).count(),
    })
}

fn place_words(c: &Ctx, place: &str) -> String {
    let key = match place {
        "landed" => "place.merged",
        "closed" => "place.closed",
        "waiting_person" => "place.needs_you",
        "review" => "place.in_review",
        "in_work" => "place.working.long",
        "not_in" => "place.not_started",
        "skipped" => "place.skipped",
        other => return other.to_string(),
    };
    c.w.get(key)
}

/// The words of a stage or a `why` the screen knows; else the record's text as written.
/// `repo` is the repository of the item's project, when recorded.
fn detail(c: &Ctx, item: &Value, repo: Option<&str>) -> Value {
    let task = str_of(&item["task_id"]);
    // The item's pull request: the row of its task, else the row of its own number in its
    // project's repository. pull_requests[] lists one row per pull request, so a second run on
    // a shared open pull request (no `items[].pr` yet) finds none: the read surface lacks the
    // link (screen.md section 11).
    let prs = arr(&c.st["pull_requests"]);
    let by_task = task.and_then(|t| prs.iter().find(|p| p["task_id"] == t));
    let by_number = || {
        let (Some(r), Some(nr)) = (repo, item["pr"].as_i64()) else {
            return None;
        };
        prs.iter()
            .find(|p| p["pr"] == nr && str_of(&p["repo"]).is_some_and(|pr| eq_repo(pr, r)))
    };
    let pr_ci = by_task.or_else(by_number).map(|p| p["ci"].clone());
    // `<prefix> #12 #34`: every number, written "12, #34" into the key's `#{n}`.
    let waits = |s: &str, prefix: &str, key: &str| -> Option<String> {
        let rest = s.strip_prefix(prefix)?;
        let nums: Option<Vec<i64>> = rest
            .split_whitespace()
            .map(|t| t.strip_prefix('#').and_then(|d| d.parse::<i64>().ok()))
            .collect();
        let nums = nums.filter(|v| !v.is_empty())?;
        let joined = nums
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(", #");
        Some(c.w.fill(key, &[n(joined)]))
    };
    if let Some(stage) = str_of(&item["stage"]) {
        let words = match stage {
            "ci" => match pr_ci.as_ref().and_then(|v| v.as_str()) {
                Some("running") => Some(c.w.get("stage.checks_running")),
                Some("failed") => Some(c.w.get("stage.checks_failed")),
                _ => None,
            },
            "to_land" => Some(c.w.get("stage.ready_to_merge")),
            "no_pr" => Some(c.w.get("stage.no_pr")),
            "launching" => Some(c.w.get("stage.starting")),
            "design_only" => Some(c.w.get("stage.design_only")),
            s => waits(s, "waits_landing", "stage.waits_landing"),
        };
        if let Some(w) = words {
            return json!(w);
        }
        // A stage the screen has no words for, with no `why`, is shown by its record name.
        if item["why"].is_null() && !matches!(stage, "ci" | "at_work" | "launch_failed" | "parked")
        {
            return json!(stage);
        }
    }
    match str_of(&item["why"]) {
        Some("no_slot") => json!(c.w.get("stage.free_place")),
        Some("budget") => json!(c.w.get("stage.budget")),
        Some(w) => waits(w, "waits_for", "stage.waits_for").map_or_else(|| json!(w), |s| json!(s)),
        None => Value::Null,
    }
}

fn ranges(c: &Ctx) -> (Value, Value, Value) {
    let mut out = Vec::new();
    let mut flagged = Vec::new();
    for r in arr(&c.st["ranges"]) {
        let project = str_of(&r["project"]).map(String::from);
        let mut items = Vec::new();
        let mut dns = 0;
        // A stop of the whole range (a budget spent) waits for the person with no task.
        let mut flag = arr(&c.st["pending"])
            .iter()
            .any(|p| p["status"] == "escalated" && p["range_id"] == r["id"]);
        for i in arr(&r["items"]) {
            let place = str_of(&i["place"]).unwrap_or("");
            let task = str_of(&i["task_id"]);
            let did_not_start = place == "in_work" && task.is_some_and(|t| c.launch_failed(t));
            let anomaly = task.and_then(|t| c.anomaly(t));
            if did_not_start {
                dns += 1;
            }
            if did_not_start
                || place == "waiting_person"
                || (anomaly.is_some() && place != "landed")
            {
                flag = true;
            }
            let words = if did_not_start {
                c.w.get("fail.did_not_start")
            } else if let (Some(a), "in_work" | "review") = (anomaly, place) {
                c.anomaly_words(str_of(&a["rule"]).unwrap_or(""))
            } else {
                place_words(c, place)
            };
            let rank = if did_not_start {
                Some(0)
            } else if anomaly.is_some() && matches!(place, "in_work" | "review" | "waiting_person")
            {
                Some(1)
            } else {
                match place {
                    "waiting_person" => Some(2),
                    "in_work" => Some(3),
                    "review" => Some(4),
                    _ => None,
                }
            };
            // The age of the state shown (screen.md 2.5, Rows): an anomaly's own time when the
            // row shows the anomaly, else the item's.
            let at = match anomaly {
                Some(a) if rank == Some(1) => &a["since"],
                _ => &i["since"],
            };
            items.push((
                rank,
                c.ms(at).unwrap_or(0),
                json!({
                    "issue": i["issue"], "ref": c.reference(&project, &i["issue"]),
                    "title": i["title"], "place": place, "words": words,
                    "detail": detail(c, i, project.as_deref().and_then(|p| c.repo_of(p))), "age_s": c.age_s(at), "task_id": i["task_id"],
                    "did_not_start": did_not_start, "pr": i["pr"], "pr_title": i["pr_title"],
                }),
            ));
        }
        let mut active: Vec<(i64, i64, Value)> = items
            .iter()
            .filter_map(|(rank, since, v)| rank.map(|k| (k, *since, v.clone())))
            .collect();
        active.sort_by_key(|(k, s, _)| (*k, *s));
        let active: Vec<Value> = active.into_iter().map(|(_, _, v)| v).collect();
        let items: Vec<Value> = items.into_iter().map(|(_, _, v)| v).collect();
        let band = if items.len() <= 12 {
            let cells: Vec<Value> = items
                .iter()
                .map(|i| {
                    if i["did_not_start"] == true {
                        json!("did_not_start")
                    } else {
                        i["place"].clone()
                    }
                })
                .collect();
            json!({"cells": cells})
        } else {
            let mut pieces = Vec::new();
            for p in PLACE_ORDER {
                let count = items.iter().filter(|i| i["place"] == p).count();
                if p == "in_work" && dns > 0 {
                    pieces.push(json!({"place": "in_work", "count": count - dns}));
                    pieces.push(json!({"place": "did_not_start", "count": dns}));
                } else {
                    pieces.push(json!({"place": p, "count": count}));
                }
            }
            json!({"pieces": pieces})
        };
        let group = |key: &str, pick: &dyn Fn(&Value) -> bool| -> Value {
            let of: Vec<Value> = items
                .iter()
                .filter(|i| pick(i))
                .map(|i| i["issue"].clone())
                .collect();
            json!({"key": key, "words": c.w.get(key), "count": of.len(), "issues": of})
        };
        let groups = json!({
            "open": [
                group("place.needs_you", &|i| i["place"] == "waiting_person"),
                group("fail.did_not_start", &|i| i["did_not_start"] == true),
                group("place.in_review", &|i| i["place"] == "review"),
                group("place.working.long", &|i| i["place"] == "in_work" && i["did_not_start"] != true),
            ],
            "folded": [
                group("place.merged", &|i| i["place"] == "landed"),
                group("place.closed", &|i| i["place"] == "closed"),
                group("place.not_started", &|i| i["place"] == "not_in"),
                group("place.skipped", &|i| i["place"] == "skipped"),
            ],
        });
        let status = str_of(&r["status"]).unwrap_or("");
        let status_words = match status {
            "active" => c.w.get("range.in_progress"),
            "proposed" => c.w.get("range.proposed"),
            "paused" => c.w.get("range.paused"),
            "ended" => c.w.get("range.ended"),
            other => other.to_string(),
        };
        let budget_words = match (r["used"]["tasks"].as_i64(), r["budget"]["tasks"].as_i64()) {
            (Some(u), Some(b)) => json!(c.w.fill("range.budget", &[n(u), ("m", b.to_string())])),
            _ => Value::Null,
        };
        let id = r["id"].as_i64();
        let line = |ok: bool, verb: &str| -> Value {
            match (ok, id) {
                (true, Some(id)) => json!(format!("makiba range {verb} {id}")),
                _ => Value::Null,
            }
        };
        // Issue 570: behind the typed range id, and only with the read and the mode shown.
        let approve = match (
            status,
            id,
            r["read_id"].as_i64(),
            str_of(&r["approves_under"]),
        ) {
            ("proposed", Some(id), Some(read), Some(m @ ("human" | "acceptance"))) => {
                typed_control(
                    c,
                    "range.approve",
                    json!({"act": "approve", "id": id.to_string(), "read_id": read,
                           "mode": m, "words": null}),
                )
            }
            _ => Value::Null,
        };
        let proposal = if status == "proposed" {
            let (yes, no): (Vec<&Value>, Vec<&Value>) = arr(&r["candidates"])
                .iter()
                .partition(|k| k["admitted"] == true);
            json!({
                "would_start": yes.iter().map(|k| {
                    // Admitted for its design only, until the Issues it waits for land.
                    let waits: Vec<String> = arr(&k["design_only_waits"])
                        .iter()
                        .filter_map(|i| i.as_i64().map(|i| i.to_string()))
                        .collect();
                    let detail = if waits.is_empty() {
                        Value::Null
                    } else {
                        json!(c.w.fill("stage.waits_landing", &[n(waits.join(", #"))]))
                    };
                    json!({"issue": k["issue"], "title": k["title"],
                           "ref": c.reference(&project, &k["issue"]),
                           "design_only_waits": k["design_only_waits"], "detail": detail})
                }).collect::<Vec<_>>(),
                "not_now": no.iter().map(|k| json!({"issue": k["issue"], "title": k["title"],
                    "ref": c.reference(&project, &k["issue"]), "why": k["left_out_why"]}))
                    .collect::<Vec<_>>(),
                "read_id": r["read_id"], "read_at": r["read_at"],
                "read_complete": r["read_complete"], "approves_under": r["approves_under"],
                "replaces": r["replaces"],
            })
        } else {
            Value::Null
        };
        let terms = json!({
            "issues": r["issues"], "takes": r["takes"], "brief_head": r["brief_head"],
            "at_once": r["at_once"], "approval_mode": r["approval_mode"],
            "follow_ups": r["follow_ups"], "design_ahead": r["design_ahead"],
            "approved_by": r["approved_by"], "approved_at": r["approved_at"],
            "ended_reason": r["ended_reason"], "budget": r["budget"], "used": r["used"],
        });
        flagged.push(flag);
        out.push(json!({
            "id": r["id"], "project": project, "status": status,
            "selected": false,
            "heading": c.w.fill("range.title", &[n(id.unwrap_or(0))]),
            "status_words": status_words, "budget_words": budget_words,
            "places": r["places"], "place_order": PLACE_ORDER, "did_not_start": dns,
            "band": band, "items": items, "groups": groups, "active": active,
            "terms": terms, "proposal": proposal,
            "lines": {
                "hold": line(status == "active", "pause"),
                "resume": line(status == "paused", "resume"),
                "end": line(status == "active" || status == "paused", "end"),
            },
            "approve": approve,
        }));
    }
    let sel = (0..out.len())
        .find(|&i| out[i]["status"] == "active" && flagged[i])
        .or_else(|| {
            (0..out.len())
                .filter(|&i| out[i]["status"] == "active")
                .max_by_key(|&i| {
                    c.range(&out[i]["id"])
                        .and_then(|r| c.ms(&r["approved_at"]))
                        .unwrap_or(0)
                })
        })
        .or(if out.is_empty() { None } else { Some(0) });
    let mut others: Vec<usize> = (0..out.len()).filter(|&i| Some(i) != sel).collect();
    others.sort_by_key(|&i| !flagged[i]);
    let more = others.len().saturating_sub(3);
    let others: Vec<Value> = others
        .iter()
        .take(3)
        .map(|&i| out[i]["id"].clone())
        .collect();
    if let Some(i) = sel {
        out[i]["selected"] = json!(true);
    }
    (json!(out), json!(others), json!(more))
}

fn notready_words(c: &Ctx, kind: &str) -> String {
    let key = match kind {
        "blocked" => "notready.waits",
        "put_aside" => "notready.set_aside",
        "no_goal_ledger" => "notready.no_goals",
        "goal_ledger_error" => "notready.goals_error",
        "no_repository" => "notready.no_repo",
        "dependency_unreadable" => "notready.unreadable",
        other => return other.to_string(),
    };
    c.w.get(key)
}

/// Left-out entries merged: equal in repository (case aside), number, kind and reason when they
/// have a repository; an entry with no repository is never merged (screen.md, "After Issue 415").
/// What merges two left-out entries: repository (lower case), number, kind and reason.
type MergeKey = Option<(String, Value, Value, Value)>;

fn merged_left_out(c: &Ctx, entries: &[&Value]) -> Vec<Value> {
    let mut rows: Vec<(MergeKey, Vec<String>, &Value)> = Vec::new();
    for e in entries {
        let key = str_of(&e["repo"]).map(|r| {
            (
                r.to_ascii_lowercase(),
                e["issue"].clone(),
                e["kind"].clone(),
                e["reason"].clone(),
            )
        });
        let project = str_of(&e["project"]).map(String::from);
        match rows.iter_mut().find(|(k, _, _)| key.is_some() && *k == key) {
            Some((_, ps, _)) => ps.extend(project),
            None => rows.push((key, project.into_iter().collect(), e)),
        }
    }
    rows.into_iter()
        .map(|(_, mut ps, e)| {
            if let Some(r) = str_of(&e["repo"]) {
                ps.extend(c.projects_of_repo(r));
            }
            ps.sort();
            ps.dedup();
            let kind = str_of(&e["kind"]).unwrap_or("");
            let reference = match (ps.first(), e["issue"].as_i64(), str_of(&e["repo"])) {
                (Some(p), Some(i), _) => json!(format!("{p}#{i}")),
                (None, Some(i), Some(r)) => json!(format!("{r}#{i}")),
                _ => Value::Null,
            };
            json!({
                "ref": reference, "repo": e["repo"], "issue": e["issue"], "title": e["title"],
                "kind": kind, "words": notready_words(c, kind), "reason": e["reason"],
                "projects": ps,
            })
        })
        .collect()
}

fn ready(c: &Ctx) -> Value {
    let nw = &c.st["next_work"];
    let count = nw["candidate_count"].as_i64().unwrap_or(0);
    let not_listed = nw["candidates_not_recorded"].as_i64().unwrap_or(0);
    let lo_not_listed = nw["left_out_not_recorded"].as_i64().unwrap_or(0);
    let listed_words = |k: i64| -> Value {
        if k > 0 {
            json!(c.w.fill("list.not_recorded", &[n(k)]))
        } else {
            Value::Null
        }
    };
    // Held: an Issue an item of a range not ended names, matched by repository and number.
    let mut held = Vec::new();
    let mut rows = Vec::new();
    for k in arr(&nw["candidates"]) {
        let (Some(repo), Some(issue)) = (str_of(&k["repo"]), k["issue"].as_i64()) else {
            continue;
        };
        let holder = arr(&c.st["ranges"]).iter().find(|r| {
            r["status"] != "ended"
                && str_of(&r["project"])
                    .and_then(|p| c.repo_of(p))
                    .is_some_and(|rr| eq_repo(rr, repo))
                && arr(&r["items"]).iter().any(|i| i["issue"] == issue)
        });
        if let Some(r) = holder {
            let p = str_of(&r["project"]).unwrap_or("");
            held.push(
                json!({"ref": format!("{p}#{issue}"), "range_id": r["id"], "project": p,
                             "issue": issue, "title": k["title"]}),
            );
            continue;
        }
        let ps = c.projects_of_repo(repo);
        let reference = match ps.first() {
            Some(p) => format!("{p}#{issue}"),
            None => format!("{repo}#{issue}"),
        };
        let also = if ps.len() > 1 {
            json!(c.w.fill("shared.also", &[("x", ps[1..].join(", "))]))
        } else {
            Value::Null
        };
        let lines: Vec<Value> = if ps.len() > 1 {
            ps.iter()
                .map(|p| {
                    json!({"project": p, "words": c.w.fill("shared.ask_for", &[("x", p.clone())]),
                                "line": ask_to_start(repo, issue, Some(p))})
                })
                .collect()
        } else {
            let p = ps.first();
            let line = match p {
                Some(s) if !slug_ok(s) => Value::Null,
                _ => ask_to_start(repo, issue, None),
            };
            vec![json!({"project": p, "words": c.w.get("ready.ask"), "line": line})]
        };
        let size_key = if k["size_estimated"] == true {
            "ready.size_estimated"
        } else {
            "ready.size"
        };
        let size_words = str_of(&k["size"]).map_or(Value::Null, |s| {
            json!(c.w.fill(size_key, &[("x", s.to_string())]))
        });
        rows.push(json!({
            "ref": reference, "repo": repo, "issue": issue, "title": k["title"],
            "projects": ps, "also_words": also, "suggested": k["recommended"] == true,
            "no_goal": k["clause_id"].is_null(), "clause_id": k["clause_id"],
            "size_words": size_words, "why": k["why"], "lines": lines,
        }));
    }
    let with_issue: Vec<&Value> = arr(&nw["left_out"])
        .iter()
        .filter(|e| !e["issue"].is_null())
        .collect();
    let without: Vec<&Value> = arr(&nw["left_out"])
        .iter()
        .filter(|e| e["issue"].is_null())
        .collect();
    let state = if nw.is_null() {
        "no_check"
    } else if nw["status"] == "error" {
        "check_failed"
    } else if nw["candidates"].is_null() && count > 0 {
        "not_recorded"
    } else if count == 0 {
        "none"
    } else if rows.is_empty() && !held.is_empty() && not_listed == 0 {
        "all_held"
    } else {
        "listed"
    };
    let words = match state {
        "no_check" => c.w.get("ready.no_check"),
        "check_failed" => c.w.get("ready.check_failed"),
        "not_recorded" => c.w.fill("ready.not_recorded", &[n(count)]),
        "none" => c.w.get("ready.none"),
        "all_held" => c.w.get("ready.all_held"),
        _ => c.w.fill("ready.not_approved", &[n(rows.len())]),
    };
    let truncated: Vec<&str> = str_of(&nw["input_truncated"])
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    json!({
        "state": state, "words": words, "rows": rows, "held": held,
        "not_listed": not_listed, "not_listed_words": listed_words(not_listed),
        "not_ready": merged_left_out(c, &with_issue),
        "left_out_not_listed": lo_not_listed, "left_out_not_listed_words": listed_words(lo_not_listed),
        "unranked": merged_left_out(c, &without),
        "truncated": truncated,
        "scanned_at": nw["scanned_at"],
    })
}

fn one_off(c: &Ctx) -> Value {
    let mut rows: Vec<(i64, i64, Value)> = arr(&c.st["runs"])
        .iter()
        .filter(|r| r["range_id"].is_null())
        .filter_map(|r| {
            let task = str_of(&r["task_id"])?;
            let term = arr(&c.st["terminals"])
                .iter()
                .find(|t| t["name"] == format!("w-{task}"));
            let anomaly = c.anomaly(task);
            let pending = c.escalated(task);
            let term_state = term.and_then(|t| str_of(&t["state"]));
            let (rank, words, at) = if r["status"] == "failed" && r["stage"] == "failed" {
                (
                    0,
                    c.w.get("fail.did_not_start"),
                    r["notice"]["written_at"].clone(),
                )
            } else if let Some(a) = anomaly {
                (
                    1,
                    c.anomaly_words(str_of(&a["rule"]).unwrap_or("")),
                    a["since"].clone(),
                )
            } else if let Some(p) = pending {
                (2, c.w.get("place.needs_you"), p["created_at"].clone())
            } else if !r["waiting"].is_null() {
                (2, c.w.get("place.needs_you"), Value::Null)
            } else if r["status"] == "suspended" {
                (3, c.w.get("term.suspended"), Value::Null)
            } else if let Some(s @ ("blocked" | "idle")) = term_state {
                (
                    3,
                    c.w.get(&format!("term.{s}")),
                    term.map_or(Value::Null, |t| t["since"].clone()),
                )
            } else {
                let words = term_state.map_or_else(
                    || c.w.get("place.working.short"),
                    |s| c.w.get(&format!("term.{s}")),
                );
                (4, words, term.map_or(Value::Null, |t| t["since"].clone()))
            };
            let row = json!({
                "task_id": task, "project": r["project"], "title": r["title"],
                "ref": c.reference(&str_of(&r["project"]).map(String::from), &r["issue"]),
                "words": words, "age_s": c.age_s(&at), "status": r["status"],
            });
            // Within a rank, by the age shown: the time of the state's own record.
            let order = c.ms(&at).or_else(|| c.ms(&r["since"])).unwrap_or(0);
            Some((rank, order, row))
        })
        .collect();
    rows.sort_by_key(|(k, s, _)| (*k, *s));
    json!(rows.into_iter().map(|(_, _, v)| v).collect::<Vec<_>>())
}

fn pull_requests(c: &Ctx) -> Value {
    let mut rows: Vec<(i64, usize, Value)> = arr(&c.st["pull_requests"])
        .iter()
        .enumerate()
        .map(|(idx, p)| {
            let repo = str_of(&p["repo"]).unwrap_or("");
            let task = str_of(&p["task_id"]);
            let projects: Vec<String> = match task.and_then(|t| c.project_of_task(t)) {
                Some(pr) => vec![pr],
                None => c.projects_of_repo(repo),
            };
            let reference = match (projects.first(), p["pr"].as_i64()) {
                (Some(pr), Some(nr)) => json!(format!("{pr}#{nr}")),
                (None, Some(nr)) => json!(format!("{repo}#{nr}")),
                _ => Value::Null,
            };
            let ci = str_of(&p["ci"]);
            let (rank, key) = match (str_of(&p["state"]), ci) {
                (Some("merged"), _) => (
                    4,
                    if p["merged_via"] == "gate" {
                        "pr.merged_gate"
                    } else if p["merged_via"] == "hand" {
                        "pr.merged_hand"
                    } else {
                        "place.merged"
                    },
                ),
                (Some("closed"), _) => (3, "pr.closed_outside"),
                (_, Some("failed")) => (0, "stage.checks_failed"),
                (_, Some("running")) => (1, "stage.checks_running"),
                (_, Some("passed")) => (2, "pr.checks_passed"),
                _ => (2, "pr.open"),
            };
            let row = json!({
                "ref": reference, "repo": repo, "pr": p["pr"], "title": p["title"],
                "task_id": p["task_id"], "projects": projects, "words": c.w.get(key),
                "ci": p["ci"], "state": p["state"], "merged_by": p["merged_by"],
                "merge_state": p["merge_state"], "merge_order": p["merge_order"],
            });
            (rank, idx, row)
        })
        .collect();
    rows.sort_by_key(|(k, i, _)| (*k, *i));
    json!(rows.into_iter().map(|(_, _, v)| v).collect::<Vec<_>>())
}

fn mark(c: &Ctx, failed: usize, waiting: usize) -> (Value, Value) {
    if failed > 0 {
        (
            json!("failed"),
            json!(c.w.fill("tab.mark_failed", &[n(failed)])),
        )
    } else if waiting > 0 {
        (
            json!("needs_you"),
            json!(c.w.fill("tab.mark_needs_you", &[n(waiting)])),
        )
    } else {
        (Value::Null, Value::Null)
    }
}

fn views(c: &Ctx, failures: &Value, ready: &Value, one_off: &Value, prs: &Value) -> Value {
    let view = |count: usize, failed: usize, waiting: usize| {
        let (m, w) = mark(c, failed, waiting);
        json!({"count": count, "mark": m, "mark_words": w})
    };
    let f = arr(&failures["rows"]).len();
    let needs = c.w.get("place.needs_you");
    let did = c.w.get("fail.did_not_start");
    let oo = arr(one_off);
    let oo_failed = oo
        .iter()
        .filter(|r| {
            let t = str_of(&r["task_id"]).unwrap_or("");
            r["words"] == did.as_str() || c.anomaly(t).is_some()
        })
        .count();
    let oo_wait = oo.iter().filter(|r| r["words"] == needs.as_str()).count();
    let p = arr(prs);
    let p_failed = p
        .iter()
        .filter(|r| r["ci"] == "failed" && r["state"] == "open")
        .count();
    let mut out = BTreeMap::new();
    out.insert("failures", view(f, f, 0));
    // The Issues view: the ready count, with the Issues not ready and the projects not
    // checked counted apart (screen.md 3.1, "14 · not ready 5 · 1 project unchecked").
    let mut issues = view(arr(&ready["rows"]).len(), 0, 0);
    issues["not_ready"] = json!(arr(&ready["not_ready"]).len());
    issues["unchecked"] = json!(arr(&ready["unranked"]).len());
    out.insert("issues", issues);
    out.insert("one_off", view(oo.len(), oo_failed, oo_wait));
    out.insert("pull_requests", view(p.len(), p_failed, 0));
    json!(out)
}

/// The history of one task: `events` is every event of the task, as `makiba events --json
/// --after <id> --task <task>` prints them, gathered by the caller from `--after 0` page by page;
/// returned oldest first, each named in the person's words by its kind and payload, a kind with
/// no words by its own name.
pub fn history(events: &[Value], words: &Words) -> Value {
    let mut evs: Vec<&Value> = events.iter().collect();
    evs.sort_by_key(|e| e["id"].as_i64().unwrap_or(0));
    let rows: Vec<Value> = evs
        .iter()
        .map(|e| {
            let kind = str_of(&e["kind"]).unwrap_or("");
            let p = &e["payload"];
            let key = match kind {
                "belt_dispatched" => Some("hist.started.makiba"),
                // makibad writes one beside its belt_dispatched: started by makiba, not twice
                // by two hands.
                "delegate_sent" if e["actor"] == "makibad" => Some("hist.started.makiba"),
                "delegate_sent" => Some("hist.started.secretary"),
                "worker_spawn_verified" => Some("hist.began"),
                "worker_reported" => match str_of(&p["state"]) {
                    Some("start") => Some("hist.plan"),
                    Some("done") => Some("hist.done"),
                    Some("blocked") => Some("hist.blocked"),
                    _ => None,
                },
                "belt_stopped" => Some("hist.asked"),
                "pr_opened" => Some("hist.pr.opened"),
                "fix_pushed" => Some("hist.pr.updated"),
                "ci_completed" => match str_of(&p["status"]) {
                    Some("passed") => Some("hist.checks.passed"),
                    Some("failed") => Some("hist.checks.failed"),
                    _ => None,
                },
                "pr_merged" => Some("hist.merged"),
                "run_closed" => match str_of(&p["status"]) {
                    Some("completed") => Some("hist.closed.completed"),
                    Some("abandoned") => Some("hist.closed.abandoned"),
                    _ => None,
                },
                _ => None,
            };
            let text = key.map_or_else(|| kind.to_string(), |k| words.get(k));
            let detail = if kind == "worker_reported" && p["state"] == "done" {
                let r = &p["review"];
                let found: i64 = ["blocker", "major", "minor"]
                    .iter()
                    .filter_map(|k| r["findings"][k].as_i64())
                    .sum();
                if r["state"] == "passed" {
                    json!(words.get("hist.review.passed"))
                } else if found > 0 {
                    json!(words.fill("hist.review.found", &[n(found)]))
                } else {
                    Value::Null
                }
            } else {
                Value::Null
            };
            json!({"id": e["id"], "occurred_at": e["occurred_at"], "kind": kind,
                   "words": text, "detail": detail})
        })
        .collect();
    json!(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_795_514_400_000; // 2026-11-24T10:00:00Z

    fn base() -> Value {
        json!({"makibad": {"running": true, "heartbeat_at": "2026-11-24T09:59:57.000Z"},
               "projects": [{"slug": "loom", "repo": "example-org/loom"}],
               "runs": [], "terminals": [], "anomalies": [], "pending": [],
               "pull_requests": [], "ranges": [], "next_work": null})
    }

    fn range(id: i64, items: Value) -> Value {
        json!({"id": id, "project": "loom", "status": "active", "items": items,
               "approved_at": "2026-11-20T08:00:00.000Z", "places": {}})
    }

    #[test]
    fn several_landings_are_each_named() {
        let mut s = base();
        s["ranges"] = json!([range(
            1,
            json!([
            {"issue": 5, "place": "review", "stage": "waits_landing #12 #34", "why": null,
             "task_id": "t5", "since": "2026-11-24T09:00:00.000Z"},
            {"issue": 6, "place": "not_in", "stage": null, "why": "waits_for #7 #8",
             "task_id": null, "since": null}])
        )]);
        let m = model(&s, NOW, &Words::new("en"));
        let w = Words::new("en");
        assert_eq!(
            m["ranges"][0]["items"][0]["detail"],
            w.fill("stage.waits_landing", &[n("12, #34")])
        );
        assert_eq!(
            m["ranges"][0]["items"][1]["detail"],
            w.fill("stage.waits_for", &[n("7, #8")])
        );
    }

    #[test]
    fn a_stop_of_a_whole_range_selects_it() {
        let mut s = base();
        s["ranges"] = json!([range(1, json!([])), range(2, json!([]))]);
        s["ranges"][0]["approved_at"] = json!("2026-11-23T08:00:00.000Z");
        s["pending"] = json!([{"id": 9, "task_id": null, "status": "escalated", "range_id": 2,
                               "kind": "question", "question": "Budget spent; go on?",
                               "created_at": "2026-11-24T09:00:00.000Z", "context": {}}]);
        let m = model(&s, NOW, &Words::new("en"));
        assert_eq!(m["ranges"][1]["selected"], true);
        assert_eq!(m["ranges"][0]["selected"], false);
        assert_eq!(m["top"]["needs_you_in"], json!(["loom"]));
    }

    #[test]
    fn an_item_keeps_its_pull_request() {
        let mut s = base();
        s["ranges"] = json!([range(
            1,
            json!([
            {"issue": 5, "place": "landed", "task_id": "t5", "pr": 41, "pr_title": "Fix it",
             "since": "2026-11-23T09:00:00.000Z"}])
        )]);
        let m = model(&s, NOW, &Words::new("en"));
        assert_eq!(m["ranges"][0]["items"][0]["pr"], 41);
        assert_eq!(m["ranges"][0]["items"][0]["pr_title"], "Fix it");
    }

    #[test]
    fn one_off_tasks_of_one_rank_go_by_the_age_shown() {
        let mut s = base();
        let run = |t: &str, since: &str| {
            json!({"task_id": t, "project": "loom", "status": "in_use", "stage": "verified",
                   "range_id": null, "since": since, "waiting": null})
        };
        // a was dispatched first, but its error is newer than b's.
        s["runs"] = json!([
            run("a", "2026-11-24T06:00:00.000Z"),
            run("b", "2026-11-24T07:00:00.000Z")
        ]);
        s["anomalies"] = json!([
            {"task_id": "a", "rule": "error", "since": "2026-11-24T09:50:00.000Z"},
            {"task_id": "b", "rule": "error", "since": "2026-11-24T09:00:00.000Z"}]);
        let m = model(&s, NOW, &Words::new("en"));
        let order: Vec<&str> = arr(&m["one_off"])
            .iter()
            .map(|r| r["task_id"].as_str().unwrap())
            .collect();
        assert_eq!(order, vec!["b", "a"]);
    }
}

#[cfg(test)]
mod anomaly_age {
    use super::*;

    #[test]
    fn an_item_showing_an_anomaly_takes_its_age_and_order() {
        let item = |issue: i64, task: &str, since: &str| {
            json!({"issue": issue, "place": "in_work", "stage": "at_work", "why": null,
                   "task_id": task, "since": since})
        };
        let run = |t: &str| {
            json!({"task_id": t, "project": "loom", "status": "in_use", "stage": "verified",
                   "range_id": 1})
        };
        let s = json!({
            "makibad": {"running": true, "heartbeat_at": "2026-11-24T09:59:57.000Z"},
            "projects": [{"slug": "loom", "repo": "example-org/loom"}],
            "runs": [run("a"), run("b")], "terminals": [], "pending": [],
            "pull_requests": [], "next_work": null,
            // a was dispatched first, but its error is the newer one.
            "anomalies": [{"task_id": "a", "rule": "error", "since": "2026-11-24T09:50:00.000Z"},
                          {"task_id": "b", "rule": "error", "since": "2026-11-24T09:00:00.000Z"}],
            "ranges": [{"id": 1, "project": "loom", "status": "active", "places": {},
                        "items": [item(1, "a", "2026-11-24T06:00:00.000Z"),
                                  item(2, "b", "2026-11-24T07:00:00.000Z")]}],
        });
        let m = model(&s, 1_795_514_400_000, &Words::new("en"));
        let r = &m["ranges"][0];
        assert_eq!(r["items"][0]["age_s"], 600);
        let order: Vec<i64> = arr(&r["active"])
            .iter()
            .map(|i| i["issue"].as_i64().unwrap())
            .collect();
        assert_eq!(order, vec![2, 1]);
    }
}

#[cfg(test)]
mod round3 {
    use super::*;

    fn base() -> Value {
        json!({"makibad": {"running": true, "heartbeat_at": "2026-11-24T09:59:57.000Z"},
               "projects": [{"slug": "loom", "repo": "example-org/loom"}],
               "runs": [], "terminals": [], "anomalies": [], "pending": [],
               "pull_requests": [], "next_work": null, "ranges": []})
    }

    #[test]
    fn a_proposal_names_what_a_design_only_candidate_waits_for() {
        let mut s = base();
        s["ranges"] = json!([{"id": 4, "project": "loom", "status": "proposed", "items": [],
            "places": {}, "read_id": 7,
            "candidates": [{"issue": 9, "title": "T", "admitted": true, "left_out_why": null,
                            "design_only_waits": [3, 4]}]}]);
        let m = model(&s, 1_795_514_400_000, &Words::new("en"));
        let k = &m["ranges"][0]["proposal"]["would_start"][0];
        assert_eq!(k["design_only_waits"], json!([3, 4]));
        assert_eq!(
            k["detail"],
            Words::new("en").fill("stage.waits_landing", &[n("3, #4")])
        );
    }

    #[test]
    fn a_stop_of_a_whole_range_keeps_its_range_and_line() {
        let mut s = base();
        s["pending"] = json!([{"id": 9, "task_id": null, "status": "escalated", "range_id": 2,
                               "stop_line": "budget", "kind": "question", "question": "Go on?",
                               "created_at": "2026-11-24T09:00:00.000Z", "context": {}}]);
        let m = model(&s, 1_795_514_400_000, &Words::new("en"));
        let e = &m["answers"]["entries"][0];
        assert_eq!(e["range_id"], 2);
        assert_eq!(e["stop_line"], "budget");
    }

    #[test]
    fn a_belt_dispatch_is_started_by_makiba_once_in_words() {
        let ev = |id: i64, kind: &str, actor: &str| json!({"id": id, "kind": kind, "actor": actor, "payload": {}});
        let h = history(
            &[
                ev(1, "delegate_sent", "makibad"),
                ev(2, "belt_dispatched", "makibad"),
                ev(3, "delegate_sent", "secretary"),
            ],
            &Words::new("en"),
        );
        let w = Words::new("en");
        assert_eq!(h[0]["words"], w.get("hist.started.makiba"));
        assert_eq!(h[1]["words"], w.get("hist.started.makiba"));
        assert_eq!(h[2]["words"], w.get("hist.started.secretary"));
    }
}

#[cfg(test)]
mod round4 {
    use super::*;

    const NOW: i64 = 1_795_514_400_000;

    fn base() -> Value {
        json!({"makibad": {"running": true, "heartbeat_at": "2026-11-24T09:59:57.000Z"},
               "projects": [{"slug": "loom", "repo": "example-org/loom"}],
               "runs": [], "terminals": [], "anomalies": [], "pending": [],
               "pull_requests": [], "next_work": null, "ranges": []})
    }

    fn review_item(issue: i64, task: &str, pr: Value) -> Value {
        json!({"issue": issue, "place": "review", "stage": "ci", "why": null, "task_id": task,
               "pr": pr, "since": "2026-11-24T09:00:00.000Z"})
    }

    #[test]
    fn checks_are_found_by_task_else_by_the_items_own_number() {
        let mut s = base();
        // The one row of pull request 7 names task a; b's item names 7 itself; c's names none.
        s["pull_requests"] = json!([{"repo": "Example-Org/Loom", "pr": 7, "task_id": "a",
                                     "ci": "failed", "state": "open"}]);
        s["ranges"] = json!([{"id": 1, "project": "loom", "status": "active", "places": {},
            "items": [review_item(1, "a", Value::Null), review_item(2, "b", json!(7)),
                      review_item(3, "c", Value::Null)]}]);
        let m = model(&s, NOW, &Words::new("en"));
        let failed = Words::new("en").get("stage.checks_failed");
        let items = &m["ranges"][0]["items"];
        assert_eq!(items[0]["detail"], failed.as_str());
        assert_eq!(items[1]["detail"], failed.as_str());
        assert_eq!(items[2]["detail"], Value::Null);
    }

    #[test]
    fn a_review_item_with_an_open_anomaly_names_it() {
        let mut s = base();
        s["anomalies"] = json!([{"task_id": "a", "rule": "error",
                                 "since": "2026-11-24T09:50:00.000Z"}]);
        s["ranges"] = json!([{"id": 1, "project": "loom", "status": "active", "places": {},
            "items": [review_item(1, "a", Value::Null)]}]);
        let m = model(&s, NOW, &Words::new("en"));
        assert_eq!(
            m["ranges"][0]["items"][0]["words"],
            Words::new("en").get("fail.error")
        );
        assert_eq!(m["ranges"][0]["items"][0]["age_s"], 600);
    }

    #[test]
    fn the_issues_view_counts_not_ready_and_unchecked_apart() {
        let mut s = base();
        s["next_work"] = json!({"status": "no_candidates", "candidate_count": 0,
            "candidates": [], "input_truncated": "", "candidates_not_recorded": 0,
            "left_out_not_recorded": 0,
            "left_out": [
                {"project": "loom", "repo": "example-org/loom", "issue": 4, "kind": "blocked",
                 "reason": "r", "title": "t"},
                {"project": "loom", "repo": "example-org/loom", "issue": 5, "kind": "put_aside",
                 "reason": "r", "title": "t"},
                {"project": "atlas", "repo": null, "issue": null, "kind": "no_repository",
                 "reason": "r", "title": null}]});
        let m = model(&s, NOW, &Words::new("en"));
        let v = &m["views"]["issues"];
        assert_eq!(v["count"], 0);
        assert_eq!(v["not_ready"], 2);
        assert_eq!(v["unchecked"], 1);
    }
}
