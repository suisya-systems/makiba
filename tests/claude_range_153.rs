//! Issue 153: the supported Claude Code range is 2.1.326 to 2.1.328.

use makiba::agents::version::{SUPPORTED, check};

#[test]
fn the_range_covers_326_to_328_and_nothing_else() {
    for v in ["2.1.326", "2.1.327", "2.1.328"] {
        check(&format!("{v} (Claude Code)")).unwrap_or_else(|e| panic!("{v}: {e}"));
    }
    for v in ["2.1.325", "2.1.329"] {
        let err = check(&format!("{v} (Claude Code)"))
            .expect_err(v)
            .to_string();
        assert!(err.contains(v), "{err}");
    }
}

#[test]
fn the_message_names_the_new_range() {
    assert_eq!(SUPPORTED, ">=2.1.326,<=2.1.328");
    let err = check("2.1.329 (Claude Code)").unwrap_err().to_string();
    assert!(err.contains(">=2.1.326,<=2.1.328"), "{err}");
}
