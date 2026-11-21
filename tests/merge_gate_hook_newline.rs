//! Issue 200: the merge gate hook keeps a quoted argument of several lines whole. A newline
//! inside single or double quotes does not end a segment, so a commit message that names the
//! landing command on one of its lines is a mention and passes. The same words on a line of
//! their own outside quotes are still a command and are refused, and so are the two holes the
//! design review found (a substitution and a here-document spanning lines).
//!
//! The cases of `tests/merge_gate_hook.rs` and the hook cases of `tests/acceptance_gate.rs` are
//! kept by those files, which this Issue does not edit.

use makiba::hooks::merge_gate::parse;

/// The word, kept out of this file's own command lines.
const M: &str = "mer\x67e";

fn passes(cmds: &[String]) {
    let refused: Vec<(&String, String)> = cmds
        .iter()
        .filter_map(|c| parse(c).err().map(|e| (c, e)))
        .collect();
    assert!(refused.is_empty(), "refused: {refused:#?}");
}

fn refused(cmds: &[String]) {
    let passed: Vec<&String> = cmds.iter().filter(|c| parse(c).is_ok()).collect();
    assert!(passed.is_empty(), "not refused: {passed:#?}");
}

#[test]
fn a_quoted_message_of_several_lines_naming_the_landing_command_passes() {
    let sha = "0123456789abcdef0123456789abcdef01234567";
    passes(&[
        format!(
            "git commit -m \"Gate the landing\n\nThe secretary runs gh pr {M} 7 --repo o/r --squash after the gate.\n\""
        ),
        format!("git commit -m 'Gate the landing\n\ngh pr {M} 7 --repo o/r --squash\n'"),
        format!(
            "git commit -m \"Say \\\"why\\\"\ngh pr {M} 7 --repo o/r --squash --match-head-commit {sha}\n\""
        ),
        format!("git commit -m \"Title\" -m \"Body line\ngh pr {M} 7\nlast line\""),
        format!("git add -u && git commit -m \"Title\n\ngh pr {M} 7 is gated\""),
    ]);
}

#[test]
fn the_same_words_on_a_line_of_their_own_outside_quotes_are_refused() {
    refused(&[
        format!("echo hi\ngh pr {M} 7 --repo o/r --squash"),
        format!("git commit -m \"Title\nbody\"\ngh pr {M} 7 --repo o/r --squash"),
        format!("git commit -m 'Title\nbody'\ngh pr {M} 7"),
        // A quote in a comment opens nothing.
        format!("echo hi # it's\ngh pr {M} 7\n# '"),
        format!("(echo hi # it's\ngh pr {M} 7\n# ')"),
        format!("{{ echo hi # it's\ngh pr {M} 7\n# '\n}}"),
    ]);
}

#[test]
fn an_unterminated_quote_is_cut_at_every_newline_as_before() {
    refused(&[format!(
        "git commit -m \"Title\ngh pr {M} 7 --repo o/r --squash"
    )]);
}

#[test]
fn a_substitution_or_a_here_document_over_several_lines_is_still_read_line_by_line() {
    refused(&[
        // A substitution spanning lines inside double quotes (design review, hole 1).
        "echo \"$(\ngh api graphql --input q.json\n)\"".into(),
        format!("echo \"`\ngh pr {M} 7\n`\""),
        // A here-document body the splitter attaches to a later segment (design review, hole 2).
        format!("bash <<'EOF' | cat # '\ngh pr {M} 7\n# '\nEOF"),
        // A body run by bash -c over several lines.
        format!("bash -c \"echo x\ngh pr {M} 7\""),
    ]);
}
