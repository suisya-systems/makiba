//! `makiba hook worker-boundary` (M4-3): a `PreToolUse` hook on the file tools (Edit, Write,
//! MultiEdit, NotebookEdit) that lets them write only inside the agent's own folder and
//! `~/.claude/plans/`, and never the Claude Code settings or MCP configuration in that folder
//! (inventory HK-6, lines L-K4 and L-K5). Ported from claude-org-ja's
//! `.hooks/check-worker-boundary.sh` with the cases of its test.
//!
//! The folder is `CLAUDE_PROJECT_DIR`, which Claude Code gives its hooks: the folder the agent was
//! started in, a worker's worktree. Without it the hook refuses. Paths are compared after
//! resolving symbolic links and `..` (as `realpath -m`), so a path that climbs out is refused.
//! Unlike the original, nothing outside the folder takes a note: a worker hands its note in its
//! done report.
//!
//! The file tools only: a Bash command that writes is bounded by the sandbox's write surface.

use serde_json::Value;
use std::path::{Component, Path, PathBuf};

/// Files in the folder that configure the agent itself.
const LOCKED: &[&str] = &[
    ".claude/settings.json",
    ".claude/settings.local.json",
    ".mcp.json",
];

pub fn check(payload: &Value) -> Result<(), String> {
    let root = std::env::var("CLAUDE_PROJECT_DIR").unwrap_or_default();
    if root.is_empty() {
        return Err("CLAUDE_PROJECT_DIR is not set, so the agent's folder is not known".into());
    }
    let mut path = super::field(payload, "/tool_input/file_path");
    if path.is_empty() {
        path = super::field(payload, "/tool_input/notebook_path");
    }
    if path.is_empty() {
        return Err("the file tool's payload has no file_path".into());
    }
    let file = resolve(Path::new(&path));
    let root = resolve(Path::new(&root));
    if LOCKED.iter().any(|l| file == root.join(l)) {
        return Err(format!(
            "{path} configures this agent (its settings carry the guard hooks); ask the secretary instead"
        ));
    }
    if file.starts_with(&root) && file != root {
        return Ok(());
    }
    if let Some(home) = std::env::var_os("HOME") {
        let plans = resolve(&Path::new(&home).join(".claude/plans"));
        if file.starts_with(&plans) && file != plans {
            return Ok(());
        }
    }
    Err(format!(
        "{path} is outside this agent's folder {}; write only inside it",
        root.display()
    ))
}

/// `path` made absolute, with symbolic links of its existing part resolved and `.` and `..`
/// applied, as `realpath -m` does.
pub fn resolve(path: &Path) -> PathBuf {
    let mut out = if path.is_absolute() {
        PathBuf::from("/")
    } else {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
    };
    let mut exists = true;
    for c in path.components() {
        match c {
            Component::Normal(name) => {
                out.push(name);
                if exists {
                    match std::fs::canonicalize(&out) {
                        Ok(real) => out = real,
                        Err(_) => exists = out.symlink_metadata().is_ok(),
                    }
                }
            }
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
        }
    }
    out
}
