//! The `Agent` for the test agent, `makiba-fake-agent` (source: tests/common/fake_agent.rs). The
//! program follows the script given on its command line; see that file for the steps.

use super::{Agent, AgentLaunch, DialogPolicy, PromptView, TaskSettings};
use std::io;
use std::path::{Path, PathBuf};

const DIALOGS: [DialogPolicy; 1] = [DialogPolicy {
    name: "folder-trust",
    recognize: "Do you trust the files in this folder?",
    choose: "Yes, I trust this folder",
}];

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

    fn argv(&self, _launch: &AgentLaunch) -> Vec<String> {
        let mut argv = vec![
            self.program.display().to_string(),
            "--record".into(),
            self.record.display().to_string(),
        ];
        argv.extend(self.script.iter().cloned());
        argv
    }

    fn startup_dialogs(&self) -> &[DialogPolicy] {
        &DIALOGS
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

    fn write_task_settings(&self, path: &Path, _task: &TaskSettings) -> io::Result<()> {
        std::fs::write(path, "{}\n")
    }

    fn conversation_id(&self, _worktree: &Path) -> Option<String> {
        None
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
}
