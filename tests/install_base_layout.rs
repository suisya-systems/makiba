//! #382: scripts/install-base.sh installs the base in its own layout, the archive's `herdr-core`
//! as PREFIX/bin/herdr-core with `hc` a link to it, and says so when HC_BIN or PATH would find
//! another `herdr-core` or `hc` first (exit 3), so that an install leaves no older server to be
//! started.

use makiba::base::version as base_version;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const NEW: &str = "#!/bin/sh\necho new\n";
const OLD: &str = "#!/bin/sh\necho old\n";

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

/// A scratch folder for one test, empty.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("makiba-382-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// A release folder under `dir` holding a manifest for every target and one tar.gz whose
/// `herdr-core` prints "new". Returns the releases url.
fn release(dir: &Path) -> String {
    let version_dir = dir.join("releases/core").join(base_version::PINNED);
    let src = dir.join("src");
    fs::create_dir_all(&version_dir).unwrap();
    fs::create_dir_all(&src).unwrap();
    let entries = ["herdr-core", "LICENSE", "THIRD-PARTY-NOTICES"];
    for e in entries {
        let body = if e == "herdr-core" {
            NEW.to_string()
        } else {
            format!("{e}\n")
        };
        fs::write(src.join(e), body).unwrap();
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
    format!("file://{}", dir.join("releases").display())
}

/// An executable script `name` in `folder` that prints "old".
fn old_program(folder: &Path, name: &str) -> PathBuf {
    fs::create_dir_all(folder).unwrap();
    let p = folder.join(name);
    fs::write(&p, OLD).unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    p
}

/// PATH made of `first` folders, then this process's own PATH.
fn path_with(first: &[&Path]) -> String {
    let mut parts: Vec<String> = first.iter().map(|p| p.display().to_string()).collect();
    parts.push(std::env::var("PATH").unwrap_or_default());
    parts.join(":")
}

fn install(url: &str, prefix: &Path, path: &str, hc_bin: Option<&Path>) -> Output {
    let mut cmd = Command::new("bash");
    cmd.arg(root().join("scripts/install-base.sh"))
        .arg(prefix)
        .env("HERDR_RELEASES_URL", url)
        .env("PATH", path)
        .env_remove("HC_BIN");
    if let Some(h) = hc_bin {
        cmd.env("HC_BIN", h);
    }
    cmd.output().expect("bash runs")
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn install_puts_herdr_core_beside_hc_and_hc_links_to_it() {
    let dir = scratch("layout");
    let url = release(&dir);
    let prefix = dir.join("prefix");
    let bin = prefix.join("bin");
    old_program(&bin, "herdr-core");
    old_program(&bin, "hc");
    let out = install(&url, &prefix, &path_with(&[&bin]), None);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let core = bin.join("herdr-core");
    assert_eq!(fs::read_to_string(&core).unwrap(), NEW);
    assert_eq!(
        fs::metadata(&core).unwrap().permissions().mode() & 0o777,
        0o755
    );
    let hc = bin.join("hc");
    assert!(fs::symlink_metadata(&hc).unwrap().file_type().is_symlink());
    assert_eq!(fs::read_link(&hc).unwrap(), PathBuf::from("herdr-core"));
    assert_eq!(fs::read_to_string(&hc).unwrap(), NEW);
}

#[test]
fn install_fails_when_an_older_herdr_core_is_found_first() {
    let dir = scratch("old-core");
    let url = release(&dir);
    let prefix = dir.join("prefix");
    let old = dir.join("old");
    let found = old_program(&old, "herdr-core");
    let out = install(
        &url,
        &prefix,
        &path_with(&[&old, &prefix.join("bin")]),
        None,
    );
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert!(
        stderr(&out).contains(&found.display().to_string()),
        "stderr names {}: {}",
        found.display(),
        stderr(&out)
    );
    assert_eq!(
        fs::read_to_string(prefix.join("bin/herdr-core")).unwrap(),
        NEW
    );
}

#[test]
fn install_fails_when_another_hc_is_found_first() {
    let dir = scratch("old-hc");
    let url = release(&dir);
    let prefix = dir.join("prefix");
    let old = dir.join("old");
    let found = old_program(&old, "hc");
    let out = install(
        &url,
        &prefix,
        &path_with(&[&old, &prefix.join("bin")]),
        None,
    );
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert!(
        stderr(&out).contains(&found.display().to_string()),
        "stderr names {}: {}",
        found.display(),
        stderr(&out)
    );
}

#[test]
fn install_fails_when_hc_bin_names_another_program() {
    let dir = scratch("hc-bin");
    let url = release(&dir);
    let prefix = dir.join("prefix");
    let found = old_program(&dir.join("old"), "hc");
    let out = install(
        &url,
        &prefix,
        &path_with(&[&prefix.join("bin")]),
        Some(&found),
    );
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert!(
        stderr(&out).contains(&found.display().to_string()),
        "stderr names {}: {}",
        found.display(),
        stderr(&out)
    );
}

#[test]
fn install_refuses_a_directory_in_place_of_hc() {
    let dir = scratch("hc-dir");
    let url = release(&dir);
    let prefix = dir.join("prefix");
    let bin = prefix.join("bin");
    fs::create_dir_all(bin.join("hc")).unwrap();
    let out = install(&url, &prefix, &path_with(&[&bin]), None);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(!bin.join("herdr-core").exists());
    assert!(bin.join("hc").is_dir());
    assert_eq!(fs::read_dir(bin.join("hc")).unwrap().count(), 0);
}
