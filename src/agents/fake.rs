//! The `Agent` for the test agent, `makiba-fake-agent` (source: tests/common/fake_agent.rs). The
//! program follows the script given on its command line; see that file for the steps.

use super::{Agent, AgentLaunch, Composer, DialogPolicy, PromptView, TaskSettings};
use std::io;
use std::path::{Path, PathBuf};

pub struct Fake {
    program: PathBuf,
    record: PathBuf,
    script: Vec<String>,
    channel: bool,
}

impl Fake {
    /// `program` is the binary `makiba-fake-agent`; it records what it receives in `record` and
    /// follows `script` (its steps, such as `working` or `blocked:permission`).
    pub fn new(program: &Path, record: &Path, script: &[&str]) -> Fake {
        Fake {
            program: program.to_path_buf(),
            record: record.to_path_buf(),
            script: script.iter().map(|s| s.to_string()).collect(),
            channel: false,
        }
    }

    /// Whether `has_channel` answers yes, so both delivery routes can be tested.
    pub fn with_channel(mut self, channel: bool) -> Fake {
        self.channel = channel;
        self
    }
}

impl Agent for Fake {
    fn name(&self) -> &'static str {
        "fake"
    }

    /// The program, its record file, the MCP configuration (the door and the token), the steps.
    fn argv(&self, launch: &AgentLaunch) -> Vec<String> {
        let mut argv = vec![
            self.program.display().to_string(),
            "--record".into(),
            self.record.display().to_string(),
            "--mcp-config".into(),
            super::mcp_config(launch.door),
        ];
        // A resume runs the steps file its conversation id names (in the workplace) before the
        // script: an id with no such file is a resume that fails (the agent exits at once).
        argv.extend(launch.resume.map(|id| format!("steps:{id}")));
        argv.extend(self.script.iter().cloned());
        argv
    }

    /// Claude Code's list: the test agent shows the dialogs Claude Code shows (M3-5).
    fn startup_dialogs(&self) -> &[DialogPolicy] {
        &super::claude_code::DIALOGS
    }

    fn recognize_prompt(&self, screen: &[String]) -> Option<PromptView> {
        let at = screen
            .iter()
            .rposition(|l| l.contains("Do you want to proceed?"))?;
        Some(PromptView {
            text: screen[at..].join("\n"),
            yes_key: "enter",
            no_key: "esc",
        })
    }

    /// The last line that begins with `>` is the prompt; a no-break space after it is nothing.
    fn composer(&self, screen: &[String]) -> Composer {
        match screen.iter().rev().find_map(|l| l.strip_prefix('>')) {
            None => Composer::Unknown,
            Some(rest) if rest.replace('\u{a0}', " ").trim().is_empty() => Composer::Empty,
            Some(_) => Composer::Input,
        }
    }

    /// Sandbox on, the write surface of `task`. No Unix socket path is allowed: it does nothing
    /// on Linux (P1); agents reach `makibad` by the MCP configuration (M3-4).
    fn write_task_settings(&self, path: &Path, task: &TaskSettings) -> io::Result<()> {
        super::write_sandbox_settings(path, task)
    }

    /// What `.fake-conversation` in the workplace says, if anything.
    fn conversation_id(&self, worktree: &Path, _session: &str) -> Option<String> {
        let id = std::fs::read_to_string(worktree.join(".fake-conversation")).ok()?;
        let id = id.trim();
        (!id.is_empty()).then(|| id.to_string())
    }

    fn wraps_delivery(&self) -> bool {
        true
    }

    fn has_channel(&self) -> bool {
        self.channel
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_prompt_on_the_screen_is_recognized_and_a_plain_screen_is_not() {
        let fake = Fake::new(Path::new("agent"), Path::new("rec"), &["prompt"]);
        let screen: Vec<String> = ["[fake] blocked", "Do you want to proceed?", "❯ 1. Yes"]
            .map(String::from)
            .into();
        let view = fake.recognize_prompt(&screen).unwrap();
        assert!(view.text.starts_with("Do you want to proceed?"));
        assert_eq!(fake.recognize_prompt(&screen[..1]), None);
    }

    #[test]
    fn task_settings_turn_the_sandbox_on_and_allow_no_socket() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("t.json");
        let task = TaskSettings {
            write_dirs: vec!["/w".into(), "/tmp".into()],
            ..Default::default()
        };
        Fake::new(Path::new("a"), Path::new("r"), &[])
            .write_task_settings(&file, &task)
            .unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap();
        assert_eq!(v["sandbox"]["enabled"], true);
        assert_eq!(
            v["sandbox"]["filesystem"]["allowWrite"],
            serde_json::json!(["/w", "/tmp"])
        );
        assert!(!v.to_string().contains("allowUnixSockets"));
    }
}
