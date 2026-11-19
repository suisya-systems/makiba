//! Issue 114: the argv of the `claude_code` agent module for a worker, the curator and the
//! secretary. The directions follow probes.md: `--setting-sources project,local` for a worker and
//! the curator (P7), the token in the arguments (P11), the channel flag as P10 recorded it, the
//! task as the last argument (P5). The Remote Control option is the secretary's, and only with
//! `secretary.remote_control` on (spec section 9).

use makiba::agents::claude_code::ClaudeCode;
use makiba::agents::{Agent, AgentLaunch, DoorAccess, Role};
use std::path::Path;

const TOKEN: &str = "tok-0123456789abcdef";
const TASK: &str = "do the task";

fn door() -> DoorAccess {
    DoorAccess {
        address: "127.0.0.1:47317".into(),
        token: TOKEN.into(),
    }
}

fn argv(agent: &ClaudeCode, role: Role, brief: Option<&str>) -> Vec<String> {
    agent.argv(&AgentLaunch {
        role,
        brief,
        rules_file: Path::new("/r/rules.md"),
        settings_file: Path::new("/s/task.json"),
        model: None,
        resume: None,
        door: &door(),
    })
}

fn agent(remote_control: bool) -> ClaudeCode {
    ClaudeCode::new(
        Path::new("claude"),
        Path::new("/bin/makiba"),
        remote_control,
    )
}

/// The value after `flag`.
fn value<'a>(argv: &'a [String], flag: &str) -> Option<&'a str> {
    let i = argv.iter().position(|a| a == flag)?;
    argv.get(i + 1).map(String::as_str)
}

#[test]
fn a_worker_is_isolated_from_the_persons_settings_and_gets_the_door() {
    let a = argv(&agent(true), Role::Worker, Some(TASK));
    assert_eq!(a[0], "claude");
    // P7.
    assert_eq!(value(&a, "--setting-sources"), Some("project,local"));
    // P5: the task is the last argument.
    assert_eq!(a.last().map(String::as_str), Some(TASK));
    // The per-task settings file and the role rules as an appended file.
    assert_eq!(value(&a, "--settings"), Some("/s/task.json"));
    assert_eq!(
        value(&a, "--append-system-prompt-file"),
        Some("/r/rules.md")
    );
    // P10: the channel flag, naming the channel server of the configuration.
    assert_eq!(
        value(&a, "--dangerously-load-development-channels"),
        Some("server:makiba-channel")
    );
    // P11: the token is in the arguments: the door's bearer and the channel server's own.
    let cfg: serde_json::Value = serde_json::from_str(value(&a, "--mcp-config").unwrap()).unwrap();
    let servers = &cfg["mcpServers"];
    assert_eq!(servers["makiba"]["url"], "http://127.0.0.1:47317/mcp");
    assert_eq!(
        servers["makiba"]["headers"]["Authorization"],
        format!("Bearer {TOKEN}")
    );
    assert_eq!(servers["makiba-channel"]["command"], "/bin/makiba");
    assert_eq!(
        servers["makiba-channel"]["args"],
        serde_json::json!(["channel", "--door", "127.0.0.1:47317", "--token", TOKEN])
    );
    // Remote Control is the secretary's: never a worker's, even with the setting on.
    assert!(!a.iter().any(|x| x.starts_with("--remote-control")));
}

#[test]
fn the_curator_is_isolated_like_a_worker() {
    let a = argv(&agent(true), Role::Curator, Some(TASK));
    assert_eq!(value(&a, "--setting-sources"), Some("project,local"));
    assert_eq!(a.last().map(String::as_str), Some(TASK));
    assert!(!a.iter().any(|x| x.starts_with("--remote-control")));
}

#[test]
fn the_secretary_keeps_the_settings_sources_and_remote_control_follows_the_setting() {
    // Spec section 9 and Issue 114: the secretary's settings sources are decided with M6-2, so
    // its argv carries no `--setting-sources` yet.
    let off = argv(&agent(false), Role::Secretary, None);
    assert!(!off.iter().any(|x| x == "--setting-sources"));
    assert!(!off.iter().any(|x| x.starts_with("--remote-control")));
    // Everything else of the door and the channel is the same.
    assert!(value(&off, "--mcp-config").unwrap().contains(TOKEN));
    assert_eq!(
        value(&off, "--dangerously-load-development-channels"),
        Some("server:makiba-channel")
    );

    let on = argv(&agent(true), Role::Secretary, None);
    assert!(!on.iter().any(|x| x == "--setting-sources"));
    assert!(on.iter().any(|x| x == "--remote-control"));
    // The option takes an optional value, so it is never the argument before the task.
    let i = on.iter().position(|x| x == "--remote-control").unwrap();
    assert!(i + 1 < on.len(), "something follows it, and it is no value");
    assert!(on[i + 1].starts_with("--"));
}

#[test]
fn the_token_is_in_the_arguments_and_the_module_has_no_environment_to_put_it_in() {
    // `Agent::argv` is all an agent module gives the launch; the environment of a launch is the
    // base's (spec section 6.4) and carries no key (section 4).
    for role in [Role::Worker, Role::Curator, Role::Secretary] {
        let a = argv(&agent(false), role, None);
        assert!(a.iter().any(|x| x.contains(TOKEN)), "{role:?}");
    }
}

#[test]
fn a_model_and_a_resume_are_passed_and_the_task_stays_last() {
    let launch = AgentLaunch {
        role: Role::Worker,
        brief: Some(TASK),
        rules_file: Path::new("/r/rules.md"),
        settings_file: Path::new("/s/task.json"),
        model: Some("m-1"),
        resume: Some("sess-1"),
        door: &door(),
    };
    let a = agent(false).argv(&launch);
    assert_eq!(value(&a, "--model"), Some("m-1"));
    assert_eq!(value(&a, "--resume"), Some("sess-1"));
    assert_eq!(a.last().map(String::as_str), Some(TASK));
}

#[test]
fn workers_and_the_curator_start_in_auto_permission_mode() {
    for role in [Role::Worker, Role::Curator] {
        let a = argv(&agent(false), role, Some(TASK));
        assert_eq!(value(&a, "--permission-mode"), Some("auto"), "{role:?}");
        assert_eq!(a.last().map(String::as_str), Some(TASK));
    }
    let s = argv(&agent(false), Role::Secretary, None);
    assert!(!s.iter().any(|x| x == "--permission-mode"));
}
