//! M0-2: the pinned base version, the supported Claude Code versions, and the install script.

use makiba::agents::version as agent_version;
use makiba::base::version as base_version;
use makiba::base::{BaseError, BaseInfo};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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
    assert!(base_version::check("0.1.22").is_ok());
    assert_eq!(base_version::PINNED, "0.1.22");
}

#[test]
fn a_base_version_outside_the_range_is_refused_naming_both() {
    for found in [
        "0.1.21",
        "0.1.23",
        "0.2.0",
        "1.1.22",
        "0.1.22-rc1",
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
                    text.contains(found) && text.contains(">=0.1.22,<0.1.23"),
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
    assert!(run("0.1.23").is_err());
    assert!(run("0.1.22").is_ok());
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
    let (a, b, c) = agent_version::LOWEST;
    assert!(agent_version::check(&format!("{a}.{b}.{c} (Claude Code)")).is_ok());
    assert!(agent_version::check(&format!("{a}.{b}.{c}\n")).is_ok());
}

#[test]
fn a_claude_version_below_the_range_or_unparsable_is_refused() {
    let lo = agent_version::LOWEST;
    let below = format!("{}.{}.{} (Claude Code)", lo.0, lo.1, lo.2 - 1);
    for text in [below.as_str(), "1.0.0", "", "claude", "2.1"] {
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
    release_at(name, base_version::PINNED, entries, sha)
}

/// As `release`, with the manifest under `version` instead of `PINNED`.
fn release_at(name: &str, version: &str, entries: &[&str], sha: Option<&str>) -> (String, PathBuf) {
    let dir = std::env::temp_dir().join(format!("makiba-m0-2-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let version_dir = dir.join("releases/core").join(version);
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

/// PREFIX/bin comes first on PATH and HC_BIN is removed, so that an hc of the machine running the
/// test is not found ahead of the one installed (the script exits 3 then: #382). MAKIBA_BASE_PINNED
/// is removed, so that one exported by the person does not replace the checkout's PINNED (#419).
fn install(url: &str, prefix: &Path) -> Output {
    let path = format!(
        "{}:{}",
        prefix.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    Command::new("bash")
        .arg(root().join("scripts/install-base.sh"))
        .arg(prefix)
        .env("HERDR_RELEASES_URL", url)
        .env("PATH", path)
        .env_remove("HC_BIN")
        .env_remove("MAKIBA_BASE_PINNED")
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

// --- MAKIBA_BASE_PINNED (#419): an installed makiba has no checkout ---

const HERDR_CORE: &str = "#!/bin/sh\necho herdr-core\n";
const OLD: &str = "#!/bin/sh\necho old\n";

/// A copy of scripts/install-base.sh alone in a scratch folder: no src/base/version.rs beside it.
fn lone_script(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("makiba-419-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("scripts")).unwrap();
    let script = dir.join("scripts/install-base.sh");
    fs::copy(root().join("scripts/install-base.sh"), &script).unwrap();
    assert!(!dir.join("src/base/version.rs").exists());
    script
}

/// Runs `script PREFIX` with HERDR_RELEASES_URL set to `url`, PATH the folders of `path_front`
/// then the test's own PATH, HC_BIN and MAKIBA_BASE_PINNED removed unless `envs` gives them.
fn run_script(
    script: &Path,
    url: &str,
    prefix: &Path,
    path_front: &[&Path],
    envs: &[(&str, &str)],
) -> Output {
    let mut path: Vec<String> = path_front.iter().map(|p| p.display().to_string()).collect();
    path.push(std::env::var("PATH").unwrap_or_default());
    let mut cmd = Command::new("bash");
    cmd.arg(script)
        .arg(prefix)
        .env("HERDR_RELEASES_URL", url)
        .env("PATH", path.join(":"))
        .env_remove("HC_BIN")
        .env_remove("MAKIBA_BASE_PINNED");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.output().expect("bash runs")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn install_base_takes_the_version_from_makiba_base_pinned_without_a_checkout() {
    let (url, prefix) = release_at("419-var", "9.8.7", &FULL, None);
    let script = lone_script("var");
    let out = run_script(
        &script,
        &url,
        &prefix,
        &[&prefix.join("bin")],
        &[("MAKIBA_BASE_PINNED", "9.8.7")],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(
        fs::read_to_string(prefix.join("bin/hc")).unwrap(),
        HERDR_CORE
    );
    assert!(
        text(&out.stdout).contains("herdr-core 9.8.7"),
        "{}",
        text(&out.stdout)
    );
}

#[test]
fn install_base_prefers_makiba_base_pinned_over_the_checkout() {
    // Only 9.8.7 has a manifest: reading PINNED from the checkout would fail.
    let (url, prefix) = release_at("419-over", "9.8.7", &FULL, None);
    let script = root().join("scripts/install-base.sh");
    let out = run_script(
        &script,
        &url,
        &prefix,
        &[&prefix.join("bin")],
        &[("MAKIBA_BASE_PINNED", "9.8.7")],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(prefix.join("bin/hc").exists());
    assert!(
        text(&out.stdout).contains("herdr-core 9.8.7"),
        "{}",
        text(&out.stdout)
    );
}

#[test]
fn install_base_refuses_a_makiba_base_pinned_that_is_not_plain() {
    // A release at PINNED exists, so falling back to the checkout would install.
    let (url, prefix) = release("419-plain", &FULL, None);
    let script = root().join("scripts/install-base.sh");
    for value in ["", "0.1.22-rc1", "../0.1.22", "0.1", "0.1.22 "] {
        let out = run_script(
            &script,
            &url,
            &prefix,
            &[&prefix.join("bin")],
            &[("MAKIBA_BASE_PINNED", value)],
        );
        assert_eq!(
            out.status.code(),
            Some(1),
            "{value:?}: {}",
            text(&out.stderr)
        );
        assert!(
            !prefix.join("bin/hc").exists() && !prefix.join("bin/herdr-core").exists(),
            "{value:?} installed something"
        );
        assert!(
            text(&out.stderr).contains("MAKIBA_BASE_PINNED"),
            "{value:?}: {}",
            text(&out.stderr)
        );
    }
}

#[test]
fn install_base_without_the_variable_or_a_checkout_installs_nothing() {
    let (url, prefix) = release("419-none", &FULL, None);
    let script = lone_script("none");
    let out = run_script(&script, &url, &prefix, &[&prefix.join("bin")], &[]);
    assert!(!out.status.success());
    assert!(
        text(&out.stderr).contains("src/base/version.rs"),
        "{}",
        text(&out.stderr)
    );
    assert!(!prefix.join("bin/hc").exists());
}

#[test]
fn install_base_for_staging_passes_its_check_beside_the_person_install() {
    use std::os::unix::fs::PermissionsExt;
    let (url, staging) = release_at("419-staging", "9.8.7", &FULL, None);
    let other = staging.parent().unwrap().join("other/bin");
    fs::create_dir_all(&other).unwrap();
    for name in ["hc", "herdr-core"] {
        let file = other.join(name);
        fs::write(&file, OLD).unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let other_hc = other.join("hc").display().to_string();
    let script = lone_script("staging");

    // The person's environment: HC_BIN and PATH find the existing install.
    let out = run_script(
        &script,
        &url,
        &staging,
        &[&other],
        &[("HC_BIN", &other_hc), ("MAKIBA_BASE_PINNED", "9.8.7")],
    );
    assert_eq!(out.status.code(), Some(3), "{}", text(&out.stderr));

    // The staging environment: HC_BIN removed, the staging bin first on PATH.
    let out = run_script(
        &script,
        &url,
        &staging,
        &[&staging.join("bin"), &other],
        &[("MAKIBA_BASE_PINNED", "9.8.7")],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(
        fs::read_to_string(staging.join("bin/hc")).unwrap(),
        HERDR_CORE
    );
    for name in ["hc", "herdr-core"] {
        assert_eq!(fs::read_to_string(other.join(name)).unwrap(), OLD);
    }
}
