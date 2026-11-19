//! Issue 129: `makiba hook deny-base` refuses a redirection written against the name inside a
//! quoted nested command, and still passes a command that only mentions the name next to a
//! redirection as an argument of an unrelated program.

use std::io::Write;
use std::process::{Command, Stdio};

/// The hook's exit code for a Bash `command`.
fn hook(command: &str) -> i32 {
    let payload = serde_json::json!({"tool_name": "Bash", "tool_input": {"command": command}});
    let mut child = Command::new(env!("CARGO_BIN_EXE_makiba"))
        .args(["hook", "deny-base"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn makiba");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    child.wait().expect("wait for makiba").code().unwrap_or(-1)
}

#[test]
fn a_redirection_against_the_name_in_a_quoted_nested_command_is_refused() {
    let name = &makiba::base::worker_denies().commands[0];
    for c in [
        format!("bash -c '{name}>/dev/null'"),
        format!("sh -c \"{name}</dev/null\""),
        format!("bash -c '/opt/bin/{name}>out 2>&1'"),
    ] {
        assert_eq!(hook(&c), 2, "{c}");
    }
}

#[test]
fn the_name_next_to_a_redirection_as_an_argument_of_another_program_passes() {
    let name = &makiba::base::worker_denies().commands[0];
    for c in [
        format!("echo {name}>out"),
        format!("echo '{name}>out'"),
        format!("grep {name}<in"),
        format!("cat <in {name}.md"),
    ] {
        assert_eq!(hook(&c), 0, "{c}");
    }
}
