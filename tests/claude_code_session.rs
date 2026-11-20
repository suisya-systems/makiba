//! Issue 41: makiba names a Claude Code worker's conversation itself. A first launch carries
//! `--session-id` with the id `agents::session_id` makes from the launch's spawn id, and that id is
//! what is stored as the launch's conversation id; a resume carries `--resume` with the stored id
//! and no `--session-id`. The task stays the last argument (P5).

use makiba::agents::claude_code::ClaudeCode;
use makiba::agents::{self, Agent, AgentLaunch, DoorAccess, Role};
use std::path::Path;

const TASK: &str = "do the task";

fn argv(resume: Option<&str>, session: Option<&str>) -> Vec<String> {
    let door = DoorAccess {
        address: "127.0.0.1:47317".into(),
        token: "tok".into(),
    };
    ClaudeCode::new(Path::new("claude"), Path::new("/bin/makiba"), false).argv_with_session(
        &AgentLaunch {
            role: Role::Worker,
            brief: Some(TASK),
            rules_file: Path::new("/r/rules.md"),
            settings_file: Path::new("/s/task.json"),
            model: None,
            resume,
            door: &door,
        },
        session,
    )
}

fn value<'a>(argv: &'a [String], flag: &str) -> Option<&'a str> {
    let i = argv.iter().position(|a| a == flag)?;
    argv.get(i + 1).map(String::as_str)
}

#[test]
fn a_first_launch_names_its_conversation_and_that_name_is_stored() {
    let session = agents::session_id("spawn-1");
    let a = argv(None, Some(&session));
    assert_eq!(value(&a, "--session-id"), Some(session.as_str()));
    assert!(!a.iter().any(|x| x == "--resume"), "{a:?}");
    assert_eq!(a.last().map(String::as_str), Some(TASK));
    let stored = ClaudeCode::new(Path::new("claude"), Path::new("/bin/makiba"), false)
        .conversation_id(Path::new("/w"), &session);
    assert_eq!(stored.as_deref(), Some(session.as_str()));
}

#[test]
fn a_resume_names_the_stored_conversation_and_no_new_one() {
    let a = argv(Some("stored-id"), Some(&agents::session_id("spawn-2")));
    assert_eq!(value(&a, "--resume"), Some("stored-id"));
    assert!(!a.iter().any(|x| x == "--session-id"), "{a:?}");
    assert_eq!(a.last().map(String::as_str), Some(TASK));
}

/// The id is a version 4 UUID, the same for the same spawn id and another for another.
#[test]
fn the_session_id_is_a_uuid_of_the_spawn_id() {
    let id = agents::session_id("spawn-1");
    let parts: Vec<&str> = id.split('-').collect();
    assert_eq!(
        parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
        [8, 4, 4, 4, 12]
    );
    assert!(id.chars().all(|c| c == '-' || c.is_ascii_hexdigit()));
    assert!(parts[2].starts_with('4'), "{id}");
    assert!(matches!(&parts[3][..1], "8" | "9" | "a" | "b"), "{id}");
    assert_eq!(id, agents::session_id("spawn-1"));
    assert_ne!(id, agents::session_id("spawn-2"));
}
