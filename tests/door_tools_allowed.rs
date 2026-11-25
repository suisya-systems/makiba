//! Issue 292: a role's settings allow its own door tools, so the auto mode classifier does not
//! stand between an agent and `makibad`.
//!
//! The settings are the file the Claude Code module writes for a launch (as `prepare.rs` calls
//! it). For each role they allow each door tool of the role's verbs by its full name
//! (`mcp__makiba__<verb>`, spec section 12) and no door tool outside them, with no wildcard; the
//! role's denies and hooks are all still there.

use makiba::agents::{self, Role, TaskSettings};
use makiba::spec::Verb;
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::Path;

fn built(role: Option<Role>) -> Value {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("settings.json");
    let task = TaskSettings {
        role,
        write_dirs: vec!["/org/workplace".into()],
        deny_dirs: vec!["/org/base".into()],
        deny_files: vec!["/org/state.db".into()],
        deny_write_dirs: vec!["/org/share".into()],
        deny_write_files: vec![],
        allowed_hosts: None,
    };
    agents::by_name("claude_code")
        .unwrap()
        .write_task_settings(&file, &task)
        .unwrap();
    serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap()
}

fn strings(v: &Value) -> BTreeSet<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn full(tool: &str) -> String {
    format!("mcp__makiba__{tool}")
}

/// Spec section 4, the table "Verbs allowed", as far as a verb has a tool (section 12).
fn expected(role: Role) -> BTreeSet<String> {
    let names: &[&str] = match role {
        Role::Secretary => &[
            "dispatch",
            "close",
            "answer",
            "pr",
            "pending",
            "peek",
            "status",
            "send",
            "show",
            "renew",
            "handover",
            "inbox",
            "note",
            "push",
            "project",
            "update",
            "candidate",
            "goal",
            "range",
            "review",
            "merge",
        ],
        Role::Worker => &["report", "send", "show", "inbox", "review"],
        Role::Curator => &["report", "send", "show", "inbox"],
        Role::Person => &[],
    };
    names.iter().map(|n| full(n)).collect()
}

fn name_of(role: Role) -> &'static str {
    match role {
        Role::Secretary => "secretary",
        Role::Worker => "worker",
        Role::Curator => "curator",
        Role::Person => "person",
    }
}

const ROLES: [Role; 3] = [Role::Secretary, Role::Worker, Role::Curator];

/// 1. Each role's settings allow exactly the door tools of its verbs, by full name.
#[test]
fn each_roles_settings_allow_exactly_its_own_door_tools() {
    for role in ROLES {
        let v = built(Some(role));
        assert_eq!(
            strings(&v["permissions"]["allow"]),
            expected(role),
            "{}: {v}",
            name_of(role)
        );
    }
}

/// 2. No door tool outside the role's verbs is allowed: not a verb of another role, not a verb
///    that has no tool (`up`, `down`, `config`, `gate`, `events`).
#[test]
fn no_door_tool_outside_the_roles_verbs_is_allowed() {
    for role in ROLES {
        let allowed = strings(&built(Some(role))["permissions"]["allow"]);
        for verb in Verb::ALL {
            let name = full(verb.as_str());
            if expected(role).contains(&name) {
                assert!(allowed.contains(&name), "{}: {name} missing", name_of(role));
            } else {
                assert!(
                    !allowed.contains(&name),
                    "{} is allowed {name}",
                    name_of(role)
                );
            }
        }
    }
}

/// 3. Nothing but those names is allowed: no wildcard, no entry for the whole server, no other
///    server (the channel server included), no other kind of tool.
#[test]
fn the_allow_list_holds_nothing_wider() {
    for role in ROLES {
        let v = built(Some(role));
        for entry in strings(&v["permissions"]["allow"]) {
            assert!(!entry.contains('*'), "{}: {entry}", name_of(role));
            assert!(
                entry.starts_with("mcp__makiba__") && expected(role).contains(&entry),
                "{}: {entry} is not one of its door tools",
                name_of(role)
            );
        }
    }
}

/// 4. The role's own denies and hooks are all still there, and the sandbox is as it was.
#[test]
fn the_roles_denies_and_hooks_are_kept() {
    for role in ROLES {
        let name = name_of(role);
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("roles/{name}/settings.json"));
        let file: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let v = built(Some(role));
        let deny = strings(&v["permissions"]["deny"]);
        for rule in strings(&file["permissions"]["deny"]) {
            assert!(deny.contains(&rule), "{name}: the deny {rule} is gone");
        }
        assert_eq!(v["hooks"], file["hooks"], "{name}");
        assert_eq!(v["sandbox"]["enabled"], true, "{name}");
        assert_eq!(v["sandbox"]["allowUnsandboxedCommands"], false, "{name}");
    }
}

/// 5. Settings of no role carry no allow list.
#[test]
fn settings_of_no_role_allow_nothing() {
    assert!(strings(&built(None)["permissions"]["allow"]).is_empty());
}
