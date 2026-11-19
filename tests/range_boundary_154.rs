//! Issue 154: the checks of the supported Claude Code range take the boundary from the constants
//! of `src/agents/version.rs`, so moving the range never needs an edit here.

use makiba::agents::version::{HIGHEST, LOWEST, SUPPORTED, check};

fn text((a, b, c): (u64, u64, u64)) -> String {
    format!("{a}.{b}.{c} (Claude Code)")
}

#[test]
fn both_ends_of_the_range_are_accepted() {
    for v in [LOWEST, HIGHEST] {
        check(&text(v)).unwrap_or_else(|e| panic!("{v:?}: {e}"));
    }
}

#[test]
fn one_below_the_lowest_and_one_above_the_highest_are_refused() {
    // ponytail: assumes the lowest patch is not 0; use the minor then if it ever is.
    let below = (LOWEST.0, LOWEST.1, LOWEST.2 - 1);
    let above = (HIGHEST.0, HIGHEST.1, HIGHEST.2 + 1);
    for v in [below, above] {
        let err = check(&text(v)).expect_err(&text(v)).to_string();
        assert!(err.contains(&text(v)), "{err}");
        assert!(err.contains(SUPPORTED), "{err}");
    }
}

#[test]
fn the_message_text_agrees_with_the_constants() {
    let f = |(a, b, c): (u64, u64, u64)| format!("{a}.{b}.{c}");
    assert_eq!(SUPPORTED, format!(">={},<={}", f(LOWEST), f(HIGHEST)));
}
