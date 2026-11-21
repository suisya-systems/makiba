//! Issue 136, review round 1: `git am` runs `applypatch-msg` for the message, not `commit-msg`,
//! so a patch whose files are plain but whose message holds a token is refused by the scanner's
//! hooks too. The token-shaped text is joined from two halves at run time.

use std::path::Path;
use std::process::{Command, Output, Stdio};

const MAKIBA: &str = env!("CARGO_BIN_EXE_makiba");

fn git_out(dir: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = git_out(dir, args);
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

#[test]
fn git_am_of_a_patch_with_a_token_in_its_message_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let (src, dst) = (root.join("src"), root.join("dst"));
    for r in [&src, &dst] {
        std::fs::create_dir_all(r).unwrap();
        git(r, &["init", "-q", "-b", "main"]);
        git(r, &["commit", "-q", "--allow-empty", "-m", "first"]);
    }
    // The patch: a plain file, a token-shaped message, from a repository with no scanner.
    let token = format!("{}{}", "AKIA", "QWERTYUIOPASDFG7");
    std::fs::write(src.join("plain.txt"), "plain\n").unwrap();
    git(&src, &["add", "plain.txt"]);
    git(
        &src,
        &["commit", "-q", "-m", &format!("subject\n\nbody {token}")],
    );
    let patch = root.join("p.patch");
    let text = git(&src, &["format-patch", "-1", "--stdout"]);
    std::fs::write(&patch, format!("{text}\n")).unwrap();

    let hooks = root.join("hooks");
    makiba::hooks::git_hook::install(&hooks, Path::new(MAKIBA)).unwrap();
    git(
        &dst,
        &["config", "core.hooksPath", &hooks.to_string_lossy()],
    );
    let before = git(&dst, &["rev-parse", "HEAD"]);
    let out = git_out(&dst, &["am", "-q", &patch.to_string_lossy()]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "git am passed a token in the message"
    );
    assert!(err.contains("commit message"), "{err}");
    assert_eq!(git(&dst, &["rev-parse", "HEAD"]), before);
}
