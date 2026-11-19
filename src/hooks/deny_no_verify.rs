//! `makiba hook deny-no-verify` (M4-3): a `PreToolUse` hook on Bash that refuses a git command
//! that skips the repository's hooks (inventory HK-2, lines L-K3 and L-K12). Ported from
//! claude-org-ja's `.hooks/block-no-verify.sh`:
//!
//! - `--no-verify` on `commit`, `push`, `merge`, `pull` or `am`;
//! - `-n` on `commit`, alone or inside a bundle of short options;
//! - `HUSKY=`, `SKIP_SECRET_SCAN=` or `NO_VERIFY=` beside one of those commands;
//! - `git -c core.hooksPath=...` or `git -c hook.*` beside one of them.
//!
//! The command is read as `deny-dangerous-git` reads it (see `shell`).

use super::deny_dangerous_git::has_git_subcmd;
use super::re::Re;
use super::shell;
use serde_json::Value;

fn has(text: &str, pattern: &str) -> bool {
    Re::new(pattern).any_line(text)
}

fn icase(text: &str, pattern: &str) -> bool {
    Re::icase(pattern).any_line(text)
}

pub fn check(payload: &Value) -> Result<(), String> {
    let command = super::command(payload);
    if command.is_empty() {
        return Ok(());
    }
    let (segments, assigns) = shell::segments_and_assignments(&command);
    for segment in segments.iter().filter(|s| !s.is_empty()) {
        let flat = shell::flat_segment(segment, &assigns);
        let subs = ["commit", "push", "merge", "pull", "am"].map(|s| has_git_subcmd(&flat, s));
        if !subs.iter().any(|&s| s) {
            continue;
        }
        if has(&flat, r"(^|[[:space:]])--no-verify([[:space:]]|$)") {
            let which = ["commit", "push", "merge", "pull", "am"]
                .iter()
                .zip(subs)
                .find(|(_, s)| *s)
                .map(|(n, _)| *n)
                .unwrap_or("am");
            return Err(format!(
                "git {which} --no-verify skips the repository's hooks (the secret scanner among them); run it without the flag"
            ));
        }
        if subs[0]
            && (has(&flat, r"(^|[[:space:]])-n([[:space:]]|$)")
                || has(&flat, r"(^|[[:space:]])-[a-zA-Z]*n[a-zA-Z]+([[:space:]]|$)")
                || has(&flat, r"(^|[[:space:]])-[a-zA-Z]+n([[:space:]]|$)"))
        {
            return Err(
                "git commit -n is --no-verify; it skips the repository's hooks. Run it without the flag".into(),
            );
        }
    }
    let mut whole = shell::plain_git(&shell::flatten_text(&command));
    if !assigns.is_empty() {
        whole = shell::expand(&whole, &assigns);
    }
    let any_sub = r"(commit|push|merge|pull|am)";
    if has(
        &whole,
        &format!(r"(^|[[:space:]])git[[:space:]]+{any_sub}([[:space:]]|$)"),
    ) || has(
        &whole,
        &format!(r"(^|[[:space:]])git[[:space:]].*[[:space:]]{any_sub}([[:space:]]|$)"),
    ) {
        for var in ["HUSKY", "SKIP_SECRET_SCAN", "NO_VERIFY"] {
            if has(&whole, &format!(r"(^|[[:space:]]){var}=")) {
                return Err(format!(
                    "{var}=... switches the repository's hooks off; run the command without it"
                ));
            }
        }
    }
    if (has(&whole, r"(^|[[:space:]])git[[:space:]]+-c[[:space:]]")
        || has(
            &whole,
            r"(^|[[:space:]])git[[:space:]].*[[:space:]]-c[[:space:]]",
        ))
        && has(&whole, &format!(r"(^|[[:space:]]){any_sub}([[:space:]]|$)"))
    {
        if icase(&whole, r"(^|[[:space:]])-c[[:space:]]+core\.hookspath=") {
            return Err("git -c core.hooksPath=... switches the repository's hooks off".into());
        }
        if icase(&whole, r"(^|[[:space:]])-c[[:space:]]+hook\.") {
            return Err("git -c hook.* overrides the repository's hooks".into());
        }
    }
    Ok(())
}
