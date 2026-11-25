//! M4-3: the role settings and the git guard hooks carried from paddock and claude-org-ja.
//!
//! - The cases of claude-org-ja's hook tests, ported, pass: `tests/hook_cases.json` holds each
//!   call those tests make (recorded from them, with makiba's names and folders), and every hook
//!   refuses each broken payload of `test-hooks-payload-fail-closed.sh`.
//! - Every line of `docs/design/lines.md` names a settings entry, a hook or a test that exists,
//!   or an Issue that will hold it.
//! - The rules file of each role carries the two wrapper sentences of spec section 8 exactly.

use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const ROLES: [&str; 3] = ["secretary", "worker", "curator"];

/// Every hook a role's settings may name; `deny-base` is M4-1's.
const HOOKS: [&str; 8] = [
    "deny-base",
    "deny-push",
    "deny-dangerous-git",
    "deny-no-verify",
    "deny-no-verify-code-run",
    "deny-workers-delete",
    "worker-boundary",
    "deny-foreground-subagent",
];

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(root().join(rel)).unwrap_or_else(|e| panic!("cannot read {rel}: {e}"))
}

/// The folders a hook run sees: a home, the agent's folder, and the worker and document folders.
struct Place {
    _dir: tempfile::TempDir,
    home: PathBuf,
    worktree: PathBuf,
    workers: PathBuf,
    docs: PathBuf,
}

impl Place {
    fn new() -> Place {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let p = Place {
            home: base.join("home"),
            worktree: base.join("worktree"),
            workers: base.join("workers"),
            docs: base.join("docs"),
            _dir: dir,
        };
        for d in [&p.home, &p.worktree, &p.workers, &p.docs] {
            std::fs::create_dir_all(d).unwrap();
        }
        p
    }

    /// The exit code and standard error of `makiba hook <name>` for `stdin`.
    fn hook(&self, name: &str, stdin: &[u8], in_worktree: bool) -> (i32, String) {
        let mut c = Command::new(env!("CARGO_BIN_EXE_makiba"));
        c.args(["hook", name])
            .env("HOME", &self.home)
            .env("MAKIBA_WORKERS_DIR", &self.workers)
            .env("MAKIBA_DOCS_DIR", &self.docs)
            .env_remove("CLAUDE_PROJECT_DIR")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        if in_worktree {
            c.env("CLAUDE_PROJECT_DIR", &self.worktree);
        }
        let mut child = c.spawn().expect("spawn makiba");
        child.stdin.take().unwrap().write_all(stdin).unwrap();
        let out = child.wait_with_output().expect("wait for makiba");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    /// `payload` with the fixture's placeholders replaced, JSON-escaped as the payload is JSON.
    fn fill(&self, payload: &str) -> String {
        let esc = |s: String| {
            serde_json::to_string(&s)
                .unwrap()
                .trim_matches('"')
                .to_string()
        };
        let workers = self.workers.display().to_string();
        payload
            .replace("{WORKTREE}", &esc(self.worktree.display().to_string()))
            .replace("{WORKERS_BACKSLASH}", &esc(workers.replace('/', "\\")))
            .replace("{WORKERS}", &esc(workers))
    }
}

/// The broken payloads of claude-org-ja's `tests/test-hooks-payload-fail-closed.sh`.
const BROKEN: [&str; 17] = [
    "",
    "  \n\t \n",
    "not json{",
    r#"{"tool_name":"Agent","tool_input":{"#,
    r#""a string""#,
    "[1,2,3]",
    "42",
    "true",
    "null",
    r#"{"tool_name":"Agent","tool_input":"not-object"}"#,
    r#"{"tool_name":"Bash","tool_input":[1,2,3]}"#,
    r#"{"tool_name":"Edit","tool_input":true}"#,
    "-n",
    "-e",
    "-neE",
    r#"{"tool_name":"Edit","tool_input":{"file_path":"/x"}} {"tool_name":"Edit","tool_input":{"file_path":"/x"}}"#,
    r#"{"tool_name":"Bash","tool_input":{"command":"ls"}} 42"#,
];

#[test]
fn the_cases_of_the_carried_hook_tests_pass() {
    let place = Place::new();
    let cases: Vec<Value> = serde_json::from_str(&read("tests/hook_cases.json")).unwrap();
    assert!(cases.len() > 500, "the fixture lost its cases");
    let mut failed = Vec::new();
    for case in &cases {
        let hook = case["hook"].as_str().unwrap();
        let expect = case["expect"].as_i64().unwrap() as i32;
        let payload = place.fill(case["payload"].as_str().unwrap());
        let in_worktree = case["worktree"].as_bool().unwrap_or(true);
        let (code, err) = place.hook(hook, payload.as_bytes(), in_worktree);
        if code != expect {
            failed.push(format!(
                "{hook} ({}): expected {expect}, got {code}: {payload:?} {err}",
                case["from"]
            ));
        }
    }
    for hook in HOOKS {
        for broken in BROKEN {
            let (code, err) = place.hook(hook, broken.as_bytes(), true);
            if code != 2 || !err.contains("payload") {
                failed.push(format!(
                    "{hook}: broken payload {broken:?} not refused as such: {code} {err}"
                ));
            }
        }
    }
    assert!(
        failed.is_empty(),
        "{} of {} cases failed:\n{}",
        failed.len(),
        cases.len() + HOOKS.len() * BROKEN.len(),
        failed.join("\n")
    );
}

/// True when `v` holds a string equal to `text` or ending with it.
fn holds_string(v: &Value, text: &str) -> bool {
    match v {
        Value::String(s) => s == text || s.ends_with(text),
        Value::Array(a) => a.iter().any(|x| holds_string(x, text)),
        Value::Object(o) => o.values().any(|x| holds_string(x, text)),
        _ => false,
    }
}

/// Why `reference` does not name something that exists, or `None` when it does.
fn missing(reference: &str, place: &Place, issues: &[u64]) -> Option<String> {
    if let Some(name) = reference.strip_prefix("hook ") {
        let (_, err) = place.hook(name, b"", true);
        return err
            .contains("usage:")
            .then(|| format!("no hook named {name}"));
    }
    if let Some(n) = reference.strip_prefix("Open: #") {
        let Ok(n) = n.parse::<u64>() else {
            return Some(format!("#{n} is not an Issue number"));
        };
        let newest = issues.iter().max().copied().unwrap_or(0);
        return (!issues.contains(&n) && n <= newest)
            .then(|| format!("#{n} is not an Issue of the plan"));
    }
    if let Some((file, text)) = reference.split_once(".json: ") {
        let file = format!("{file}.json");
        let settings: Value = serde_json::from_str(&read(&file)).unwrap();
        // `/a/b` is a JSON pointer to a setting; any other text is a string the settings hold.
        let held = if text.starts_with('/') {
            settings.pointer(text).is_some()
        } else {
            holds_string(&settings, text)
        };
        return (!held).then(|| format!("{file} holds no {text:?}"));
    }
    if reference.starts_with("tests/") {
        let (file, func) = reference
            .split_once("::")
            .map_or((reference, None), |(f, n)| (f, Some(n)));
        let Ok(text) = std::fs::read_to_string(root().join(file)) else {
            return Some(format!("no test file {file}"));
        };
        return func
            .filter(|f| !text.contains(&format!("fn {f}(")))
            .map(|f| format!("{file} has no fn {f}"));
    }
    Some("not a settings entry, a hook, a test or an Open Issue".into())
}

#[test]
fn every_line_in_lines_md_names_a_holder_that_exists() {
    let place = Place::new();
    let numbers: Value = serde_json::from_str(&read("issues/numbers.json")).unwrap();
    let issues: Vec<u64> = numbers
        .as_object()
        .unwrap()
        .values()
        .filter_map(Value::as_u64)
        .collect();
    let doc = read("docs/design/lines.md");
    let mut lines = Vec::new();
    let mut failed = Vec::new();
    for row in doc.lines().filter(|l| l.starts_with("| L-")) {
        let cells: Vec<&str> = row.trim_matches('|').split(" | ").collect();
        let id = cells[0].split_whitespace().next().unwrap().to_string();
        let held = cells.last().unwrap();
        let refs: Vec<&str> = held.split('`').skip(1).step_by(2).collect();
        if refs.is_empty() {
            failed.push(format!("{id} names nothing"));
        }
        for r in refs {
            if let Some(why) = missing(r, &place, &issues) {
                failed.push(format!("{id}: `{r}`: {why}"));
            }
        }
        lines.push(id);
    }
    let expected: Vec<String> = (1..=12)
        .map(|n| format!("L-K{n}"))
        .chain((1..=9).map(|n| format!("L-N{n}")))
        .collect();
    assert_eq!(lines, expected, "lines.md does not carry paddock's lines");
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

#[test]
fn the_rules_of_each_role_carry_the_wrapper_sentences() {
    for role in ROLES {
        let rules = read(&format!("roles/{role}/rules.md"));
        assert!(
            rules.contains(makiba::spec::WRAPPER_RULE),
            "roles/{role}/rules.md lacks the wrapper sentences of spec section 8"
        );
    }
}
