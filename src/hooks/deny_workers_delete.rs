//! `makiba hook deny-workers-delete` (M4-3): a `PreToolUse` hook on Bash that refuses a recursive
//! `rm` naming the folder of worktrees or of document folders (spec section 2), which hold other
//! tasks' work (inventory HK-4, line L-K6). Ported from claude-org-ja's
//! `.hooks/block-workers-delete.sh` with the cases of its test.
//!
//! The folders are `$MAKIBA_WORKERS_DIR` (default `~/makiba-workers`) and `$MAKIBA_DOCS_DIR`
//! (default `~/makiba-docs`). The command is refused when it has a recursive `rm` (`-r`, `-R`, in
//! a bundle, or `--recursive`) and names either folder anywhere: resolved, as written in the
//! variable, with `~/` or `$HOME/`, or with backslashes. Like the original it reads text, so a
//! path built by a variable or reached by a relative `cd` is not seen.

use super::re::Re;
use super::worker_boundary::resolve;
use serde_json::Value;
use std::path::{Path, PathBuf};

fn folder(var: &str, default: &str) -> (String, PathBuf) {
    let home = std::env::var("HOME").unwrap_or_default();
    let raw = std::env::var(var)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| format!("~/{default}"));
    let expanded = match raw.strip_prefix("~/") {
        Some(rest) => Path::new(&home).join(rest),
        None => PathBuf::from(&raw),
    };
    (raw, resolve(&expanded))
}

pub fn check(payload: &Value) -> Result<(), String> {
    if super::field(payload, "/tool_name") != "Bash" {
        return Ok(());
    }
    let command = super::command(payload);
    if command.is_empty() {
        return Ok(());
    }
    let recursive = Re::new(
        r"(^|[|&;()`$[:space:]])rm[[:space:]]+-[a-zA-Z]*[rR]|(^|[|&;()`$[:space:]])rm[[:space:]].*[[:space:]]-[a-zA-Z]*[rR]|(^|[|&;()`$[:space:]])rm[[:space:]].*--recursive",
    );
    // makiba's addition: `/bin/rm` is `rm` too.
    if !recursive.any_line(&super::shell::plain(&command, "rm")) {
        return Ok(());
    }
    let home = std::env::var("HOME").unwrap_or_default();
    for (raw, real) in [
        folder("MAKIBA_WORKERS_DIR", "makiba-workers"),
        folder("MAKIBA_DOCS_DIR", "makiba-docs"),
    ] {
        let real = real.display().to_string();
        let mut forms = vec![real.clone(), raw, real.replace('/', "\\")];
        if !home.is_empty()
            && let Some(rest) = real.strip_prefix(&format!("{home}/"))
        {
            forms.push(format!("~/{rest}"));
            forms.push(format!("$HOME/{rest}"));
            forms.push(format!("${{HOME}}/{rest}"));
        }
        if forms
            .iter()
            .any(|f| !f.is_empty() && command.split('\n').any(|l| l.contains(f.as_str())))
        {
            return Err(format!(
                "a recursive rm naming {real} could delete other tasks' work; a person deletes there by hand if it must go"
            ));
        }
    }
    Ok(())
}
