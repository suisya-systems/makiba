//! `makiba hook deny-dangerous-git` (M4-3): a `PreToolUse` hook on Bash that refuses git commands
//! that destroy history or uncommitted work (inventory HK-2, line L-K3). Ported from
//! claude-org-ja's `.hooks/block-dangerous-git.sh` with the cases of its test:
//!
//! - `push` with a plain force flag; `--force-with-lease` to a protected branch or to a target
//!   that cannot be read (none, `HEAD`, a wildcard, `--all`, `--mirror`, `--tags`, a ref outside
//!   `refs/heads/`);
//! - `reset --hard`, `branch -D` (or `--delete --force`), `clean` with a force flag,
//!   `checkout --`, `restore` from a source onto the worktree, `tag -d`, `update-ref -d` or
//!   `--stdin`, `reflog expire` or `delete` of everything;
//! - every `stash` that changes something (only `list` and `show` pass);
//! - `add` of everything (`-A`, `--all`, `.`).
//!
//! Each segment of the command (see `shell`) is checked with its known variables expanded and its
//! substitutions flattened, so a git command inside `eval`, `bash -c` or `$(...)` is seen too.
//! A word inside a quoted literal that reads like one of these is refused as well; that leans to
//! refusing, as the original does.

use super::re::Re;
use super::shell;
use serde_json::Value;

const PROTECTED: &[&str] = &["main", "master", "develop", "production"];

const KNOWN: &str = " add am annotate apply archive bisect blame branch bundle cat-file check-attr check-ignore checkout cherry-pick clean clone column commit commit-tree config count-objects credential describe diff difftool fetch filter-branch filter-repo for-each-ref format-patch fsck gc grep hash-object help init interpret-trailers lfs log ls-files ls-remote ls-tree maintenance merge merge-base mergetool mv name-rev notes p4 patch-id prune pull push range-diff read-tree rebase reflog remote repack replace request-pull reset restore rev-list rev-parse revert rm send-email shortlog show show-branch sparse-checkout status stripspace submodule svn switch symbolic-ref tag update-index update-ref var verify-commit verify-tag whatchanged worktree write-tree ";

fn has(flat: &str, pattern: &str) -> bool {
    Re::new(pattern).any_line(flat)
}

/// True when `flat` runs git's `sub`, as far as the words tell.
pub fn has_git_subcmd(flat: &str, sub: &str) -> bool {
    has(
        flat,
        &format!(r"(^|[[:space:]])git[[:space:]]+{sub}([[:space:]]|$)"),
    ) || has(
        flat,
        &format!(r"(^|[[:space:]])git[[:space:]].*[[:space:]]{sub}([[:space:]]|$)"),
    )
}

/// The words after `push` in `flat` that are not options: the remote, then the refspecs.
fn push_positional(flat: &str) -> Vec<&str> {
    let mut args = Vec::new();
    for line in flat.split('\n') {
        let mut found = false;
        for f in line.split_whitespace() {
            if found {
                args.push(f);
            } else if f == "push" {
                found = true;
            }
        }
    }
    args.into_iter().filter(|t| !t.starts_with('-')).collect()
}

/// True when a `--force-with-lease` push of `flat` may reach a protected or unreadable target.
fn push_target_requires_deny(flat: &str) -> bool {
    if has(flat, r"(^|[[:space:]])--(all|mirror|tags)([[:space:]=]|$)") {
        return true;
    }
    let positional = push_positional(flat);
    if positional.len() < 2 {
        return true;
    }
    for refspec in &positional[1..] {
        let refspec = refspec.strip_prefix('+').unwrap_or(refspec);
        if refspec.contains(['"', '\'', '$', '`']) || refspec == "tag" {
            return true;
        }
        let dst = refspec.rsplit(':').next().unwrap_or(refspec);
        if dst.is_empty() || dst.contains(['*', '?', '[']) || dst == "HEAD" || dst == "@" {
            return true;
        }
        let target = if let Some(t) = dst.strip_prefix("refs/heads/") {
            t
        } else if dst.starts_with("refs/") {
            return true;
        } else {
            dst
        };
        if PROTECTED.contains(&target) || target.starts_with("release/") {
            return true;
        }
    }
    false
}

/// For each git invocation of `flat` that runs `target`, the words after it; `__alias__` for an
/// alias definition that names it.
pub fn subcommand_args(flat: &str, target: &str) -> Vec<String> {
    let takes_value = |t: &str| {
        matches!(
            t,
            "-C" | "-c"
                | "--git-dir"
                | "--work-tree"
                | "--namespace"
                | "--exec-path"
                | "--super-prefix"
                | "--config-env"
                | "--attr-source"
        )
    };
    let loose_eq =
        Re::new(r"^--(git-dir|work-tree|namespace|exec-path|super-prefix|config-env|attr-source)=");
    let alias = Re::new(r"^alias\.[^=]*=");
    let word = Re::new(r"^[a-z][a-z0-9-]*$");
    let gitpath = Re::new(r"[/\\]git(\.exe)?$");
    let mut out = Vec::new();
    for line in flat.split('\n') {
        let f: Vec<&str> = line.split_whitespace().collect();
        for w in &f {
            if alias.is_match(w) && w.contains(target) {
                out.push("__alias__".to_string());
            }
        }
        for i in 0..f.len() {
            let name = f[i].to_lowercase();
            if name != "git" && name != "git.exe" && !gitpath.is_match(&name) {
                continue;
            }
            let mut j = i + 1;
            let mut loose = false;
            while j < f.len() {
                if f[j].starts_with('-') {
                    if takes_value(f[j]) {
                        j += 1;
                        loose = true;
                    } else if loose_eq.is_match(f[j]) {
                        loose = true;
                    }
                    j += 1;
                    continue;
                }
                if !word.is_match(f[j]) {
                    j += 1;
                    continue;
                }
                if f[j] == target {
                    break;
                }
                if KNOWN.contains(&format!(" {} ", f[j])) && !loose {
                    j = f.len() + 1;
                    break;
                }
                j += 1;
            }
            if j < f.len() && f[j] == target {
                out.push(f[j + 1..].join(" "));
            }
        }
    }
    out
}

pub fn check(payload: &Value) -> Result<(), String> {
    let command = super::command(payload);
    if command.is_empty() {
        return Ok(());
    }
    let (segments, assigns) = shell::segments_and_assignments(&command);
    if let Some(why) = shell::unreadable(&segments, &assigns) {
        return Err(why);
    }
    for segment in segments.iter().filter(|s| !s.is_empty()) {
        check_flat(&shell::flat_segment(segment, &assigns))?;
    }
    Ok(())
}

fn check_flat(flat: &str) -> Result<(), String> {
    let refuse = |why: &str| Err(why.to_string());
    if has_git_subcmd(flat, "push") {
        let lease = has(flat, r"(^|[[:space:]])--force-with-lease([[:space:]=]|$)");
        let plain = has(flat, r"(^|[[:space:]])--force([[:space:]=]|$)")
            || has(flat, r"(^|[[:space:]])-f([[:space:]]|$)")
            || has(flat, r"(^|[[:space:]])-[a-zA-Z]*f[a-zA-Z]*([[:space:]]|$)")
            // makiba's addition: a refspec starting with `+` forces that ref.
            || push_positional(flat).iter().skip(1).any(|r| r.starts_with('+'));
        if plain {
            return refuse(
                "a plain force push (--force, -f, a bundle holding f, or a +refspec) rewrites shared history; use --force-with-lease to an unprotected branch you name",
            );
        }
        if lease && push_target_requires_deny(flat) {
            return refuse(
                "--force-with-lease is allowed only to an unprotected branch named outright (not main, master, develop, production or release/*, and not HEAD, a wildcard, --all, --mirror or --tags)",
            );
        }
    }
    if has_git_subcmd(flat, "reset") && has(flat, r"(^|[[:space:]])--hard([[:space:]=]|$)") {
        return refuse(
            "git reset --hard loses uncommitted work; to set work aside, commit it to your branch (git add -u) and undo with git reset --soft HEAD~1",
        );
    }
    if has_git_subcmd(flat, "branch") {
        if has(flat, r"(^|[[:space:]])-D([[:space:]]|$)") {
            return refuse("git branch -D deletes an unmerged branch; try -d, or ask");
        }
        if has(flat, r"(^|[[:space:]])--delete([[:space:]]|$)")
            && has(flat, r"(^|[[:space:]])--force([[:space:]=]|$)")
        {
            return refuse("git branch --delete --force is -D; try -d, or ask");
        }
    }
    if has_git_subcmd(flat, "clean")
        && (has(flat, r"(^|[[:space:]])--force([[:space:]=]|$)")
            || has(flat, r"(^|[[:space:]])-f([[:space:]]|$)")
            || has(flat, r"(^|[[:space:]])-[a-zA-Z]*f[a-zA-Z]*([[:space:]]|$)"))
    {
        return refuse(
            "git clean with a force flag loses untracked files; look at them and delete each by name",
        );
    }
    if has_git_subcmd(flat, "checkout") && has(flat, r"(^|[[:space:]])--([[:space:]]|$)") {
        return refuse(
            "git checkout -- <path> loses uncommitted work; commit it to your branch first, and read the old content with git show HEAD:<path>",
        );
    }
    let index_only = || {
        let staged = has(flat, r"(^|[[:space:]])(--staged|-S)([[:space:]]|$)");
        staged && !has(flat, r"(^|[[:space:]])(--worktree|-W)([[:space:]]|$)")
    };
    if has_git_subcmd(flat, "restore")
        && has(
            flat,
            r"(^|[[:space:]])(--source([[:space:]=])|-s([[:space:]=]|$|[^[:space:]]))",
        )
        && !index_only()
    {
        return refuse(
            "git restore --source=<ref> overwrites uncommitted work; restore the index only with --staged alone",
        );
    }
    if has_git_subcmd(flat, "tag")
        && (has(flat, r"(^|[[:space:]])-d([[:space:]]|$)")
            || has(flat, r"(^|[[:space:]])--delete([[:space:]]|$)"))
    {
        return refuse("deleting a tag changes the shared tags; ask the secretary");
    }
    if has_git_subcmd(flat, "update-ref")
        && (has(flat, r"(^|[[:space:]])-d([[:space:]]|$)")
            || has(flat, r"(^|[[:space:]])--stdin([[:space:]]|$)"))
    {
        return refuse(
            "git update-ref -d or --stdin rewrites refs directly; it is outside a worker's work",
        );
    }
    if has_git_subcmd(flat, "reflog")
        && has(flat, r"(^|[[:space:]])(expire|delete)([[:space:]]|$)")
        && (has(flat, r"(^|[[:space:]])--all([[:space:]]|$)")
            || has(flat, r"(^|[[:space:]])--expire(=|[[:space:]])(now|0)")
            || has(
                flat,
                r"(^|[[:space:]])--expire-unreachable(=|[[:space:]])(now|0)",
            ))
    {
        return refuse("expiring the whole reflog loses its record");
    }
    for args in subcommand_args(flat, "stash") {
        let first = args.split(' ').next().unwrap_or("");
        if first != "list" && first != "show" {
            return refuse(
                "a git stash that changes something is refused (only list and show pass); set work aside with a commit on your branch (git add -u) and undo it with git reset --soft HEAD~1",
            );
        }
    }
    for args in subcommand_args(flat, "add") {
        for tok in args.split_whitespace() {
            let mut c = tok.chars();
            let bundle_a = c.next() == Some('-')
                && c.next().is_some_and(|l| l.is_ascii_alphabetic())
                && c.as_str().contains('A');
            if tok == "__alias__"
                || tok.starts_with("-A")
                || tok == "--all"
                || tok == "."
                || tok == "./"
                || bundle_a
            {
                return refuse(
                    "git add -A, --all or . stages everything; stage tracked changes with git add -u and new files by path",
                );
            }
        }
    }
    Ok(())
}
