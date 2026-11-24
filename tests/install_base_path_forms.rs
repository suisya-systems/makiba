//! #382, beside tests/install_base_layout.rs: the check of scripts/install-base.sh also sees an
//! `hc` found through a relative PATH entry, and an HC_BIN that is set but empty.

use makiba::base::version as base_version;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

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

/// A scratch folder holding a release (every target pointing at one tar.gz whose `herdr-core`
/// prints "new"). Returns (folder, releases url).
fn release(name: &str) -> (PathBuf, String) {
    let dir = std::env::temp_dir().join(format!("makiba-382f-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let version_dir = dir.join("releases/core").join(base_version::PINNED);
    let src = dir.join("src");
    fs::create_dir_all(&version_dir).unwrap();
    fs::create_dir_all(&src).unwrap();
    let entries = ["herdr-core", "LICENSE", "THIRD-PARTY-NOTICES"];
    for e in entries {
        fs::write(src.join(e), "#!/bin/sh\necho new\n").unwrap();
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
    let sha = sha256(&file);
    let targets = [
        "linux-x86_64",
        "linux-aarch64",
        "macos-aarch64",
        "macos-x86_64",
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
    let url = format!("file://{}", dir.join("releases").display());
    (dir, url)
}

fn install(dir: &Path, url: &str, path: &str, hc_bin: Option<&str>) -> Output {
    let mut cmd = Command::new("bash");
    cmd.arg(root().join("scripts/install-base.sh"))
        .arg(dir.join("prefix"))
        .current_dir(dir)
        .env("HERDR_RELEASES_URL", url)
        .env("PATH", path)
        .env_remove("HC_BIN");
    if let Some(h) = hc_bin {
        cmd.env("HC_BIN", h);
    }
    cmd.output().expect("bash runs")
}

fn own_path() -> String {
    std::env::var("PATH").unwrap_or_default()
}

#[test]
fn an_hc_found_through_a_relative_path_entry_is_named() {
    let (dir, url) = release("relative");
    fs::create_dir_all(dir.join("old")).unwrap();
    let old = dir.join("old/hc");
    fs::write(&old, "#!/bin/sh\necho old\n").unwrap();
    fs::set_permissions(&old, fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("./old:{}:{}", dir.join("prefix/bin").display(), own_path());
    let out = install(&dir, &url, &path, None);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(3), "{err}");
    assert!(err.contains("./old/hc"), "{err}");
}

#[test]
fn an_empty_hc_bin_is_named() {
    let (dir, url) = release("empty-hc-bin");
    let path = format!("{}:{}", dir.join("prefix/bin").display(), own_path());
    let out = install(&dir, &url, &path, Some(""));
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(3), "{err}");
    assert!(err.contains("HC_BIN is set and empty"), "{err}");
}

/// A bare HC_BIN that PATH does not find is named, even when the current folder holds a file of
/// that name (makiba looks a bare name up on PATH, never in the current folder).
#[test]
fn a_bare_hc_bin_not_on_path_is_named() {
    let (dir, url) = release("bare-hc-bin");
    let bin = dir.join("prefix/bin");
    fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink("herdr-core", bin.join("makiba-382-absent")).unwrap();
    let path = format!("{}:{}", dir.join("empty").display(), own_path());
    let out = Command::new("bash")
        .arg(root().join("scripts/install-base.sh"))
        .arg(dir.join("prefix"))
        .current_dir(&bin)
        .env("HERDR_RELEASES_URL", &url)
        .env("PATH", path)
        .env("HC_BIN", "makiba-382-absent")
        .output()
        .expect("bash runs");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("which PATH does not find"), "{err}");
    assert_eq!(out.status.code(), Some(3), "{err}");
}

#[test]
fn hc_bin_naming_the_installed_link_passes() {
    let (dir, url) = release("hc-bin-ok");
    let path = format!("{}:{}", dir.join("prefix/bin").display(), own_path());
    let hc = dir.join("prefix/bin/hc");
    let out = install(&dir, &url, &path, Some(&hc.display().to_string()));
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
