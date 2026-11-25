//! Issue 47, after review: how the merge gate hook reads a command line. Up to the subcommand of
//! the GitHub CLI it reads only what it reads with certainty; a text program that names the forms
//! passes; a program that may run gh does not. The findings of the review rounds stay refused.

use makiba::hooks::merge_gate::parse;

/// The word, kept out of this file's own command lines.
const M: &str = "mer\x67e";

#[test]
fn a_redirection_before_the_program_does_not_hide_a_merge() {
    for cmd in [
        format!("2>/dev/null gh pr {M} 7 --repo o/r --squash"),
        format!("> out.txt gh pr {M} 7 --repo o/r --squash"),
        format!("<in.txt gh pr {M} 7"),
        format!("2>&1 gh pr {M} 7"),
        format!("&>/dev/null gh pr {M} 7"),
        format!("2>/dev/null curl -X PUT https://api.github.com/repos/o/r/pulls/3/{M}"),
        format!("env 2>/dev/null gh pr {M} 7"),
    ] {
        assert!(parse(&cmd).is_err(), "not refused: {cmd:?}");
    }
    // Between the program, its group and its subcommand, too.
    for cmd in [
        format!("gh pr 2>/dev/null {M} 7 --repo o/r --squash"),
        format!("gh 2> err.txt pr {M} 7"),
        format!("gh pr >out.txt {M} 7"),
        format!("gh api -X PUT 2>/dev/null repos/o/r/pulls/3/{M}"),
    ] {
        assert!(parse(&cmd).is_err(), "not refused: {cmd:?}");
    }
    // A redirection after the program is still read as before.
    assert_eq!(parse("gh pr view 7 --json mergedAt 2>/dev/null"), Ok(()));
}

#[test]
fn a_quoted_word_with_blanks_before_the_program_stays_one_word() {
    for cmd in [
        format!("NOTE=\"two words\" gh pr {M} 7 --repo o/r --squash"),
        format!("NOTE='a b c' gh pr {M} 7"),
        format!("NOTE=a\\ b gh pr {M} 7"),
        format!("2>\"/tmp/a file\" gh pr {M} 7"),
        format!("env NOTE=\"x y\" gh pr {M} 7"),
    ] {
        assert!(parse(&cmd).is_err(), "not refused: {cmd:?}");
    }
    // A quoted argument with blanks is still one argument: a mention passes.
    assert_eq!(
        parse(&format!("git commit -m \"gh pr {M} 7 is gated\"")),
        Ok(())
    );
}

#[test]
fn a_ripgrep_pattern_is_a_mention_and_its_pre_option_is_not() {
    for cmd in [
        format!("rg 'gh pr {M}' skills/"),
        format!("rg -n \"gh pr {M}\" src docs"),
        format!("rg -l 'pulls/[0-9]+/{M}' ."),
    ] {
        assert_eq!(parse(&cmd), Ok(()), "refused a search: {cmd:?}");
    }
    for cmd in [
        format!("rg --pre $X {M} f"),
        format!("rg --pre=./run.sh {M} f"),
    ] {
        assert!(parse(&cmd).is_err(), "not refused: {cmd:?}");
    }
}

fn refused(cmds: &[String]) {
    let passed: Vec<&String> = cmds.iter().filter(|c| parse(c).is_ok()).collect();
    assert!(passed.is_empty(), "not refused: {passed:#?}");
}

fn passes(cmds: &[String]) {
    let refused: Vec<(&String, String)> = cmds
        .iter()
        .filter_map(|c| parse(c).err().map(|e| (c, e)))
        .collect();
    assert!(refused.is_empty(), "refused: {refused:#?}");
}

/// The findings of the four review rounds, kept as regressions.
#[test]
fn the_review_findings_stay_refused() {
    refused(&[
        // Round 1: a redirection before the program.
        format!("2>/dev/null gh pr {M} 7 --repo o/r --squash"),
        // Round 2: a quoted word with blanks before the program.
        format!("NOTE=\"two words\" gh pr {M} 7 --repo o/r --squash"),
        // Round 3: a redirection between gh's words.
        format!("gh pr 2>/dev/null {M} 7 --repo o/r --squash"),
        // Round 4: an operator glued to the subcommand.
        format!("gh pr {M}>/dev/null 7 --repo o/r --squash"),
        format!("gh pr {M}</dev/null 7"),
        format!("gh pr {M}2>/dev/null 7"),
        format!("gh pr>/dev/null {M} 7"),
        format!("gh>/dev/null pr {M} 7"),
    ]);
    // Round 1 also: a ripgrep pattern is a mention.
    passes(&[format!("rg 'gh pr {M}' skills/")]);
}

/// Up to the subcommand, a word the hook cannot read with certainty is refused, merge or not.
#[test]
fn an_uncertain_word_up_to_the_subcommand_is_refused() {
    refused(&[
        "gh pr mer?e 7".into(),
        "gh pr m*e 7".into(),
        "gh \"$G\" view 7".into(),
        "gh pr \"$S\" 7".into(),
        "gh -R $R pr view 3".into(),
        "gh pr ${S} 7".into(),
        "gh pr v\\iew 7".into(),
        "gh pr \"vi\\ew\" 7".into(),
        "gh api $PATH_TO".into(),
        "gh api repos/o/r/pulls/3 -f x=\"$V\"".into(),
        "curl \"$URL\"".into(),
        "wget $U".into(),
        "curl https://example.com/x 2>$LOG".into(),
    ]);
    // After a known read subcommand, an argument cannot make it a merge.
    passes(&[
        "gh pr view $PR --repo o/r --json mergedAt".into(),
        "gh pr view \"$PR\" --json state -q '.state'".into(),
        "gh run view $RUN --log-failed".into(),
    ]);
}

#[test]
fn an_unknown_gh_group_or_an_alias_definition_is_refused() {
    refused(&[
        "gh pm 7".into(),
        "gh my-extension run".into(),
        "gh alias set pv 'pr view'".into(),
        "gh alias import aliases.yml".into(),
    ]);
    passes(&["gh alias list".into(), "gh co 7".into(), "gh status".into()]);
}

#[test]
fn gh_fed_its_words_by_another_program_is_refused() {
    refused(&[
        format!("echo {M} 7 | xargs gh pr"),
        "xargs gh pr view < prs.txt".into(),
        "find . -name x -exec gh pr view {} ;".into(),
        "parallel gh pr view ::: 1 2".into(),
        format!("xargs -I{{}} $X pr {M} {{}}"),
    ]);
    passes(&[
        "find . -name '*.md'".into(),
        "xargs -n1 echo < list.txt".into(),
    ]);
}

#[test]
fn a_program_word_that_is_not_plain_is_refused() {
    refused(&[
        "$EDITOR notes.md".into(),
        "\"$PY\" tool.py".into(),
        "${GH:-gh} pr view 7".into(),
        "$(which gh) pr view 7".into(),
        "`which gh` pr view 7".into(),
        "g\\h pr view 7".into(),
        "env \"$X\" pr view 7".into(),
    ]);
}

/// The wrappers the hook reads through, and a wrapper it cannot read.
#[test]
fn a_wrapper_runs_gh_as_the_command() {
    refused(&[
        format!("env GH_REPO=o/r gh pr {M} 7"),
        format!("env -u X gh pr {M} 7"),
        format!("command gh pr {M} 7"),
        format!("builtin exec gh pr {M} 7"),
        format!("exec -a x gh pr {M} 7"),
        format!("nohup gh pr {M} 7"),
        format!("time -p gh pr {M} 7"),
        format!("nice -n 5 gh pr {M} 7"),
        format!("timeout 30 gh pr {M} 7"),
        format!("timeout -k 5 30 gh pr {M} 7"),
        format!("sudo -u me gh pr {M} 7"),
        format!("doas gh pr {M} 7"),
        format!("stdbuf -oL gh pr {M} 7"),
        format!("setsid gh pr {M} 7"),
        format!("if gh pr {M} 7; then echo ok; fi"),
        format!("! gh pr {M} 7"),
        format!("( gh pr {M} 7 )"),
        format!("{{ gh pr {M} 7; }}"),
        // `env -S` splits a string into a command the hook does not read.
        "env -S 'gh pr view 7'".into(),
    ]);
    passes(&[
        "env GH_REPO=o/r gh pr view 7".into(),
        "timeout 30 gh pr checks 7 --repo o/r".into(),
        "command -v gh".into(),
    ]);
}

/// A program that may run its arguments: the GitHub CLI among them is not a mention.
#[test]
fn gh_under_a_program_that_may_run_it_is_refused() {
    refused(&[
        "flock /tmp/l gh pr view 7".into(),
        "ssh host gh pr view 7".into(),
        "unknown-tool --run gh pr view 7".into(),
        "watch gh pr checks 7".into(),
        format!("python3 -c \"import os; os.system('gh pr {M} 7')\""),
        format!("bash -c 'gh pr {M} 7'"),
    ]);
}

/// Text programs only name the forms: they pass.
#[test]
fn a_text_program_that_names_the_forms_passes() {
    passes(&[
        format!("echo gh pr {M} 7 --admin"),
        format!("printf '%s\\n' gh pr {M} 7"),
        format!("git commit -m \"run gh pr {M} only through the gate\""),
        "git commit -F .worker-scratch/msg.txt".into(),
        format!("grep -rn 'gh pr {M}' skills"),
        "sed -n '1,20p' skills/pull-request/SKILL.md".into(),
        "head -5 notes.md".into(),
        "echo curl https://api.github.com/repos/o/r".into(),
        "cat docs/design/spec.md".into(),
    ]);
}

/// The reads the secretary needs stay usable.
#[test]
fn the_secretarys_reads_pass() {
    passes(&[
        "gh pr view 7 --repo o/r --json state,headRefOid,mergedAt -q '.headRefOid'".into(),
        "gh pr view 7 --repo o/r --json statusCheckRollup -q '.statusCheckRollup[] | select(.conclusion != \"SUCCESS\")'".into(),
        "gh pr checks 7 --repo o/r".into(),
        "gh run view 123 --repo o/r --log-failed > failed.txt".into(),
        "gh run view 123 --repo o/r --log-failed >failed.txt 2>&1".into(),
        "gh pr list --repo o/r --state open --json number,title".into(),
        "gh api repos/o/r/pulls/7".into(),
        "gh api repos/o/r/commits/abc/check-runs --jq '.check_runs[].name'".into(),
    ]);
}

/// A here-document fed to a text program is data; one fed to a shell is read as commands.
#[test]
fn a_heredoc_body_is_data_only_for_a_text_program() {
    passes(&[
        "cat > body.md <<'EOF'\nRun the read gh pr view 7 first\nEOF".into(),
        "cat > body.md <<EOF\nThen gh pr checks 7\nEOF".into(),
        "tee notes.md <<-'EOF'\n\tgh pr view 7\n\tEOF".into(),
    ]);
    refused(&[
        format!("bash <<'EOF'\ngh pr {M} 7\nEOF"),
        "sh -s <<EOF\nflock l gh pr view 7\nEOF".into(),
        format!("cat > b.md <<'EOF'\nx\nEOF\ngh pr {M} 7"),
        format!("cat /dev/null # <<'EOF'\ngh pr {M} 7\nEOF"),
    ]);
}
