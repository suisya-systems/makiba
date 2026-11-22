//! Issue 300, after the first diff review: the shapes through which the flag that skips git's hooks
//! reaches a shell without being git's own argument in the command line are refused by the
//! no-verify hook, and data that only names the flag still passes.
//!
//! - a here-string fed to a shell;
//! - a substitution inside a parameter expansion (`${v:-$(...)}`), and such an expansion where git
//!   reads its options;
//! - a here-document fed to a text program whose output reaches a shell later in the command: a
//!   pipe through other text programs, a group piped on, a file written and then run, a value kept
//!   and then evaluated.

use std::io::Write;
use std::process::{Command, Stdio};

fn hook(command: &str) -> i32 {
    let payload = serde_json::json!({"tool_name": "Bash", "tool_input": {"command": command}});
    let mut child = Command::new(env!("CARGO_BIN_EXE_makiba"))
        .args(["hook", "deny-no-verify"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn makiba");
    let _ = child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes());
    child.wait().expect("wait for makiba").code().unwrap_or(-1)
}

#[test]
fn the_flag_reaching_a_shell_by_another_way_is_refused() {
    let commands = [
        "bash <<< 'git commit -n -m x'",
        "sh <<< \"git commit --no-verify -m x\"",
        "unset v; echo \"${v:-$(git commit -n -m x)}\"",
        "echo ${v:-`git commit -n -m x`}",
        "git commit ${F:-\"-n\"} -m x",
        "cat <<'EOF' | cat | bash\ngit commit -n -m x\nEOF",
        "{ cat <<'EOF'; } | bash\ngit commit -n -m x\nEOF",
        "(cat <<'EOF') | sh\ngit commit -n -m x\nEOF",
        "cat > run.sh <<'EOF'\ngit commit -n -m x\nEOF\nbash run.sh",
        "X=$(cat <<'EOF'\ngit commit -n -m x\nEOF\n)\neval \"$X\"",
        "$(cat <<'EOF'\ngit commit -n -m x\nEOF\n)",
        // After the second diff review: a substitution inside arithmetic, a variable set
        // outside a substitution, and code given to another interpreter.
        "echo \"$(( $(git commit -n -m x >/dev/null) + 0 ))\"",
        "F=-n; echo \"$(git commit $F -m x)\"",
        "F=--no-verify; bash <<EOF\ngit commit $F -m x\nEOF",
        "python3 -c 'import os; os.system(\"git commit -n -m x\")'",
        "node -e \"require('child_process').execSync('git commit --no-verify -m x')\"",
    ];
    let passed: Vec<&str> = commands.into_iter().filter(|c| hook(c) != 2).collect();
    assert!(passed.is_empty(), "not refused: {passed:#?}");
}

#[test]
fn data_that_names_the_flag_still_passes() {
    let commands = [
        "grep -c x <<< 'git commit -n is --no-verify'",
        "echo \"${v:-bash -n}\"",
        "cat <<'EOF' | grep -c rule\nThe rule: git commit -n is --no-verify.\nEOF",
        "git commit -F - <<'EOF'\nci: bash -n\nEOF",
        // A message git keeps is data, whatever runs after it.
        "git commit -F - <<'EOF'\ndocs: git commit -n is --no-verify\nEOF\ncargo test",
        "git commit -F - <<'EOF'\ndocs: git commit -n is --no-verify\nEOF\nbash scripts/check.sh",
        "cat <<'EOF' > notes.md\ngit commit -n is refused\nEOF\nwc -l notes.md",
    ];
    let refused: Vec<&str> = commands.into_iter().filter(|c| hook(c) != 0).collect();
    assert!(refused.is_empty(), "refused: {refused:#?}");
}
