//! Claude Code and the agents makiba launches. Everything that differs between agent programs
//! lives in one module per program behind the trait `Agent` (spec section 9). `fake` is the test
//! agent; `claude_code` is the interactive `claude` program.

use std::io;
use std::path::{Path, PathBuf};

pub mod claude_code;
pub mod fake;
pub mod version;

/// Who an agent is for (spec section 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Secretary,
    Worker,
    Curator,
    Person,
}

/// How an agent reaches the door (spec section 3): the address and the agent's token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoorAccess {
    pub address: String,
    pub token: String,
}

pub struct AgentLaunch<'a> {
    pub role: Role,
    pub brief: Option<&'a str>,
    pub rules_file: &'a Path,
    pub settings_file: &'a Path,
    pub model: Option<&'a str>,
    pub resume: Option<&'a str>,
    pub door: &'a DoorAccess,
}

/// A startup dialog the agent is allowed to answer (spec section 7): it is recognized by all of
/// its option lines, each matched whole, and `choose` is the one of them to choose. `status` is
/// the message of the blocked permission status the base reports while it is shown (`None`: it
/// is shown with no status).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogPolicy {
    pub name: &'static str,
    pub options: &'static [&'static str],
    pub choose: &'static str,
    pub status: Option<&'static str>,
}

/// A permission prompt as `makiba answer` shows it: its text, and the keys that answer it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptView {
    pub text: String,
    pub yes_key: &'static str,
    pub no_key: &'static str,
}

/// What a task's settings file carries (spec section 9). M4 grows it for `claude_code`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskSettings {
    pub write_dirs: Vec<PathBuf>,
    /// Folders and files denied for read and write (M4-2): the base's socket and state folders,
    /// the person key file and `state.db`, resolved when the settings are written.
    pub deny_dirs: Vec<PathBuf>,
    pub deny_files: Vec<PathBuf>,
    /// Folders denied for write only: the share folder as resolved, whose entry point the role
    /// hooks run (the role's settings name only its default place).
    pub deny_write_dirs: Vec<PathBuf>,
    /// Files denied for write only: the worktree's `.mcp.json`, inside the write surface (Issue
    /// 137).
    pub deny_write_files: Vec<PathBuf>,
    /// The role whose `roles/<role>/settings.json` the file is written on top of (Issue 134).
    pub role: Option<Role>,
}

/// The role files of `roles/`, carried in the binary: the rules and the settings (Issue 134).
fn role_files(role: Role) -> Option<(&'static str, &'static str)> {
    match role {
        Role::Worker => Some((
            include_str!("../../roles/worker/rules.md"),
            include_str!("../../roles/worker/settings.json"),
        )),
        Role::Secretary => Some((
            include_str!("../../roles/secretary/rules.md"),
            include_str!("../../roles/secretary/settings.json"),
        )),
        Role::Curator => Some((
            include_str!("../../roles/curator/rules.md"),
            include_str!("../../roles/curator/settings.json"),
        )),
        Role::Person => None,
    }
}

/// Where a role's rules file is read by its agent: `<share>/roles/<role>/rules.md` (spec
/// section 2), outside every workplace.
pub fn rules_path(role: Role) -> PathBuf {
    let name = match role {
        Role::Secretary => "secretary",
        Role::Worker => "worker",
        Role::Curator => "curator",
        Role::Person => "person",
    };
    crate::cli::share_dir()
        .join("roles")
        .join(name)
        .join("rules.md")
}

/// Write the role's rules file from the binary unless it already holds them.
pub fn write_rules(role: Role) -> io::Result<()> {
    let Some((rules, _)) = role_files(role) else {
        return Ok(());
    };
    let path = rules_path(role);
    if std::fs::read_to_string(&path).is_ok_and(|s| s == rules) {
        return Ok(());
    }
    std::fs::create_dir_all(path.parent().unwrap_or(&path))?;
    std::fs::write(path, rules)
}

/// A path as a permission rule names it: `~/` as is, an absolute path in the `//` form.
fn rule_path(p: &Path) -> String {
    let s = p.display().to_string();
    if s.starts_with('/') {
        format!("/{s}")
    } else {
        s
    }
}

/// What the prompt line of an agent's screen shows (spec section 7, the wake line).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Composer {
    /// The prompt line is there and nothing is typed on it.
    Empty,
    /// Something is typed on it, possibly over several lines.
    Input,
    /// The screen is not understood: nothing is typed into it.
    Unknown,
}

/// The test seam that gives the test agent's command line: a JSON array of its program, its
/// record file and its steps (spec section 2).
pub const FAKE_AGENT_ENV: &str = "MAKIBA_FAKE_AGENT";

/// The test agent of `MAKIBA_FAKE_AGENT`; `None` when it is unset or not such an array.
fn fake_from_env() -> Option<fake::Fake> {
    let v: Vec<String> = serde_json::from_str(&std::env::var(FAKE_AGENT_ENV).ok()?).ok()?;
    let (program, rest) = v.split_first()?;
    let (record, steps) = rest.split_first()?;
    let steps: Vec<&str> = steps.iter().map(String::as_str).collect();
    Some(fake::Fake::new(
        Path::new(program),
        Path::new(record),
        &steps,
    ))
}

/// `<share>/makiba`, the entry point the role hooks run (spec section 2): a link to this binary.
/// A link to another file is pointed here; a file that is not a link is the person's and is kept.
pub fn link_entry_point() -> io::Result<()> {
    let exe = std::env::current_exe()?;
    let link = crate::cli::share_dir().join("makiba");
    match std::fs::symlink_metadata(&link) {
        Ok(m) if !m.file_type().is_symlink() => return Ok(()),
        Ok(_) if std::fs::read_link(&link).is_ok_and(|t| t == exe) => return Ok(()),
        Ok(_) => std::fs::remove_file(&link)?,
        Err(_) => std::fs::create_dir_all(crate::cli::share_dir())?,
    }
    std::os::unix::fs::symlink(exe, link)
}

/// `name` as `PATH` finds it, as `makiba up` runs it, made absolute: the agent starts in its
/// workplace, where a relative entry of `PATH` would name another file.
pub fn on_path(name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    std::env::var_os("PATH")
        .iter()
        .flat_map(std::env::split_paths)
        .map(|d| d.join(name))
        .find(|p| {
            std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
        .and_then(|p| std::path::absolute(p).ok())
}

/// The conversation id makiba names for the launch `spawn` (Issue 41): a version 4 UUID made from
/// the SHA-256 of the spawn id, so it needs no store until the launch is verified.
pub fn session_id(spawn: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut b: [u8; 16] = Sha256::digest(spawn.as_bytes())[..16]
        .try_into()
        .expect("16 bytes");
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    )
}

/// The module named `name` (a `run_launch.agent`).
pub fn by_name(name: &str) -> Option<Box<dyn Agent>> {
    match name {
        // Without the seam the test agent has no program: it can write settings, not be launched.
        "fake" => {
            Some(Box::new(fake_from_env().unwrap_or_else(|| {
                fake::Fake::new(Path::new(""), Path::new(""), &[])
            })))
        }
        // ponytail: `secretary.remote_control` has no store yet and only workers are launched.
        "claude_code" => Some(Box::new(claude_code::ClaudeCode::new(
            &on_path("claude").unwrap_or_else(|| PathBuf::from("claude")),
            &std::env::current_exe().unwrap_or_else(|_| PathBuf::from("makiba")),
            false,
        ))),
        _ => None,
    }
}

/// Whether a launch of `name` can be started: the test agent only when `MAKIBA_FAKE_AGENT` gives
/// it a program, so the binary on the stand-in stops at `settings_ready` (M3-2's tests).
pub fn launchable(name: &str) -> bool {
    name != "fake" || fake_from_env().is_some()
}

/// The MCP configuration an agent is launched with (spec section 4): the door's tools at
/// `address`, with the token as the bearer the door reads.
pub fn mcp_config(door: &DoorAccess) -> String {
    serde_json::json!({"mcpServers": {"makiba": {
        "type": "http",
        "url": format!("http://{}{}", door.address, crate::daemon::door::MCP_PATH),
        "headers": {"Authorization": format!("Bearer {}", door.token)},
    }}})
    .to_string()
}

/// The per-task settings file both modules write: the role's settings when `task` names a role,
/// with sandbox on and no way around it, the write surface of `task`, and its denies added, for
/// Bash by the sandbox and for the Read and Edit tools (which run outside it) by permission
/// rules. One file, passed as `--settings`: the form probe P7 measured under
/// `--setting-sources project,local`. No Unix socket path is allowed: it does nothing on Linux
/// (P1); agents reach `makibad` by the MCP configuration (M3-4).
fn write_sandbox_settings(path: &Path, task: &TaskSettings) -> io::Result<()> {
    let shown =
        |ps: &[PathBuf]| -> Vec<String> { ps.iter().map(|p| p.display().to_string()).collect() };
    let denied: Vec<String> = shown(&task.deny_dirs)
        .into_iter()
        .chain(shown(&task.deny_files))
        .collect();
    let targets = task
        .deny_dirs
        .iter()
        .map(|d| format!("{}/**", rule_path(d)))
        .chain(task.deny_files.iter().map(|f| rule_path(f)));
    let rules: Vec<String> = targets
        .flat_map(|t| [format!("Read({t})"), format!("Edit({t})")])
        .chain(
            task.deny_write_dirs
                .iter()
                .map(|d| format!("Edit({}/**)", rule_path(d))),
        )
        .chain(
            task.deny_write_files
                .iter()
                .map(|f| format!("Edit({})", rule_path(f))),
        )
        .collect();
    let mut settings: serde_json::Value = match task.role.and_then(role_files) {
        Some((_, role)) => serde_json::from_str(role).map_err(io::Error::other)?,
        None => serde_json::json!({}),
    };
    // Each list keeps the role's entries and gains the task's that it does not hold yet.
    let add = |list: &mut serde_json::Value, more: Vec<String>| {
        if !list.is_array() {
            *list = serde_json::json!([]);
        }
        let list = list.as_array_mut().expect("an array");
        for m in more {
            if !list.iter().any(|v| v.as_str() == Some(&m)) {
                list.push(m.into());
            }
        }
    };
    add(&mut settings["permissions"]["deny"], rules);
    let sandbox = &mut settings["sandbox"];
    sandbox["enabled"] = true.into();
    sandbox["failIfUnavailable"] = true.into();
    sandbox["allowUnsandboxedCommands"] = false.into();
    let fs = &mut sandbox["filesystem"];
    fs["allowWrite"] = shown(&task.write_dirs).into();
    add(&mut fs["denyRead"], denied.clone());
    add(&mut fs["denyWrite"], denied);
    add(&mut fs["denyWrite"], shown(&task.deny_write_dirs));
    add(&mut fs["denyWrite"], shown(&task.deny_write_files));
    std::fs::write(path, format!("{settings}\n"))
}

pub trait Agent: Send + Sync {
    /// `"claude_code"` or `"fake"`.
    fn name(&self) -> &'static str;
    /// Full argv for the agent program, with the door's address and the agent's token. Unsetting
    /// variables is the base's job.
    fn argv(&self, launch: &AgentLaunch) -> Vec<String>;
    /// As `argv`, naming the conversation `session` of a first launch (Issue 41) when the program
    /// takes one; a resume (`launch.resume`) is never given one.
    fn argv_with_session(&self, launch: &AgentLaunch, _session: Option<&str>) -> Vec<String> {
        self.argv(launch)
    }
    /// The policy list of spec section 7.
    fn startup_dialogs(&self) -> &[DialogPolicy];
    /// For a blocked terminal: the prompt's text, normalized, and its two keys.
    fn recognize_prompt(&self, screen: &[String]) -> Option<PromptView>;
    /// The prompt line of `screen` (the last lines of the terminal). Anything not understood is
    /// `Unknown`, and nothing is typed into it.
    fn composer(&self, _screen: &[String]) -> Composer {
        Composer::Unknown
    }
    /// The per-task settings file: sandbox write surface, the door, denies.
    fn write_task_settings(&self, path: &Path, task: &TaskSettings) -> io::Result<()>;
    /// The id to pass back as `resume`, read after the first turn; `session` is the one makiba
    /// named at launch (`session_id`).
    fn conversation_id(&self, worktree: &Path, session: &str) -> Option<String>;
    /// True when spec section 8's wrapper is safe to type as is.
    fn wraps_delivery(&self) -> bool;
    /// True when this program can take a channel push; when false a delivery goes by the wake
    /// line and the box.
    fn has_channel(&self) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The configuration names the door's MCP path and carries the token as the bearer the door
    /// reads (M3-3).
    #[test]
    fn the_mcp_configuration_names_the_door_and_carries_the_token() {
        let door = DoorAccess {
            address: "127.0.0.1:47317".into(),
            token: "abc".into(),
        };
        let v: serde_json::Value = serde_json::from_str(&mcp_config(&door)).unwrap();
        let s = &v["mcpServers"]["makiba"];
        assert_eq!(s["type"], "http");
        assert_eq!(s["url"], "http://127.0.0.1:47317/mcp");
        assert_eq!(s["headers"]["Authorization"], "Bearer abc");
    }
}
