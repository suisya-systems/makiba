//! Claude Code: the startup-dialog policy list (spec section 7, Issue M3-5) and the `Agent` for
//! the interactive `claude` program (spec section 9, Issue 114).

use super::{Agent, AgentLaunch, Composer, DialogPolicy, PromptView, Role, TaskSettings};
use std::io;
use std::path::{Path, PathBuf};

/// The channel server's name in the MCP configuration; P10 recorded the flag naming it.
const CHANNEL_SERVER: &str = "makiba-channel";

/// The policy list of spec section 7, as P10 recorded the dialogs on Claude Code 2.1.326 and the
/// statuses shown with them on 2.1.327 (its second run). Changed only by a reviewed commit.
pub const DIALOGS: [DialogPolicy; 2] = [
    DialogPolicy {
        name: "folder-trust",
        options: &["No, exit", "Yes, I trust this folder"],
        choose: "Yes, I trust this folder",
        status: Some("trust this folder"),
    },
    DialogPolicy {
        name: "dev-channel",
        options: &["I am using this for local development", "Exit"],
        choose: "I am using this for local development",
        status: None,
    },
];

pub struct ClaudeCode {
    program: PathBuf,
    /// The `makiba` binary, run as `makiba channel` for the channel server.
    makiba: PathBuf,
    /// `secretary.remote_control`: only the secretary's argv ever carries the option.
    remote_control: bool,
}

impl ClaudeCode {
    pub fn new(program: &Path, makiba: &Path, remote_control: bool) -> ClaudeCode {
        ClaudeCode {
            program: program.to_path_buf(),
            makiba: makiba.to_path_buf(),
            remote_control,
        }
    }
}

impl Agent for ClaudeCode {
    fn name(&self) -> &'static str {
        "claude_code"
    }

    /// The options are ordered so that the task is the last argument (P5): `--mcp-config` takes
    /// several values and `--remote-control` an optional one, so neither stands just before it.
    fn argv(&self, launch: &AgentLaunch) -> Vec<String> {
        let door = launch.door;
        let mut config: serde_json::Value =
            serde_json::from_str(&super::mcp_config(door)).expect("mcp_config is JSON");
        // The channel server's own arguments carry the token too (P11: out of a worker's reach).
        config["mcpServers"][CHANNEL_SERVER] = serde_json::json!({
            "command": self.makiba.display().to_string(),
            "args": ["channel", "--door", door.address, "--token", door.token],
        });
        let mut argv = vec![
            self.program.display().to_string(),
            "--mcp-config".into(),
            config.to_string(),
            "--dangerously-load-development-channels".into(),
            format!("server:{CHANNEL_SERVER}"),
        ];
        if launch.role == Role::Secretary && self.remote_control {
            argv.push("--remote-control".into());
        }
        // P7: nothing of the person's own settings reaches a worker or the curator. The
        // secretary keeps them (M6-2): it is the person's own session, as in paddock.
        // Workers and the curator start in auto permission mode (M4-3, paddock's templates), so
        // most permission prompts never appear. The mode is an option because Claude Code ignores
        // `defaultMode: "auto"` in a project settings file (paddock inventory, PRM-2).
        if matches!(launch.role, Role::Worker | Role::Curator) {
            argv.extend(["--setting-sources".into(), "project,local".into()]);
            argv.extend(["--permission-mode".into(), "auto".into()]);
        }
        argv.extend([
            "--append-system-prompt-file".into(),
            launch.rules_file.display().to_string(),
        ]);
        if let Some(model) = launch.model {
            argv.extend(["--model".into(), model.into()]);
        }
        if let Some(id) = launch.resume {
            argv.extend(["--resume".into(), id.into()]);
        }
        argv.extend([
            "--settings".into(),
            launch.settings_file.display().to_string(),
        ]);
        argv.extend(launch.brief.map(String::from));
        argv
    }

    /// A first launch names its conversation with `--session-id` (Issue 41); a resume already
    /// names it with `--resume`. Put right after the program, never just before the task.
    fn argv_with_session(&self, launch: &AgentLaunch, session: Option<&str>) -> Vec<String> {
        let mut argv = self.argv(launch);
        if let (None, Some(id)) = (launch.resume, session) {
            argv.splice(1..1, ["--session-id".to_string(), id.to_string()]);
        }
        argv
    }

    fn startup_dialogs(&self) -> &[DialogPolicy] {
        &DIALOGS
    }

    // ponytail: Claude Code's permission prompt is not recognized until its text is measured on
    // the pinned version; an unrecognized prompt is not answered.
    fn recognize_prompt(&self, _screen: &[String]) -> Option<PromptView> {
        None
    }

    fn composer(&self, _screen: &[String]) -> Composer {
        Composer::Unknown
    }

    fn write_task_settings(&self, path: &Path, task: &TaskSettings) -> io::Result<()> {
        super::write_sandbox_settings(path, task)
    }

    /// The session id makiba gave it at launch (`--session-id`).
    fn conversation_id(&self, _worktree: &Path, session: &str) -> Option<String> {
        Some(session.to_string())
    }

    fn wraps_delivery(&self) -> bool {
        true
    }

    fn has_channel(&self) -> bool {
        true
    }
}
