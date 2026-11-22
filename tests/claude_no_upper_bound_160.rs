//! Issue 160: Claude Code has no upper bound. A version at or above the lowest supported one is
//! accepted (one newer than the newest checked says so in one line); a version below it, one on
//! the refused list and a text that cannot be parsed are refused with a message naming what was
//! found and why. Every version is built from the constants.

mod common;

use common::makibad;
use makiba::agents::version::{LOWEST, NEWEST_CHECKED, REFUSED, SUPPORTED, check, check_with};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

fn text((a, b, c): (u64, u64, u64)) -> String {
    format!("{a}.{b}.{c} (Claude Code)")
}

fn above(v: (u64, u64, u64)) -> (u64, u64, u64) {
    (v.0, v.1, v.2 + 1)
}

#[test]
fn lowest_newest_checked_and_above_are_accepted() {
    let far = (NEWEST_CHECKED.0 + 5, 0, 0);
    for v in [LOWEST, NEWEST_CHECKED, above(NEWEST_CHECKED), far] {
        check(&text(v)).unwrap_or_else(|e| panic!("{v:?}: {e}"));
    }
}

#[test]
fn only_a_version_above_the_newest_checked_gets_a_line() {
    for v in [LOWEST, NEWEST_CHECKED] {
        assert_eq!(check(&text(v)).unwrap(), None, "{v:?}");
    }
    let v = above(NEWEST_CHECKED);
    let line = check(&text(v)).unwrap().expect("a line above the newest");
    assert!(line.contains(&text(v)), "{line}");
    assert!(!line.contains('\n'), "{line}");
}

#[test]
fn below_the_lowest_and_unparsable_are_refused_naming_what_was_found() {
    // ponytail: assumes the lowest patch is not 0; use the minor then if it ever is.
    let below = text((LOWEST.0, LOWEST.1, LOWEST.2 - 1));
    for t in [below.as_str(), "", "claude", "2.1", "x.y.z"] {
        let err = check(t).expect_err(t).to_string();
        assert!(err.contains(&format!("{t:?}")) || err.contains(t), "{err}");
        assert!(err.contains(SUPPORTED), "{err}");
    }
}

#[test]
fn a_refused_entry_is_refused_with_its_reason_and_others_still_pass() {
    let bad = above(NEWEST_CHECKED);
    let list = [(bad, "its sandbox settings mean less")];
    let err = check_with(&text(bad), &list).unwrap_err().to_string();
    assert!(err.contains(&text(bad)), "{err}");
    assert!(err.contains("its sandbox settings mean less"), "{err}");
    check_with(&text(NEWEST_CHECKED), &list).unwrap();
    check_with(&text(above(bad)), &list).unwrap();
}

#[test]
fn the_shipped_refused_list_is_empty_and_the_message_agrees_with_the_constants() {
    assert!(REFUSED.is_empty());
    let (a, b, c) = LOWEST;
    assert_eq!(SUPPORTED, format!(">={a}.{b}.{c}"));
}

/// `makiba up` with a stand-in `claude` printing `claude_prints`.
fn up(claude_prints: &str) -> (bool, String) {
    let root = tempfile::tempdir().unwrap();
    let bin = root.path().join("bin");
    fs::create_dir(&bin).unwrap();
    for (name, body) in [
        ("claude", format!("echo '{claude_prints}'")),
        ("hc", "exit 0".to_string()),
    ] {
        let p = bin.join(name);
        fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let r = |p: &str| root.path().join(p);
    let out = makibad::up(root.path())
        .env_clear()
        .env(makibad::KNOWLEDGE_ENV, makibad::knowledge(root.path()))
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("HOME", r("home"))
        .env("XDG_CONFIG_HOME", r("config"))
        .env("XDG_STATE_HOME", r("state"))
        .env("HC_BIN", bin.join("hc"))
        .env("MAKIBA_BASE", "stub")
        .env("MAKIBA_DOOR", "none")
        .env("MAKIBA_PORT", "0")
        .output()
        .unwrap();
    // End the daemon `up` started, if any.
    if let Ok(pid) = fs::read_to_string(r("state/makiba/makibad.lock")) {
        let _ = Command::new("kill").arg(pid.trim()).status();
    }
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), said)
}

#[test]
fn up_prints_the_line_above_the_newest_checked_and_nothing_at_it() {
    let v = above(NEWEST_CHECKED);
    let (ok, said) = up(&text(v));
    assert!(ok, "{said}");
    assert!(
        said.contains("claude: ") && said.contains(&text(v)),
        "{said}"
    );
    let (ok, said) = up(&text(NEWEST_CHECKED));
    assert!(ok, "{said}");
    assert!(!said.contains("newest version the probes"), "{said}");
}

#[test]
fn up_refuses_below_the_lowest_and_starts_nothing() {
    let below = text((LOWEST.0, LOWEST.1, LOWEST.2 - 1));
    let (ok, said) = up(&below);
    assert!(!ok, "{said}");
    assert!(said.contains(&below) && said.contains(SUPPORTED), "{said}");
}
