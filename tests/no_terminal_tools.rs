//! M6-3 (Issue 44): no skill and no rules file operates a terminal (goal clause G3).
//!
//! - No file under `skills/` or `roles/` names the base's command or a pane tool. The role
//!   settings (`roles/<role>/settings.json`) are exempt: they are the deny rules for the base's
//!   command and folders, so they must name them (M4-1; the comment on Issue 44).
//! - Every tool or command a skill names is a verb of the table in spec section 12. A skill names
//!   a tool in one of three forms, and only these count as names; any other word is prose:
//!   `mcp__<server>__<tool>` (the server must be `makiba`), `` `makiba <verb>` `` (the person's
//!   command) and `` `<tool>` tool``. The mailbox tools of the systems makiba replaces
//!   (`send_message`, `check_messages`) are refused in any form.
//! - The secretary's rules say when to send `typed` and that it never goes to the secretary
//!   (spec section 7), and keep the two wrapper sentences of spec section 8.

use regex::Regex;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The skills this Issue builds.
const SKILLS: [&str; 8] = [
    "delegate",
    "escalation",
    "pull-request",
    "issue",
    "update-branch",
    "curate",
    "retro",
    "document",
];

/// What no skill or rules file may name (Issue 44), matched without regard to case. `hc ` counts
/// only where a word starts, so "which " is not a match.
const FORBIDDEN: [&str; 9] = [
    "hc ",
    "herdr",
    "send_keys",
    "inspect_pane",
    "list_panes",
    "spawn_pane",
    "close_pane",
    "tmux",
    "wezterm",
];

/// Mailbox tools of paddock and claude-org-ja, which the door's tools replace.
const OLD_MAILBOX: [&str; 2] = ["send_message", "check_messages"];

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// Every regular file under `dir`, sorted; none when `dir` does not exist.
fn files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries {
            let p = e.unwrap().path();
            if p.is_dir() {
                todo.push(p);
            } else if p.is_file() {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// A role's settings file: `roles/<role>/settings.json`, exactly.
fn is_role_settings(base: &Path, path: &Path) -> bool {
    let rel = path.strip_prefix(base).unwrap();
    let parts: Vec<_> = rel.components().collect();
    parts.len() == 3 && parts[0].as_os_str() == "roles" && parts[2].as_os_str() == "settings.json"
}

/// The check: one `file:line: word` for each forbidden word in `skills/` and `roles/` of `base`.
fn scan(base: &Path) -> Vec<String> {
    let mut found = Vec::new();
    for dir in ["skills", "roles"] {
        for path in files(&base.join(dir)) {
            if is_role_settings(base, &path) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue; // not text
            };
            let rel = path.strip_prefix(base).unwrap().display().to_string();
            for (n, line) in text.lines().enumerate() {
                let lower = line.to_lowercase();
                for word in FORBIDDEN {
                    let hit = lower.match_indices(word).any(|(i, _)| {
                        word != "hc " || i == 0 || !lower.as_bytes()[i - 1].is_ascii_alphanumeric()
                    });
                    if hit {
                        found.push(format!("{rel}:{}: {}", n + 1, word.trim_end()));
                    }
                }
            }
        }
    }
    found
}

/// The table of spec section 12: (the verbs of the command, the tools for agents).
fn verb_table() -> (BTreeSet<String>, BTreeSet<String>) {
    let spec = read(&root().join("docs/design/spec.md"));
    let start = spec.find("\n## 12.").expect("spec.md has no section 12");
    let end = start
        + spec[start..]
            .find("\n## 13.")
            .expect("spec.md has no section 13");
    let tick = Regex::new(r"^`([a-z]+)`").unwrap();
    let (mut verbs, mut tools) = (BTreeSet::new(), BTreeSet::new());
    for line in spec[start..end].lines().filter(|l| l.starts_with("| ")) {
        let cells: Vec<&str> = line.trim_matches('|').split(" | ").map(str::trim).collect();
        if let Some(c) = tick.captures(cells[0]) {
            verbs.insert(c[1].to_string());
        }
        if let Some(c) = tick.captures(cells[cells.len() - 1]) {
            tools.insert(c[1].to_string());
        }
    }
    assert!(
        verbs.contains("dispatch") && tools.contains("inbox"),
        "section 12's table not read"
    );
    (verbs, tools)
}

/// Each tool or command named in the skills of `base` that is not in the table, as `file:line: name`.
fn unknown_names(base: &Path) -> Vec<String> {
    let (verbs, tools) = verb_table();
    let mcp = Regex::new(r"mcp__([A-Za-z0-9_-]+?)__([A-Za-z0-9_]+)").unwrap();
    let command = Regex::new(r"`makiba ([a-z_-]+)").unwrap();
    let tool = Regex::new(r"`([A-Za-z0-9_-]+)` tool").unwrap();
    let mut bad = Vec::new();
    for path in files(&base.join("skills")) {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let rel = path.strip_prefix(base).unwrap().display().to_string();
        for (n, line) in text.lines().enumerate() {
            let mut miss = |name: String| bad.push(format!("{rel}:{}: {name}", n + 1));
            for c in mcp.captures_iter(line) {
                if &c[1] != "makiba" || !tools.contains(&c[2]) {
                    miss(c[0].to_string());
                }
            }
            for c in command.captures_iter(line) {
                if !verbs.contains(&c[1]) {
                    miss(c[0].to_string());
                }
            }
            for c in tool.captures_iter(line) {
                if !tools.contains(&c[1]) {
                    miss(c[0].to_string());
                }
            }
            for old in OLD_MAILBOX {
                if line.contains(old) {
                    miss(old.to_string());
                }
            }
        }
    }
    bad
}

fn write(base: &Path, rel: &str, text: &str) {
    let p = base.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

#[test]
fn the_repository_names_no_terminal_tool() {
    let missing: Vec<_> = SKILLS
        .iter()
        .filter(|s| !root().join(format!("skills/{s}/SKILL.md")).is_file())
        .collect();
    assert!(missing.is_empty(), "skills missing: {missing:?}");
    let found = scan(root());
    assert!(
        found.is_empty(),
        "terminal tools named:\n{}",
        found.join("\n")
    );
}

#[test]
fn a_scratch_skill_naming_hc_send_fails_the_scan() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path();
    write(
        base,
        "skills/scratch/SKILL.md",
        "# scratch\n\nRun hc send w-1 hello.\n",
    );
    write(
        base,
        "skills/scratch/notes.md",
        "Which way: no match here.\n",
    );
    write(
        base,
        "roles/x/settings.json",
        "{\"deny\": [\"Bash(hc:*)\", \"herdr\"]}\n",
    );
    write(base, "skills/scratch/settings.json", "[\"tmux\"]\n");
    assert_eq!(
        scan(base),
        [
            "skills/scratch/SKILL.md:3: hc",
            "skills/scratch/settings.json:1: tmux"
        ],
        "only the skill's two lines are findings; a role's settings.json is exempt"
    );
}

#[test]
fn every_tool_or_command_a_skill_names_is_a_verb_of_section_12() {
    let bad = unknown_names(root());
    assert!(
        bad.is_empty(),
        "names not in spec section 12:\n{}",
        bad.join("\n")
    );

    // The check itself finds what it must.
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path();
    write(
        base,
        "skills/scratch/SKILL.md",
        "Call mcp__makiba__dispatch, then the `inbox` tool; tell the person to run `makiba down`.\n\
         Call mcp__makiba__spawn.\n\
         Call mcp__org-broker__send_message.\n\
         Run `makiba launch`.\n\
         Call the `peek_pane` tool.\n\
         Run check_messages.\n",
    );
    assert_eq!(
        unknown_names(base),
        [
            "skills/scratch/SKILL.md:2: mcp__makiba__spawn",
            "skills/scratch/SKILL.md:3: mcp__org-broker__send_message",
            "skills/scratch/SKILL.md:3: send_message",
            "skills/scratch/SKILL.md:4: `makiba launch",
            "skills/scratch/SKILL.md:5: `peek_pane` tool",
            "skills/scratch/SKILL.md:6: check_messages",
        ]
    );
}

#[test]
fn the_secretary_rules_say_when_to_send_typed() {
    let rules = read(&root().join("roles/secretary/rules.md"));
    let typed: Vec<String> = rules
        .lines()
        .filter(|l| l.contains("`typed`"))
        .map(str::to_lowercase)
        .collect();
    assert!(
        !typed.is_empty(),
        "roles/secretary/rules.md does not name `typed`"
    );
    assert!(
        typed.iter().any(|l| l.contains("refused")),
        "the rules do not say to send again with `typed` when a channel message was refused"
    );
    assert!(
        typed.iter().any(|l| l.contains("from the start")),
        "the rules do not say to send typed at once an instruction known from the start to need it"
    );
    assert!(
        typed
            .iter()
            .any(|l| l.contains("never") && l.contains("secretary")),
        "the rules do not say that `typed` never goes to the secretary"
    );
}

#[test]
fn the_secretary_rules_keep_the_wrapper_sentences() {
    let rules = read(&root().join("roles/secretary/rules.md"));
    assert!(
        rules.contains(makiba::spec::WRAPPER_RULE),
        "roles/secretary/rules.md lacks the wrapper sentences of spec section 8"
    );
}
