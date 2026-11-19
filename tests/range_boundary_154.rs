//! Issue 154: the checks of the supported Claude Code range take the boundary from the constants
//! of `src/agents/version.rs`, so moving the range never needs an edit here.

use makiba::agents::version::{LOWEST, NEWEST_CHECKED, SUPPORTED, check};

fn text((a, b, c): (u64, u64, u64)) -> String {
    format!("{a}.{b}.{c} (Claude Code)")
}

#[test]
fn the_lowest_and_the_newest_checked_are_accepted() {
    for v in [LOWEST, NEWEST_CHECKED] {
        check(&text(v)).unwrap_or_else(|e| panic!("{v:?}: {e}"));
    }
}

#[test]
fn one_below_the_lowest_is_refused_and_one_above_the_newest_checked_is_accepted() {
    // ponytail: assumes the lowest patch is not 0; use the minor then if it ever is.
    let below = (LOWEST.0, LOWEST.1, LOWEST.2 - 1);
    let above = (NEWEST_CHECKED.0, NEWEST_CHECKED.1, NEWEST_CHECKED.2 + 1);
    let err = check(&text(below)).expect_err(&text(below)).to_string();
    assert!(err.contains(&text(below)), "{err}");
    assert!(err.contains(SUPPORTED), "{err}");
    check(&text(above)).unwrap_or_else(|e| panic!("{above:?}: {e}"));
}

#[test]
fn the_message_text_agrees_with_the_constants() {
    let f = |(a, b, c): (u64, u64, u64)| format!("{a}.{b}.{c}");
    assert_eq!(SUPPORTED, format!(">={}", f(LOWEST)));
}
