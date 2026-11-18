//! M0-2: the pinned base version, the supported Claude Code versions, and the install script.

use makiba::agents::version as agent_version;
use makiba::base::version as base_version;
use makiba::base::{BaseError, BaseInfo};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn doc(name: &str) -> String {
    fs::read_to_string(root().join("docs").join(name)).expect("the document exists")
}

fn info(version: &str) -> Result<BaseInfo, BaseError> {
    Ok(BaseInfo {
        name: "herdr-core".into(),
        version: version.into(),
    })
}

#[test]
fn a_base_version_inside_the_range_is_accepted() {
    assert!(base_version::check("0.1.21").is_ok());
    assert_eq!(base_version::PINNED, "0.1.21");
}

#[test]
fn a_base_version_outside_the_range_is_refused_naming_both() {
    for found in [
        "0.1.20",
        "0.1.22",
        "0.2.0",
        "1.1.21",
        "0.1.21-rc1",
        "0.1",
        "",
        "junk",
    ] {
        match base_version::check(found) {
            Err(BaseError::UnsupportedVersion {
                found: f,
                supported,
            }) => {
                assert_eq!(f, found);
                assert_eq!(supported, base_version::SUPPORTED);
                let text = BaseError::UnsupportedVersion {
                    found: f,
                    supported,
                }
                .to_string();
                assert!(
                    text.contains(found) && text.contains(">=0.1.21,<0.1.22"),
                    "{text}"
                );
            }
            other => panic!("{found:?} must be refused, got {other:?}"),
        }
    }
}

#[test]
fn a_refused_version_stops_before_any_other_call() {
    let mut later_calls = 0;
    let mut run = |version: &str| {
        base_version::checked_hello(|| info(version))?;
        later_calls += 1; // stands for the first call after hello
        Ok::<(), BaseError>(())
    };
    assert!(run("0.1.22").is_err());
    assert!(run("0.1.21").is_ok());
    assert_eq!(later_calls, 1);
}

#[test]
fn upgrading_the_base_names_the_test_and_the_probes() {
    let text = doc("upgrading-the-base.md");
    for needle in ["tests/base_compat.rs", "P1", "P3", "P4", "P5", "P8"] {
        assert!(
            text.contains(needle),
            "docs/upgrading-the-base.md must name {needle}"
        );
    }
}

#[test]
fn a_claude_version_inside_the_range_is_accepted() {
    assert!(agent_version::check("2.1.326 (Claude Code)").is_ok());
    assert!(agent_version::check("2.1.326\n").is_ok());
}

#[test]
fn a_claude_version_outside_the_range_or_unparsable_is_refused() {
    for text in [
        "2.1.318 (Claude Code)",
        "2.1.327 (Claude Code)",
        "3.0.0",
        "",
        "claude",
        "2.1",
    ] {
        let err = agent_version::check(text).expect_err(text).to_string();
        assert!(
            err.contains(&format!("{text:?}")) || err.contains(text),
            "{err}"
        );
        assert!(err.contains(agent_version::SUPPORTED), "{err}");
    }
}

#[test]
fn upgrading_claude_code_names_each_probe_and_the_runbook() {
    let text = doc("upgrading-claude-code.md");
    for needle in [
        "P1",
        "P3",
        "P4",
        "P5",
        "P6",
        "P7",
        "P8",
        "P9",
        "P10",
        "P11",
        "paddock",
        "M4-5",
        "docs/runbooks/first-worker.md",
    ] {
        assert!(
            text.contains(needle),
            "docs/upgrading-claude-code.md must name {needle}"
        );
    }
}

// --- scripts/install-base.sh ---

fn sha256(file: &Path) -> String {
    let out = Command::new("sha256sum")
        .arg(file)
        .output()
        .or_else(|_| {
            Command::new("shasum")
                .args(["-a", "256"])
                .arg(file)
                .output()
        })
        .expect("sha256sum or shasum");
    String::from_utf8(out.stdout)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_string()
}

/// A release folder holding a manifest for every target and one file, with `sha` as the
/// manifest's hash of it. Returns (releases url, prefix).
fn release(name: &str, sha: Option<&str>) -> (String, PathBuf) {
    let dir = std::env::temp_dir().join(format!("makiba-m0-2-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let version_dir = dir.join("releases/core").join(base_version::PINNED);
    fs::create_dir_all(&version_dir).unwrap();
    let file = version_dir.join("hc-download");
    fs::write(&file, "#!/bin/sh\necho fake hc\n").unwrap();
    let sha = sha.map(str::to_string).unwrap_or_else(|| sha256(&file));
    let targets = [
        "linux-x86_64",
        "linux-aarch64",
        "macos-aarch64",
        "macos-x86_64",
        "windows-x86_64",
    ]
    .map(|t| {
        format!(
            r#""{t}": {{"url": "file://{}", "sha256": "{sha}"}}"#,
            file.display()
        )
    })
    .join(",");
    fs::write(
        version_dir.join("manifest.json"),
        format!(r#"{{"targets": {{{targets}}}}}"#),
    )
    .unwrap();
    (
        format!("file://{}", dir.join("releases").display()),
        dir.join("prefix"),
    )
}

fn install(url: &str, prefix: &Path) -> std::process::Output {
    Command::new("bash")
        .arg(root().join("scripts/install-base.sh"))
        .arg(prefix)
        .env("HERDR_RELEASES_URL", url)
        .output()
        .expect("bash runs")
}

#[test]
fn install_base_installs_the_file_when_the_hash_matches() {
    let (url, prefix) = release("match", None);
    let out = install(&url, &prefix);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let hc = prefix.join("bin/hc");
    assert_eq!(fs::read_to_string(hc).unwrap(), "#!/bin/sh\necho fake hc\n");
}

#[test]
fn install_base_installs_nothing_and_fails_when_the_hash_differs() {
    let (url, prefix) = release("mismatch", Some(&"0".repeat(64)));
    let out = install(&url, &prefix);
    assert!(!out.status.success());
    assert!(!prefix.join("bin/hc").exists());
}
