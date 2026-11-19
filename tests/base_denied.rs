//! M4-1: no role reaches the base by its command. Every role's settings deny what
//! `base::worker_denies()` lists, the person key file and an unsandboxed run (a shape test, as
//! paddock's `tests/worker-settings-*.test.sh`), and the hook `makiba hook deny-base` refuses the
//! base's command and fails closed on input it cannot read.

use serde_json::Value;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

const ROLES: [&str; 3] = ["secretary", "worker", "curator"];
/// The person key file's default place (spec section 2), as the role settings name it.
const PERSON_KEY: &str = "~/.config/makiba/person.key";

fn role_settings(role: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("roles/{role}/settings.json"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{} is not JSON: {e}", path.display()))
}

fn strings<'a>(v: &'a Value, pointer: &str) -> Vec<&'a str> {
    v.pointer(pointer)
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

/// The hook's exit code for `stdin`.
fn hook(stdin: &[u8]) -> i32 {
    let mut child = Command::new(env!("CARGO_BIN_EXE_makiba"))
        .args(["hook", "deny-base"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn makiba");
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    child.wait().expect("wait for makiba").code().unwrap_or(-1)
}

fn bash(command: &str) -> Vec<u8> {
    serde_json::json!({"tool_name": "Bash", "tool_input": {"command": command}})
        .to_string()
        .into_bytes()
}

#[test]
fn every_role_settings_denies_the_base_the_person_key_and_an_unsandboxed_run() {
    let denies = makiba::base::worker_denies();
    assert!(!denies.commands.is_empty() && !denies.folders.is_empty());
    for role in ROLES {
        let s = role_settings(role);
        let deny = strings(&s, "/permissions/deny");
        let deny_read = strings(&s, "/sandbox/filesystem/denyRead");
        let deny_write = strings(&s, "/sandbox/filesystem/denyWrite");
        for c in &denies.commands {
            let rule = format!("Bash({c}:*)");
            assert!(deny.contains(&rule.as_str()), "{role}: no {rule}");
        }
        let folders: Vec<String> = denies
            .folders
            .iter()
            .map(|f| f.display().to_string())
            .collect();
        let files = folders
            .iter()
            .map(|f| (f.clone(), format!("{f}/**")))
            .chain([(PERSON_KEY.to_string(), PERSON_KEY.to_string())]);
        for (path, pattern) in files {
            for rule in [format!("Read({pattern})"), format!("Edit({pattern})")] {
                assert!(deny.contains(&rule.as_str()), "{role}: no {rule}");
            }
            assert!(
                deny_read.contains(&path.as_str()),
                "{role}: {path} not in denyRead"
            );
            assert!(
                deny_write.contains(&path.as_str()),
                "{role}: {path} not in denyWrite"
            );
        }
        assert_eq!(
            s.pointer("/sandbox/allowUnsandboxedCommands"),
            Some(&Value::Bool(false)),
            "{role}: an unsandboxed run is not refused"
        );
    }
}

#[test]
fn the_hook_refuses_the_command_bare_by_path_and_behind_env_and_passes_a_mention() {
    for c in makiba::base::worker_denies().commands {
        for refused in [
            format!("{c} list"),
            format!("/usr/local/bin/{c} list"),
            format!("env FOO=1 {c} list"),
        ] {
            assert_eq!(hook(&bash(&refused)), 2, "not refused: {refused}");
        }
        let mention = format!("grep -rn {c} docs");
        assert_eq!(hook(&bash(&mention)), 0, "refused a mention: {mention}");
    }
}

#[test]
fn the_hook_refuses_empty_and_malformed_input() {
    for input in [
        &b""[..],
        b"not json",
        br#"{"tool_name": "Bash", "tool_input": {"command": "ls""#,
        br#"{"tool_name": "Bash", "tool_input": {}}"#,
    ] {
        assert_eq!(
            hook(input),
            2,
            "not refused: {:?}",
            String::from_utf8_lossy(input)
        );
    }
}
