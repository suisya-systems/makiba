//! M4-3, beyond the carried cases: what makiba's git hooks add to claude-org-ja's (a git named by
//! its path, a push spelt with quotes or a variable), and that no payload makes a hook crash,
//! since a crash exits with a code other than 2 and Claude Code takes that as a pass.

use std::io::Write;
use std::process::{Command, Stdio};

fn hook(name: &str, stdin: &[u8]) -> i32 {
    let mut child = Command::new(env!("CARGO_BIN_EXE_makiba"))
        .args(["hook", name])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn makiba");
    // The hook may refuse before it has read everything.
    let _ = child.stdin.take().unwrap().write_all(stdin);
    child.wait().expect("wait for makiba").code().unwrap_or(-1)
}

fn bash(command: &str) -> Vec<u8> {
    serde_json::json!({"tool_name": "Bash", "tool_input": {"command": command}})
        .to_string()
        .into_bytes()
}

#[test]
fn a_git_named_by_its_path_or_a_quoted_push_is_refused_and_a_mention_passes() {
    for (name, command, expect) in [
        ("deny-dangerous-git", "/usr/bin/git reset --hard", 2),
        ("deny-dangerous-git", "/usr/bin/git status", 0),
        ("deny-no-verify", "/usr/bin/git commit --no-verify -m x", 2),
        ("deny-push", "/usr/bin/git push", 2),
        ("deny-push", "git \"push\" origin HEAD", 2),
        ("deny-push", "git pu\\sh", 2),
        ("deny-push", "P=push; git $P", 2),
        ("deny-push", "git commit -m \"say 'git push' later\"", 0),
        ("deny-push", "git log --oneline", 0),
    ] {
        assert_eq!(hook(name, &bash(command)), expect, "{name}: {command}");
    }
}

#[test]
fn a_long_payload_gets_a_verdict_and_one_over_the_limit_is_refused() {
    let hooks = [
        "deny-base",
        "deny-push",
        "deny-dangerous-git",
        "deny-no-verify",
        "deny-workers-delete",
    ];
    // Shapes that make the matcher recurse deepest: long words, long runs of blanks, many
    // options after git, many separators, deep quotes.
    let n = 12_000;
    let shapes = [
        format!("git log {}", "a".repeat(n * 5)),
        format!("git {}push", " ".repeat(n * 5)),
        format!("git {} push", "-a ".repeat(n)),
        format!("echo {}", "x;".repeat(n * 2)),
        format!("echo \"{}\"", "$(a)".repeat(n)),
        format!("rm -r {}", "/a".repeat(n * 2)),
    ];
    for shape in &shapes {
        let payload = bash(shape);
        assert!(payload.len() < 64 * 1024, "a shape is over the limit");
        for name in hooks {
            let code = hook(name, &payload);
            assert!(
                code == 0 || code == 2,
                "{name} crashed ({code}) on {:.40}",
                shape
            );
        }
    }
    let over = bash(&"a".repeat(300 * 1024));
    for name in hooks {
        assert_eq!(hook(name, &over), 2, "{name} took a payload over the limit");
    }
}
