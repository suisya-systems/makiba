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
