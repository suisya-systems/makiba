//! Claude Code and the agents makiba launches. Everything that differs between agent programs
//! lives in one module per program behind the trait `Agent` (spec section 9). `fake` is the test
//! agent; `claude_code` is added in M4.

use std::io;
use std::path::{Path, PathBuf};

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

/// A startup dialog the agent is allowed to answer (spec section 7): how to recognize it, and the
/// whole option line to choose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogPolicy {
    pub name: &'static str,
    pub recognize: &'static str,
    pub choose: &'static str,
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

/// The module named `name` (a `run_launch.agent`). `claude_code` is added in M4.
pub fn by_name(name: &str) -> Option<Box<dyn Agent>> {
    // Without the seam the test agent has no program: it can write settings, not be launched.
    (name == "fake").then(|| {
        Box::new(
            fake_from_env().unwrap_or_else(|| fake::Fake::new(Path::new(""), Path::new(""), &[])),
        ) as _
    })
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

pub trait Agent: Send + Sync {
    /// `"claude_code"` or `"fake"`.
    fn name(&self) -> &'static str;
    /// Full argv for the agent program, with the door's address and the agent's token. Unsetting
    /// variables is the base's job.
    fn argv(&self, launch: &AgentLaunch) -> Vec<String>;
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
    /// The id to pass back as `resume`, read after the first turn.
    fn conversation_id(&self, worktree: &Path) -> Option<String>;
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
