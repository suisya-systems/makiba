//! Issue 300: the hook `deny-no-verify` reads git's arguments, not the whole text of the command.
//!
//! - A commit whose message holds " -n", "bash -n" or "--no-verify" as text passes the hook, and
//!   passes every Bash hook and every deny rule of the worker's, the secretary's and the curator's
//!   settings: such a commit is not refused in any of the three sessions.
//! - git's flag that skips the hooks is still refused in each spelling the hook reads: `-n` alone
//!   or in a bundle, `--no-verify` and its abbreviations, through a variable, a wrapper, `bash -c`,
//!   `eval`, a substitution, a heredoc fed to a shell, an alias set in the same command, and with a
//!   redirection between git's words.
//! - The roles' settings hold no deny rule on `git commit` any more, and docs/design/lines.md names
//!   the hook and this test as what holds the flag (L-K3).

use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const ROLES: [&str; 3] = ["worker", "secretary", "curator"];

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(root().join(rel)).unwrap_or_else(|e| panic!("cannot read {rel}: {e}"))
}

fn settings(role: &str) -> Value {
    serde_json::from_str(&read(&format!("roles/{role}/settings.json"))).unwrap()
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

    /// The exit code and standard error of `makiba hook <name>` on a Bash payload of `command`.
    fn hook(&self, name: &str, command: &str) -> (i32, String) {
        let payload = serde_json::json!({"tool_name": "Bash", "tool_input": {"command": command}});
        let mut child = Command::new(env!("CARGO_BIN_EXE_makiba"))
            .args(["hook", name])
            .env("HOME", &self.home)
            .env("MAKIBA_WORKERS_DIR", &self.workers)
            .env("MAKIBA_DOCS_DIR", self.home.join("docs"))
            .env("CLAUDE_PROJECT_DIR", &self.worktree)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn makiba");
        let _ = child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.to_string().as_bytes());
        let out = child.wait_with_output().expect("wait for makiba");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

/// The makiba hooks a role's settings run on Bash, by name.
fn bash_hooks(role: &str) -> Vec<String> {
    let s = settings(role);
    let mut out = Vec::new();
    for group in s["hooks"]["PreToolUse"].as_array().into_iter().flatten() {
        if group["matcher"].as_str() != Some("Bash") {
            continue;
        }
        for h in group["hooks"].as_array().into_iter().flatten() {
            let cmd = h["command"].as_str().unwrap_or_default();
            if let Some((_, name)) = cmd.rsplit_once(" hook ") {
                out.push(name.trim().to_string());
            }
        }
    }
    out
}

/// The `Bash(...)` deny rules of a role's settings, as their patterns.
fn bash_denies(role: &str) -> Vec<String> {
    settings(role)["permissions"]["deny"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(|r| r.strip_prefix("Bash(")?.strip_suffix(')'))
        .map(String::from)
        .collect()
}

/// Whether a deny pattern matches `command`, matching more than Claude Code does so as to be
/// safe: `prefix:*` is a prefix of the command, or of any line or `&&`/`;`/`|` part of it; any
/// other pattern is a glob whose `*` matches any text, newlines included, over the same texts.
fn rule_matches(pattern: &str, command: &str) -> bool {
    let mut texts = vec![command.to_string()];
    for part in command.split(['\n', ';', '|', '&']) {
        texts.push(part.trim().to_string());
    }
    texts.iter().any(|t| match pattern.strip_suffix(":*") {
        Some(prefix) => t.starts_with(prefix),
        None => glob(pattern.as_bytes(), t.as_bytes()),
    })
}

fn glob(p: &[u8], t: &[u8]) -> bool {
    match p.split_first() {
        None => t.is_empty(),
        Some((b'*', rest)) => (0..=t.len()).any(|i| glob(rest, &t[i..])),
        Some((c, rest)) => t.first() == Some(c) && glob(rest, &t[1..]),
    }
}

/// Commits whose message holds the flag's letters as text: no session may refuse them.
const MESSAGES: [&str; 6] = [
    "git commit -m \"bash -n check\"",
    "git commit -m 'ci: check the scripts with bash -n' -m 'git commit -n is --no-verify'",
    "git commit --message=\"say -n and --no-veri here\"",
    "git commit -m \"first line\nthe scripts pass bash -n\nand --no-verify is refused\"",
    "git commit -F - <<'EOF'\nci: bash -n on the scripts\n\ngit commit -n is --no-verify\nEOF",
    "git add a.sh && git commit -m \"run bash -n\" -- a.sh",
];

#[test]
fn a_commit_whose_message_holds_the_flags_letters_passes_every_roles_hooks_and_rules() {
    let place = Place::new();
    let mut failed = Vec::new();
    for role in ROLES {
        let hooks = bash_hooks(role);
        assert!(
            hooks.iter().any(|h| h == "deny-no-verify"),
            "{role}: the settings do not run deny-no-verify on Bash"
        );
        for command in MESSAGES {
            for hook in &hooks {
                let (code, err) = place.hook(hook, command);
                if code != 0 {
                    failed.push(format!("{role}: hook {hook} refused {command:?}: {err}"));
                }
            }
            for rule in bash_denies(role) {
                if rule_matches(&rule, command) {
                    failed.push(format!(
                        "{role}: deny rule Bash({rule}) matches {command:?}"
                    ));
                }
            }
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

#[test]
fn text_that_is_not_an_argument_of_git_passes_the_hook() {
    let place = Place::new();
    let commands = [
        // A message that starts with a dash is the value of -m, as git reads it.
        "git commit -m -n",
        "git commit -m '--no-verify is refused'",
        "git commit -am 'bash -n'",
        // A message read from a heredoc in a substitution. (The secretary's merge-gate refuses
        // this form for reasons of its own, outside Issue 300.)
        "git commit -m \"$(cat <<'EOF'\nci: bash -n on the scripts\n\nNot --no-verify.\nEOF\n)\"",
        // A heredoc fed to a text program is data (the secretary's case of 2026-11-22).
        "gh issue create --title x --body \"$(cat <<'EOF'\nThe rule: git commit -n is --no-verify.\nEOF\n)\"",
        "cat > notes.md <<'EOF'\ngit commit -n -m x\ngit push --no-verify\nEOF",
        // A pattern or a mention inside quotes.
        "grep -e 'git commit -n' docs/design/lines.md",
        "echo 'git commit --no-verify'",
        // Other subcommands' options.
        "git merge --no-verify-signatures feat/x",
        "git log -n 3 && git commit -m x",
    ];
    let mut failed = Vec::new();
    for command in commands {
        let (code, err) = place.hook("deny-no-verify", command);
        if code != 0 {
            failed.push(format!("refused {command:?}: {err}"));
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

#[test]
fn the_flag_itself_is_still_refused_in_each_spelling_the_hook_reads() {
    let place = Place::new();
    let commands = [
        "git commit -n -m x",
        "git commit -an",
        "git commit -anm x",
        "git commit -nm x",
        "git commit \"-n\" -m x",
        "git commit -m 'fix -n handling' -n",
        "git commit --no-verify -m x",
        "git commit --no-verif -m x",
        "git commit --no-veri -m x",
        "git commit -m x --no-veri",
        "F=--no-verify; git commit $F -m x",
        "M=\"'\"; git commit -m $M -n",
        "bash -c 'git commit -n -m x'",
        "eval \"git commit -n -m x\"",
        "/usr/bin/git commit -n -m x",
        "git -C sub commit -n -m x",
        "env git commit -n -m x",
        "git -c alias.ci='commit -n' ci -m x",
        "git -c alias.ci='!git commit -n' ci -m x",
        "git config alias.ci 'commit -n'; git ci -m x",
        "git >/dev/null commit -n -m x",
        "git commit 2>&1 -n -m x",
        "git commit >out -n -m x",
        "echo \"$(git commit -n -m x)\"",
        "cat <<EOF\n$(git commit -n -m x)\nEOF",
        "bash <<EOF\ngit commit -n -m x\nEOF",
        "git commit -m x && git commit --amend -n",
        "git push --no-verify origin feat/x",
        "git push --no-veri origin feat/x",
        "git merge --no-verify feat/x",
        "git merge -m 'a merge' --no-verify feat/x",
        "git pull --no-verify",
        "git am --no-verify a.patch",
    ];
    let mut passed = Vec::new();
    for command in commands {
        let (code, _) = place.hook("deny-no-verify", command);
        if code != 2 {
            passed.push(format!("{command:?} -> {code}"));
        }
    }
    assert!(passed.is_empty(), "not refused:\n{}", passed.join("\n"));
}

#[test]
fn no_role_keeps_a_deny_rule_on_git_commit_and_lines_md_names_the_hook() {
    for role in ROLES {
        for rule in bash_denies(role) {
            assert!(
                !rule.starts_with("git commit"),
                "{role}: the deny rule Bash({rule}) is still there"
            );
        }
    }
    let doc = read("docs/design/lines.md");
    let row = doc
        .lines()
        .find(|l| l.starts_with("| L-K3 "))
        .expect("lines.md has L-K3");
    assert!(row.contains("`hook deny-no-verify`"), "{row}");
    assert!(row.contains("`tests/no_verify_arguments.rs"), "{row}");
    assert!(
        !doc.contains("Bash(git commit"),
        "lines.md still names a deny rule on git commit"
    );
}
