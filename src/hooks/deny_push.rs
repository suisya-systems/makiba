//! `makiba hook deny-push` (M4-3): a `PreToolUse` hook on Bash, for the roles that must not write
//! to GitHub (inventory HK-3, line L-K2). Ported from claude-org-ja's `.hooks/block-git-push.sh`
//! with the cases of its test. It refuses:
//!
//! - every `git push`, also behind `eval`, `bash -c` or `sh -c`;
//! - every `gh` call outside a list of reading subcommands, and `gh api` with a writing method,
//!   a body, or a GraphQL mutation; a group, subcommand or method it cannot read (a variable, a
//!   substitution, a name it does not know) is refused;
//! - a git alias that runs a shell or names push, since what it runs cannot be read.
//!
//! The text of a heredoc fed to `python` or `git commit`, a `grep` pattern and a commit message
//! are not taken for `gh` calls, when the command is plain enough to tell (no alias, `PATH=` or
//! function definition, and a parse that does not get lost).

use super::Refusal;
use super::re::Re;
use super::shell;
use serde_json::Value;

/// The original's pattern, rewritten to match the same commands without its ambiguity (which
/// backtracks for minutes on a long run of options or blanks): no empty item, since a run of
/// blanks already joins two items, and an option's value does not start with `-`, since such a
/// word reads as an option anyway.
const PUSH_RE: &str = r"(^|[|&;[:space:](`])git([[:space:]]+-[^[:space:]]+([[:space:]]+[^-|&;[:space:]][^|&;[:space:]]*)?)*[[:space:]]+push([[:space:];&|)`<>]|$)";

/// `PUSH_RE` where git is the command: at the start, after assignments and wrappers.
const PUSH_CMD: &str = r"^[[:space:]]*([A-Za-z_][A-Za-z0-9_]*=[^[:space:]]*[[:space:]]+|(env|command|builtin|exec|sudo|nohup|time|nice|timeout[[:space:]]+[^[:space:]]+)[[:space:]]+)*git([[:space:]]+-[^[:space:]]+([[:space:]]+[^-|&;[:space:]][^|&;[:space:]]*)?)*[[:space:]]+push([[:space:];&|)`<>]|$)";

const READ_GROUPS: &[&str] = &[
    "search",
    "browse",
    "status",
    "org",
    "attestation",
    "help",
    "completion",
    "version",
];
const READ_KEYS: &[&str] = &[
    "pr:list",
    "pr:status",
    "pr:checks",
    "pr:diff",
    "pr:view",
    "pr:checkout",
    "issue:list",
    "issue:status",
    "issue:view",
    "repo:list",
    "repo:view",
    "repo:clone",
    "repo:set-default",
    "release:list",
    "release:view",
    "release:download",
    "run:list",
    "run:view",
    "run:watch",
    "run:download",
    "workflow:list",
    "workflow:view",
    "gist:list",
    "gist:view",
    "gist:clone",
    "project:list",
    "project:view",
    "project:field-list",
    "project:item-list",
    "cache:list",
    "label:list",
    "secret:list",
    "variable:list",
    "variable:get",
    "ruleset:check",
    "ruleset:list",
    "ruleset:view",
    "alias:list",
    "auth:status",
    "config:get",
    "config:list",
    "extension:list",
    "extension:search",
    "extension:browse",
    "gpg-key:list",
    "ssh-key:list",
    "codespace:list",
    "codespace:view",
    "codespace:logs",
];
const NESTED: &[&str] = &[
    "repo:deploy-key",
    "repo:autolink",
    "repo:gitignore",
    "repo:license",
];
const READ_KEYS3: &[&str] = &[
    "repo:deploy-key:list",
    "repo:autolink:list",
    "repo:autolink:view",
    "repo:gitignore:list",
    "repo:gitignore:view",
    "repo:license:list",
    "repo:license:view",
];
pub(crate) const GROUPS: &[&str] = &[
    "pr",
    "issue",
    "repo",
    "release",
    "run",
    "workflow",
    "gist",
    "org",
    "project",
    "cache",
    "label",
    "secret",
    "variable",
    "ruleset",
    "search",
    "alias",
    "api",
    "auth",
    "attestation",
    "config",
    "extension",
    "gpg-key",
    "ssh-key",
    "codespace",
    "preview",
    "browse",
    "status",
    "co",
    "help",
    "completion",
    "version",
];

const PUSH_REASON: &str =
    "git push is the secretary's: report your commits and the secretary pushes them";

/// The line this hook holds.
const LINE: &str = "L-K2";

fn refuse(matched: impl Into<String>, reason: impl Into<String>) -> Result<(), Refusal> {
    Err(Refusal::new(LINE, matched, reason))
}

/// A refusal whose matched text holds `key`, the words refused on, kept in view when it is cut.
fn refuse_at(
    matched: impl Into<String>,
    key: &str,
    reason: impl Into<String>,
) -> Result<(), Refusal> {
    Err(Refusal::new(LINE, matched, reason).at(key))
}

pub fn check(payload: &Value) -> Result<(), Refusal> {
    let command = super::command(payload);
    if command.is_empty() {
        return Ok(());
    }
    let push = Re::new(PUSH_RE);
    if let Some(line) = command
        .split('\n')
        .find(|l| push.any_line(&shell::plain_git(l)))
    {
        return refuse_at(line, "push", PUSH_REASON);
    }
    let lines: Vec<String> = command.split('\n').map(String::from).collect();
    if let Some(body) = shell::unwrap(&lines)
        .into_iter()
        .find(|b| push.any_line(&shell::plain_git(b)))
    {
        return refuse_at(
            body,
            "push",
            format!("{PUSH_REASON} (seen inside eval or bash -c)"),
        );
    }
    // makiba's addition: each segment as the shell reads its words (quotes and backslashes
    // dropped, known variables expanded), so `git "push"` and `G=push; git $G` are seen.
    // Only where git is the segment's command, so a message that mentions a push is not one.
    let push_cmd = Re::new(PUSH_CMD);
    let (segments, assigns) = shell::segments_and_assignments(&command);
    if let Some((matched, why)) = shell::unreadable_at(&segments, &assigns) {
        return refuse(matched, why);
    }
    for segment in segments.iter().filter(|s| !s.is_empty()) {
        let words = shell::expand(segment, &assigns).replace(['"', '\'', '\\'], "");
        if push_cmd.any_line(&shell::plain_git(&words)) {
            return refuse_at(segment.as_str(), "push", PUSH_REASON);
        }
    }
    check_gh(&command)
}

fn strict_ok(text: &str) -> bool {
    !shell::split_segments(text).1
}

fn check_gh(command: &str) -> Result<(), Refusal> {
    let guard =
        Re::new(r"(^|[^A-Za-z0-9_-])(alias|enable|hash|function|shopt|PATH=|BASH_ENV|ENV=)|\(\)");
    let mut gh_command = command.to_string();
    let mut data_ok = false;
    if !command.contains('\u{1e}') && !guard.any_line(command) && strict_ok(command) {
        let masked = mask_data_heredocs(command);
        if strict_ok(&masked) {
            gh_command = masked;
            data_ok = true;
        }
    }
    let mut segments = Vec::new();
    for seg in shell::split_segments(&gh_command).0 {
        let seg = if data_ok {
            strip_data_literals(&seg)
        } else {
            seg
        };
        segments.extend(seg.split('\n').map(String::from));
    }
    let unwrapped = shell::unwrap(&segments);
    segments.extend(unwrapped.into_iter().filter(|l| !l.is_empty()));
    let assigns = shell::collect_assignments(&segments);
    let alias = Re::icase(
        r"alias\.[^[:space:]=]+[=[:space:]]+(!|[^[:space:]]*push)|--config-env[=[:space:]]+alias\.|GIT_CONFIG_KEY_[0-9]+=alias\.",
    );
    let gh_word = Re::icase(r"(^|[[:space:]])[^[:space:]]*gh(\.exe)?([[:space:]]|$)");
    for segment in segments.iter().filter(|s| !s.is_empty()) {
        let expanded = if assigns.is_empty() {
            segment.clone()
        } else {
            shell::expand(segment, &assigns)
        };
        let flat = shell::flatten_text(&expanded);
        let joined = shell::flatten_text(&expanded.replace(['"', '\''], ""));
        if alias.any_line(&joined.replace('\\', "")) {
            return refuse_at(
                segment.as_str(),
                "alias",
                "a git alias that runs a shell or names push cannot be read; run the command by its own name",
            );
        }
        let mut verdicts = Vec::new();
        for variant in [&flat, &joined] {
            if !gh_word.any_line(variant) {
                continue;
            }
            verdicts = classify_gh(variant);
            if !verdicts.is_empty() {
                break;
            }
        }
        if !verdicts.is_empty() {
            return refuse_at(
                segment.as_str(),
                "gh ",
                format!(
                    "a gh call that writes to GitHub, or that cannot be read, is the secretary's (seen: {}). If no gh call was meant, the word gh in a text (a commit message, a heredoc) was read as one: put the text in a file and pass it by path",
                    verdicts.join(" ")
                ),
            );
        }
    }
    Ok(())
}

/// Each `gh` call of `text` that is not a known read: `DENY` reasons, empty when all read.
fn classify_gh(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let ghpath = Re::new(r"[/\\]gh(\.exe)?$");
    let cmd = Re::new(r"^[A-Za-z0-9_][A-Za-z0-9_.-]*$");
    for line in text.split('\n') {
        let line = line.replace(['<', '>'], " ");
        let f: Vec<&str> = line.split_whitespace().collect();
        let bare = |t: &str| t.trim_start_matches('(').trim_end_matches(')').to_string();
        let kind = |t: &str| {
            if t.contains(['$', '`']) {
                "var"
            } else if cmd.is_match(t) {
                "cmd"
            } else {
                "other"
            }
        };
        for i in 0..f.len() {
            let name = bare(f[i]).to_lowercase();
            if name != "gh" && name != "gh.exe" && !ghpath.is_match(&name) {
                continue;
            }
            let Some(gi) = next_word(&f, i + 1) else {
                continue;
            };
            match kind(&bare(f[gi])) {
                "var" => {
                    out.push(format!("group-undecidable:{}", f[gi]));
                    continue;
                }
                "other" => continue,
                _ => {}
            }
            let group = bare(f[gi]).to_lowercase();
            if !GROUPS.contains(&group.as_str()) {
                out.push(format!("unknown-group:{group}"));
                continue;
            }
            if READ_GROUPS.contains(&group.as_str()) || group == "co" {
                continue;
            }
            if group == "api" {
                out.extend(classify_api(&f, gi, &line));
                continue;
            }
            let Some(si) = next_word(&f, gi + 1) else {
                continue;
            };
            match kind(&bare(f[si])) {
                "var" => {
                    out.push(format!("subcommand-undecidable:{group}:{}", f[si]));
                    continue;
                }
                "other" => continue,
                _ => {}
            }
            let key = format!("{group}:{}", bare(f[si]).to_lowercase());
            if NESTED.contains(&key.as_str()) {
                let Some(ti) = next_word(&f, si + 1) else {
                    continue;
                };
                match kind(&bare(f[ti])) {
                    "var" => {
                        out.push(format!("subcommand-undecidable:{key}:{}", f[ti]));
                        continue;
                    }
                    "other" => continue,
                    _ => {}
                }
                let key3 = format!("{key}:{}", bare(f[ti]).to_lowercase());
                if !READ_KEYS3.contains(&key3.as_str()) {
                    out.push(key3);
                }
                continue;
            }
            if !READ_KEYS.contains(&key.as_str()) {
                out.push(key);
            }
        }
    }
    out
}

pub(crate) fn takes_value(t: &str) -> bool {
    matches!(
        t,
        "-R" | "--repo"
            | "--hostname"
            | "-H"
            | "--header"
            | "-X"
            | "--method"
            | "-f"
            | "--raw-field"
            | "-F"
            | "--field"
            | "-q"
            | "--jq"
            | "-t"
            | "--template"
            | "--json"
            | "--input"
            | "--cache"
            | "-L"
            | "--limit"
            | "-b"
            | "--body"
            | "-T"
            | "--title"
    )
}

/// The first word from `idx` on that is not an option or an option's value.
pub(crate) fn next_word(f: &[&str], idx: usize) -> Option<usize> {
    let mut j = idx;
    while j < f.len() {
        if f[j].starts_with('-') {
            if takes_value(f[j]) {
                j += 1;
            }
            j += 1;
            continue;
        }
        return Some(j);
    }
    None
}

/// `gh api` at `start`: refused unless it only reads.
pub(crate) fn classify_api(f: &[&str], start: usize, line: &str) -> Vec<String> {
    let opaque_re = Re::new(r"=[@$]");
    let long_field = Re::new(r"^(--raw-field|--field)=");
    let long_opaque = Re::new(r"=[^=]*=[@$]");
    let short_field = Re::new(r"^-[fF].");
    let short = Re::new(r"^-[A-Za-z]");
    let mut method = String::new();
    let (mut body, mut input, mut opaque) = (false, false, false);
    let next = |k: usize, missing: &str| {
        f.get(k + 1)
            .map(|s| s.to_string())
            .unwrap_or(missing.to_string())
    };
    let mut k = start + 1;
    while k < f.len() {
        let t = f[k];
        if t == "-X" || t == "--method" {
            method = next(k, "__missing__");
            k += 2;
            continue;
        }
        if let Some(m) = t.strip_prefix("--method=") {
            method = m.to_string();
        } else if t.starts_with("-X") && t.chars().count() > 2 {
            method = t[2..].to_string();
        } else if t == "--input" {
            input = true;
            body = true;
            k += 1;
        } else if t.starts_with("--input=") {
            input = true;
            body = true;
        } else if matches!(t, "-f" | "--raw-field" | "-F" | "--field") {
            body = true;
            if opaque_re.is_match(&next(k, "")) {
                opaque = true;
            }
            k += 1;
        } else if long_field.is_match(t) {
            body = true;
            if long_opaque.is_match(t) {
                opaque = true;
            }
        } else if short_field.is_match(t) {
            body = true;
            if opaque_re.is_match(t) {
                opaque = true;
            }
        } else if short.is_match(t) && !t.starts_with("--") {
            let cluster = &t[1..];
            if let Some(x) = cluster.find('X') {
                let cval = &cluster[x + 1..];
                if cval.is_empty() {
                    method = next(k, "__missing__");
                    k += 1;
                } else {
                    method = cval.to_string();
                }
            }
            if let Some(fp) = cluster.find('f').or_else(|| cluster.find('F')) {
                body = true;
                let mut cval = cluster[fp + 1..].to_string();
                if cval.is_empty() {
                    cval = next(k, "");
                    k += 1;
                }
                if opaque_re.is_match(&cval) {
                    opaque = true;
                }
            }
        }
        k += 1;
    }
    let graphql = next_word(f, start + 1).is_some_and(|e| f[e].to_lowercase() == "graphql");
    if method == "__missing__" || method.contains('$') {
        return vec!["api:method-undecidable".into()];
    }
    if graphql {
        if input {
            return vec!["api:graphql-input-undecidable".into()];
        }
        if opaque {
            return vec!["api:graphql-body-undecidable".into()];
        }
        let lower = line.to_lowercase();
        let nospace: String = lower.chars().filter(|c| !c.is_whitespace()).collect();
        if lower.contains("mutation") || nospace.contains("mutation") {
            return vec!["api:graphql-mutation".into()];
        }
        return vec![];
    }
    let mut m = method.to_uppercase();
    if m.is_empty() {
        m = if body { "POST" } else { "GET" }.into();
    }
    if m == "GET" || m == "HEAD" {
        return vec![];
    }
    vec![format!("api:{m}")]
}

/// The command with the bodies of heredocs fed to `python` or `git commit` blanked, while the
/// command stays plain enough to tell what consumes them.
fn mask_data_heredocs(command: &str) -> String {
    let safe = Re::new(r"^[A-Za-z0-9_./=:@%+,~ \t&|;-]*$");
    let w = r"[A-Za-z0-9_./=:@%+,~-]+";
    let pre = r"^([A-Za-z0-9_./=:@%+,~ \t-]*(&&|\|\||;|\|))*[ \t]*";
    let delim = r#"<<-?[ \t]*('[A-Za-z0-9_]+'|"[A-Za-z0-9_]+"|[A-Za-z0-9_]+)[ \t]*$"#;
    let consumer =
        format!(r"((python|python3|python3\.[0-9]+|py[ \t]+-3)|git[ \t]+commit)([ \t]+{w})*[ \t]*");
    let subst = format!(
        r#"git[ \t]+commit([ \t]+{w})*[ \t]+(-[A-Za-z]*m[ \t]*|--message(=|[ \t]+))"\$\(cat[ \t]*"#
    );
    let in_subst = Re::new(&format!("{pre}{subst}{delim}"));
    let in_consumer = Re::new(&format!("{pre}{consumer}{delim}"));
    let marker = Re::new(r#"<<-?[ \t]*['"]?[A-Za-z0-9_]+['"]?[ \t]*$"#);
    let close = Re::new(r#"^[ \t]*\)""#);
    let special = Re::new(r"[$`\\]");
    let lines: Vec<String> = shell_records(command);
    let n = lines.len();
    let mut out = lines.clone();
    let mut ok = true;
    let mut i = 0;
    while i < n {
        let line = &lines[i];
        if !ok {
            i += 1;
            continue;
        }
        let insubst = in_subst.is_match(line);
        if insubst || in_consumer.is_match(line) {
            let chars: Vec<char> = line.chars().collect();
            let (a, b) = marker.find_chars(&chars).unwrap();
            let mut d: String = chars[a + 2..b].iter().collect();
            let strip = d.starts_with('-');
            if strip {
                d.remove(0);
            }
            d.retain(|c| c != ' ' && c != '\t');
            let quoted = d.starts_with(['\'', '"']);
            d.retain(|c| c != '\'' && c != '"');
            let mut k = i + 1;
            while k < n {
                let cmp = if strip {
                    lines[k].trim_start_matches('\t')
                } else {
                    &lines[k]
                };
                if cmp == d {
                    break;
                }
                if !quoted && special.is_match(&lines[k]) {
                    k = n;
                    break;
                }
                k += 1;
            }
            if k >= n {
                ok = false;
                i += 1;
                continue;
            }
            for item in out.iter_mut().take(k).skip(i + 1) {
                item.clear();
            }
            i = k + 1;
            if insubst {
                let tail_ok = lines.get(i).is_some_and(|l| {
                    close.is_match(l) && {
                        let at = l.find(")\"").unwrap() + 2;
                        safe.is_match(&l[at..])
                    }
                });
                if tail_ok {
                    i += 1;
                } else {
                    ok = false;
                }
            }
            continue;
        }
        if !safe.is_match(line) {
            ok = false;
        }
        i += 1;
    }
    out.join("\n")
}

/// The lines awk reads from `text`: a trailing newline ends the last line, not a new one.
fn shell_records(text: &str) -> Vec<String> {
    let t = text.strip_suffix('\n').unwrap_or(text);
    if t.is_empty() && text.is_empty() {
        return vec![];
    }
    t.split('\n').map(String::from).collect()
}

/// A segment whose quoted literals are data (a `grep` or `rg` pattern, a `git commit` message)
/// with the literals emptied; any other segment as it is.
fn strip_data_literals(seg: &str) -> String {
    let buf = shell_records(seg).join("\n");
    let s = buf.trim_start_matches([' ', '\t', '\n']);
    let mut w = s.split([' ', '\t', '\n']).filter(|x| !x.is_empty());
    let (w1, w2) = (w.next().unwrap_or(""), w.next().unwrap_or(""));
    let mut data =
        matches!(w1, "grep" | "egrep" | "fgrep" | "rg") || (w1 == "git" && w2 == "commit");
    if (w1 == "rg" && s.contains("--pre")) || s.contains("<<") {
        data = false;
    }
    if !data {
        return buf;
    }
    let c: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < c.len() {
        let ch = c[i];
        let nc = c.get(i + 1).copied().unwrap_or('\0');
        if ch == '\\' || ch == '`' {
            return buf;
        }
        if ch == '$' && matches!(nc, '(' | '\'' | '"') {
            return buf;
        }
        if (ch == '<' || ch == '>') && nc == '(' {
            return buf;
        }
        if ch == '\'' || ch == '"' {
            let Some(j) = c[i + 1..].iter().position(|&x| x == ch) else {
                return buf;
            };
            let body: String = c[i + 1..i + 1 + j].iter().collect();
            if ch == '"' && (body.contains("$(") || body.contains('`') || body.contains('\\')) {
                return buf;
            }
            out.push(ch);
            out.push(ch);
            i += j + 2;
            continue;
        }
        out.push(ch);
        i += 1;
    }
    out
}
