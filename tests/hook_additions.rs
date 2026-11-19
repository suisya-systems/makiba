//! M4-3, beyond the carried cases: what makiba's hooks add to claude-org-ja's (a git or rm named by
//! its path, a push spelt with quotes or a variable), and that no payload makes a hook crash,
//! since a crash exits with a code other than 2 and Claude Code takes that as a pass.

use std::io::Write;
use std::process::{Command, Stdio};

fn hook(name: &str, stdin: &[u8]) -> i32 {
    hook_in(name, stdin, &[])
}

fn hook_in(name: &str, stdin: &[u8], env: &[(&str, &std::path::Path)]) -> i32 {
    let mut child = Command::new(env!("CARGO_BIN_EXE_makiba"))
        .args(["hook", name])
        .envs(env.iter().copied())
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
        ("deny-dangerous-git", "git push origin +HEAD:main", 2),
        ("deny-dangerous-git", "git push origin feat/x +feat/y", 2),
        ("deny-dangerous-git", "git push origin feat/x", 0),
        // A quoted or escaped subcommand, and a variable assigned twice, are refused unread.
        ("deny-dangerous-git", "git re\"set\" --hard", 2),
        ("deny-dangerous-git", "git -C . re\\set --hard", 2),
        ("deny-no-verify", "git com\"mit\" -n -m x", 2),
        ("deny-push", "P=status; P=push; git $P origin HEAD", 2),
        ("deny-dangerous-git", "P=status; P=reset; git $P --hard", 2),
        (
            "deny-dangerous-git",
            "git commit -m \"a 'quoted' message\"",
            0,
        ),
        ("deny-dangerous-git", "A=1; B=2; git status", 0),
        ("deny-no-verify", "/usr/bin/git commit --no-verify -m x", 2),
        (
            "deny-no-verify",
            "git -ccore.hooksPath=/dev/null commit -m x",
            2,
        ),
        (
            "deny-no-verify",
            "git -chook.pre-commit.command=true commit -m x",
            2,
        ),
        ("deny-no-verify", "git -c user.name=x commit -m x", 0),
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
fn an_rm_named_by_its_path_is_seen_and_a_large_write_is_not_refused_for_its_size() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let workers = root.join("workers");
    let delete = bash(&format!("/bin/rm -rf {}/t1", workers.display()));
    let env = [("MAKIBA_WORKERS_DIR", workers.as_path())];
    assert_eq!(hook_in("deny-workers-delete", &delete, &env), 2);
    let write = serde_json::json!({"tool_name": "Write", "tool_input": {
        "file_path": root.join("big.txt"), "content": "x".repeat(300 * 1024)}});
    let env = [("CLAUDE_PROJECT_DIR", root.as_path())];
    let write = write.to_string().into_bytes();
    assert_eq!(hook_in("worker-boundary", &write, &env), 0);
}

#[test]
fn a_link_to_a_locked_file_or_out_of_the_folder_is_followed() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join(".claude")).unwrap();
    std::fs::write(root.join("conf.json"), "{}").unwrap();
    std::os::unix::fs::symlink(root.join("conf.json"), root.join(".claude/settings.json")).unwrap();
    // A link in the folder to a file outside it that does not exist yet: a write creates that file.
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path().join("new.txt"), root.join("dangling")).unwrap();
    let env = [("CLAUDE_PROJECT_DIR", root.as_path())];
    for (target, expect) in [
        ("conf.json", 2),
        (".claude/settings.json", 2),
        ("dangling", 2),
        ("other.json", 0),
    ] {
        let write = serde_json::json!({"tool_name": "Write", "tool_input": {
            "file_path": root.join(target), "content": "{}"}});
        let code = hook_in("worker-boundary", write.to_string().as_bytes(), &env);
        assert_eq!(code, expect, "{target}");
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
