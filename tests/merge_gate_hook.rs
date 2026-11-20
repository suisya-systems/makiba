//! Issue 47, after review: the merge gate hook reads a redirection before the program, and takes a
//! ripgrep pattern for a mention (only `rg --pre` runs a program).

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
    assert_eq!(parse("gh pr view 7 --json mergedAt 2>/dev/null"), Ok(None));
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
        Ok(None)
    );
}

#[test]
fn a_ripgrep_pattern_is_a_mention_and_its_pre_option_is_not() {
    for cmd in [
        format!("rg 'gh pr {M}' skills/"),
        format!("rg -n \"gh pr {M}\" src docs"),
        format!("rg -l 'pulls/[0-9]+/{M}' ."),
    ] {
        assert_eq!(parse(&cmd), Ok(None), "refused a search: {cmd:?}");
    }
    for cmd in [
        format!("rg --pre $X {M} f"),
        format!("rg --pre=./run.sh {M} f"),
    ] {
        assert!(parse(&cmd).is_err(), "not refused: {cmd:?}");
    }
}
