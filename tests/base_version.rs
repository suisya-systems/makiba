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
        "2.1.329 (Claude Code)",
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

/// A release folder holding a manifest for every target and one tar.gz built here, with `sha` as
/// the manifest's hash of it (the real hash when None). The archive holds the files named in
/// `entries` at its root, each containing its own name. Returns (releases url, prefix).
fn release(name: &str, entries: &[&str], sha: Option<&str>) -> (String, PathBuf) {
    let dir = std::env::temp_dir().join(format!("makiba-m0-2-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let version_dir = dir.join("releases/core").join(base_version::PINNED);
    let src = dir.join("src");
    fs::create_dir_all(&version_dir).unwrap();
    fs::create_dir_all(&src).unwrap();
    for e in entries {
        fs::write(src.join(e), format!("#!/bin/sh\necho {e}\n")).unwrap();
    }
    let file = version_dir.join("herdr-core-test.tar.gz");
    let status = Command::new("tar")
        .arg("czf")
        .arg(&file)
        .arg("-C")
        .arg(&src)
        .args(entries)
        .status()
        .expect("tar runs");
    assert!(status.success());
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

const FULL: [&str; 3] = ["herdr-core", "LICENSE", "THIRD-PARTY-NOTICES"];

fn install(url: &str, prefix: &Path) -> std::process::Output {
    Command::new("bash")
        .arg(root().join("scripts/install-base.sh"))
        .arg(prefix)
        .env("HERDR_RELEASES_URL", url)
        .output()
        .expect("bash runs")
}

#[test]
fn install_base_installs_herdr_core_from_the_archive_as_hc() {
    let (url, prefix) = release("match", &FULL, None);
    let out = install(&url, &prefix);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let hc = prefix.join("bin/hc");
    assert_eq!(
        fs::read_to_string(&hc).unwrap(),
        "#!/bin/sh\necho herdr-core\n"
    );
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        fs::metadata(&hc).unwrap().permissions().mode() & 0o777,
        0o755
    );
    assert!(!prefix.join("bin/LICENSE").exists());
}

#[test]
fn install_base_installs_nothing_and_fails_when_the_hash_differs() {
    let (url, prefix) = release("mismatch", &FULL, Some(&"0".repeat(64)));
    let out = install(&url, &prefix);
    assert!(!out.status.success());
    assert!(!prefix.join("bin/hc").exists());
}

#[test]
fn install_base_installs_nothing_and_fails_when_the_archive_lacks_herdr_core() {
    let (url, prefix) = release("no-binary", &["LICENSE", "THIRD-PARTY-NOTICES"], None);
    let out = install(&url, &prefix);
    assert!(!out.status.success());
    assert!(!prefix.join("bin/hc").exists());
}
