//! Issue 306: a refusal names the rule that stopped the command.
//!
//! - Every refusal by a makiba hook names the hook, the line it holds (its id in
//!   docs/design/lines.md) and the part of the command or input that matched, in the form
//!   `makiba: refused by hook <name> (line <id> in docs/design/lines.md): <reason>. Matched: "<text>"`.
//!   One refusal per hook verb is checked, and each line id named is a row of lines.md.
//! - The secret scanner's refusal names the file, line and shape, never the token itself.
//! - docs/design/lines.md lists every `permissions.deny` rule of the roles' settings, which
//!   Claude Code refuses with no reason, and says why it stays; no role keeps a rule on
//!   `git commit` text (Issue 300: git's own prepare-commit-msg and the no-verify hooks hold
//!   the flag, and name their rule).

use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const ROLES: [&str; 3] = ["worker", "secretary", "curator"];

/// The heading of the section of lines.md that lists the silent deny rules.
const SILENT_RULES: &str = "## Deny rules that refuse without a reason";

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(root().join(rel)).unwrap_or_else(|e| panic!("cannot read {rel}: {e}"))
}

/// The folders a hook run sees.
struct Place {
    _dir: tempfile::TempDir,
    home: PathBuf,
    worktree: PathBuf,
    workers: PathBuf,
}

impl Place {
    fn new() -> Place {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let p = Place {
            home: base.join("home"),
            worktree: base.join("workers/t1"),
            workers: base.join("workers"),
            _dir: dir,
        };
        for d in [&p.home, &p.worktree] {
            std::fs::create_dir_all(d).unwrap();
        }
        p
    }

    /// The exit code and standard error of `makiba hook <args>` on `stdin`.
    fn run(&self, args: &[&str], stdin: &[u8]) -> (i32, String) {
        let mut child = Command::new(env!("CARGO_BIN_EXE_makiba"))
            .arg("hook")
            .args(args)
            .env("HOME", &self.home)
            .env("MAKIBA_WORKERS_DIR", &self.workers)
            .env("MAKIBA_DOCS_DIR", self.home.join("docs"))
            .env("CLAUDE_PROJECT_DIR", &self.worktree)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn makiba");
        let _ = child.stdin.take().unwrap().write_all(stdin);
        let out = child.wait_with_output().expect("wait for makiba");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

fn bash(command: &str) -> Vec<u8> {
    json!({"tool_name": "Bash", "tool_input": {"command": command}})
        .to_string()
        .into_bytes()
}

/// The line ids that are rows of lines.md.
fn line_ids() -> Vec<String> {
    read("docs/design/lines.md")
        .lines()
        .filter(|l| l.starts_with("| L-"))
        .filter_map(|l| l.trim_start_matches("| ").split_whitespace().next())
        .map(String::from)
        .collect()
}

/// The text after `Matched: "` up to the last quote of the refusal, or `None`.
fn matched(err: &str) -> Option<&str> {
    let at = err.find("Matched: \"")? + "Matched: \"".len();
    let rest = &err[at..];
    Some(&rest[..rest.rfind('"')?])
}

/// Checks one refusal; returns what is wrong with it.
fn check(
    what: &str,
    (code, err): (i32, String),
    hook: &str,
    line: &str,
    expect_matched: &str,
) -> Vec<String> {
    let mut wrong = Vec::new();
    if code == 0 {
        wrong.push(format!("{what}: not refused"));
        return wrong;
    }
    if !err.contains("makiba: refused") {
        wrong.push(format!("{what}: no \"makiba: refused\": {err}"));
    }
    if !err.contains(&format!("by hook {hook} ")) {
        wrong.push(format!("{what}: does not name the hook {hook}: {err}"));
    }
    if !err.contains(&format!("(line {line} in docs/design/lines.md)")) {
        wrong.push(format!("{what}: does not name the line {line}: {err}"));
    }
    if !line_ids().iter().any(|id| id == line) {
        wrong.push(format!("{what}: {line} is not a row of lines.md"));
    }
    match matched(&err) {
        Some(m) if m.contains(expect_matched) => {}
        _ => wrong.push(format!(
            "{what}: the matched text does not hold {expect_matched:?}: {err}"
        )),
    }
    wrong
}

#[test]
fn each_hooks_refusal_names_the_hook_the_line_and_what_matched() {
    let p = Place::new();
    let mut wrong = Vec::new();
    // The secretary's and the curator's no-verify hook reads the commit flag in the text; the
    // worker's (Issue 300) leaves it to git's prepare-commit-msg and still refuses the others.
    let bash_cases: [(&str, &str, &str, &str); 7] = [
        ("deny-base", "hc list", "L-N2", "hc"),
        ("deny-push", "git push origin main", "L-K2", "git push"),
        (
            "deny-dangerous-git",
            "git reset --hard HEAD~1",
            "L-K3",
            "reset --hard",
        ),
        ("deny-no-verify", "git commit -n -m x", "L-K3", "-n"),
        (
            "deny-no-verify-code-run",
            "git push --no-verify origin x",
            "L-K3",
            "--no-verify",
        ),
        (
            "merge-gate",
            "gh pr merge 5 --squash",
            "L-K9",
            "gh pr merge",
        ),
        (
            "deny-push",
            "gh issue create --title x --body y",
            "L-K2",
            "issue",
        ),
    ];
    for (hook, command, line, m) in bash_cases {
        wrong.extend(check(
            command,
            p.run(&[hook], &bash(command)),
            hook,
            line,
            m,
        ));
    }
    // A recursive delete naming the folder of worktrees.
    let workers = p.workers.display().to_string();
    let delete = format!("rm -rf {workers}/t2");
    wrong.extend(check(
        &delete,
        p.run(&["deny-workers-delete"], &bash(&delete)),
        "deny-workers-delete",
        "L-K6",
        &workers,
    ));
    // A write outside the agent's folder, and a write of its settings.
    let outside = p.home.join("x.txt").display().to_string();
    let write = |path: &str| {
        json!({"tool_name": "Write", "tool_input": {"file_path": path, "content": "x"}})
            .to_string()
            .into_bytes()
    };
    wrong.extend(check(
        "a write outside",
        p.run(&["worker-boundary"], &write(&outside)),
        "worker-boundary",
        "L-K4",
        &outside,
    ));
    let own = p
        .worktree
        .join(".claude/settings.json")
        .display()
        .to_string();
    wrong.extend(check(
        "a write of the settings",
        p.run(&["worker-boundary"], &write(&own)),
        "worker-boundary",
        "L-K5",
        ".claude/settings.json",
    ));
    // A subagent in the foreground.
    let task = json!({"tool_name": "Task", "tool_input": {"prompt": "x"}})
        .to_string()
        .into_bytes();
    wrong.extend(check(
        "a foreground Task",
        p.run(&["deny-foreground-subagent"], &task),
        "deny-foreground-subagent",
        "L-K8",
        "run_in_background",
    ));
    // A payload a hook cannot read.
    wrong.extend(check(
        "a broken payload",
        p.run(&["deny-push"], b"not json{"),
        "deny-push",
        "L-K11",
        "payload",
    ));
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

#[test]
fn the_secret_scanners_refusal_names_the_hook_the_line_and_where_but_not_the_token() {
    let p = Place::new();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("COMMIT_EDITMSG");
    // Built at run time, so that no token-shaped line is in this file.
    let token = format!("{}{}", "AKIA", "Q".repeat(16));
    std::fs::write(&file, format!("fix: a thing\n\nkey {token}\n")).unwrap();
    let (code, err) = p.run(&["commit-msg", file.to_str().unwrap()], b"");
    let wrong = check(
        "a token in a commit message",
        (code, err.clone()),
        "commit-msg",
        "L-K12",
        "line 3",
    );
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    assert!(!err.contains(&token), "the refusal prints the token: {err}");
}

/// The rules of the three roles' `permissions.deny`.
fn deny_rules() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for role in ROLES {
        let s: Value = serde_json::from_str(&read(&format!("roles/{role}/settings.json"))).unwrap();
        for r in s["permissions"]["deny"].as_array().into_iter().flatten() {
            let r = r.as_str().unwrap().to_string();
            if !out.contains(&r) {
                out.push(r);
            }
        }
    }
    out
}

#[test]
fn lines_md_says_why_each_silent_deny_rule_stays() {
    let doc = read("docs/design/lines.md");
    let at = doc
        .find(SILENT_RULES)
        .unwrap_or_else(|| panic!("lines.md has no section {SILENT_RULES:?}"));
    let section = &doc[at..];
    let section = match section[SILENT_RULES.len()..].find("\n## ") {
        Some(end) => &section[..SILENT_RULES.len() + end],
        None => section,
    };
    let named: Vec<&str> = section.split('`').skip(1).step_by(2).collect();
    let rules = deny_rules();
    let missing: Vec<&String> = rules
        .iter()
        .filter(|r| !named.contains(&r.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "lines.md does not say why these deny rules stay: {missing:?}"
    );
    // Each rule the section names that looks like a rule is one of the roles' rules.
    let stale: Vec<&&str> = named
        .iter()
        .filter(|n| ["Bash(", "Read(", "Edit("].iter().any(|p| n.starts_with(p)))
        .filter(|n| !rules.iter().any(|r| r == **n))
        .collect();
    assert!(
        stale.is_empty(),
        "lines.md names deny rules no role holds: {stale:?}"
    );
    // No role keeps a rule on git commit text (Issue 300): such a rule matches a message too.
    let on_commit: Vec<&String> = rules
        .iter()
        .filter(|r| r.starts_with("Bash(git commit"))
        .collect();
    assert!(
        on_commit.is_empty(),
        "a role still denies git commit text, which refuses a message holding its letters: {on_commit:?}"
    );
}
