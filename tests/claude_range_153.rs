//! Issue 153: the supported Claude Code range starts at 2.1.326 and covers 2.1.328 (Issue 160: no
//! upper bound, so 2.1.329 is accepted).

use makiba::agents::version::{SUPPORTED, check};

#[test]
fn the_range_covers_326_to_328_and_nothing_below() {
    for v in ["2.1.326", "2.1.327", "2.1.328"] {
        check(&format!("{v} (Claude Code)")).unwrap_or_else(|e| panic!("{v}: {e}"));
    }
    check("2.1.329 (Claude Code)").unwrap();
    let err = check("2.1.325 (Claude Code)").unwrap_err().to_string();
    assert!(err.contains("2.1.325"), "{err}");
}

#[test]
fn the_message_names_the_new_range() {
    assert_eq!(SUPPORTED, ">=2.1.326");
    let err = check("2.1.325 (Claude Code)").unwrap_err().to_string();
    assert!(err.contains(">=2.1.326"), "{err}");
}
