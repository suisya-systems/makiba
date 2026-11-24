//! Issue 62 (goal G10, "It installs as one binary"): releasing makiba as one binary per target, with a
//! manifest and an installer.
//!
//! What runs here needs no network and publishes nothing. `scripts/install.sh` runs against a release
//! folder made in a temporary directory and read through `file://`, the manifest made by
//! `scripts/release-manifest.sh`, a fake `uname` first in PATH to pick the host. The rows prove: a good
//! archive installs and prints its version; an altered archive, a manifest without the host's target,
//! swapped entries, another version and a bad hash are each refused with the exit code the documentation
//! names and nothing placed; the workflow's list of targets, the installer's, the manifest script's and
//! the documentation's are the same four; the tag and Cargo.toml must agree; the notices file names each
//! crate of the locked dependency tree. What only a real release proves (the four builds on their
//! runners, GitHub's `latest` redirect, the token's right to create a release, a clean host) is checked by
//! the owner once, as `docs/installing.md` says.

mod common;

use regex::Regex;
use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const MAKIBA: &str = env!("CARGO_BIN_EXE_makiba");
const VERSION: &str = "1.2.3";
const TARGETS: [&str; 4] = [
    "linux-x86_64",
    "linux-aarch64",
    "macos-aarch64",
    "macos-x86_64",
];

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    fs::read_to_string(root().join(rel)).unwrap_or_else(|e| panic!("cannot read {rel}: {e}"))
}

fn code(o: &Output) -> i32 {
    o.status.code().expect("exited normally")
}

fn text(o: &Output) -> String {
    format!(
        "exit {:?}\nstdout: {}\nstderr: {}",
        o.status.code(),
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// A scratch place: the release folder, the install prefix, TMPDIR for the installer, and a folder
/// holding the fake `uname`.
struct Env {
    dir: tempfile::TempDir,
}

fn env() -> Env {
    let e = Env {
        dir: tempfile::tempdir().unwrap(),
    };
    for d in ["rel", "prefix", "scratch", "shim"] {
        fs::create_dir(e.dir.path().join(d)).unwrap();
    }
    e
}

fn write_exec(path: &Path, body: &str) {
    common::exe::write(path, body);
}

fn sh(args: &[&str]) -> Output {
    Command::new("sh")
        .args(args)
        .current_dir(root())
        .output()
        .expect("sh runs")
}

impl Env {
    fn p(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }
    fn vdir(&self, version: &str) -> PathBuf {
        self.p(&format!("rel/download/v{version}"))
    }
    fn manifest(&self, version: &str) -> PathBuf {
        self.vdir(version).join("manifest.json")
    }
    fn archive(&self, version: &str, target: &str) -> PathBuf {
        self.vdir(version)
            .join(format!("makiba-{version}-{target}.tar.gz"))
    }

    /// A release of `version`: one archive per target holding `makiba` (a script that prints
    /// `makiba <version>` for --version and the name of its target otherwise; `body` may replace it)
    /// when `binary`, and LICENSE; the manifest made by release-manifest.sh, put at both places the
    /// installer looks.
    fn release_with(&self, version: &str, binary: bool, body: &dyn Fn(&str) -> String) {
        let v = self.vdir(version);
        fs::create_dir_all(&v).unwrap();
        for t in TARGETS {
            let stage = self.p(&format!("stage-{t}"));
            let _ = fs::remove_dir_all(&stage);
            fs::create_dir(&stage).unwrap();
            fs::write(stage.join("LICENSE"), "licence\n").unwrap();
            let mut members = vec!["LICENSE"];
            if binary {
                write_exec(&stage.join("makiba"), &body(t));
                members.insert(0, "makiba");
            }
            let st = Command::new("tar")
                .arg("czf")
                .arg(self.archive(version, t))
                .arg("-C")
                .arg(&stage)
                .args(&members)
                .status()
                .expect("tar runs");
            assert!(st.success());
        }
        let out = sh(&[
            "scripts/release-manifest.sh",
            "make",
            version,
            &format!("file://{}", v.display()),
            v.to_str().unwrap(),
        ]);
        assert_eq!(code(&out), 0, "{}", text(&out));
        let latest = self.p("rel/latest/download");
        fs::create_dir_all(&latest).unwrap();
        // not an executable: the release manifest, read and never run
        fs::copy(self.manifest(version), latest.join("manifest.json")).unwrap();
    }

    fn release(&self, version: &str) {
        let ver = version.to_string();
        self.release_with(version, true, &move |t| fake_makiba(&ver, t));
    }

    /// Replaces text in the manifest of `version`, and in the latest one.
    fn edit_manifest(&self, version: &str, from: &str, to: &str) {
        for m in [
            self.manifest(version),
            self.p("rel/latest/download/manifest.json"),
        ] {
            let s = fs::read_to_string(&m).unwrap();
            assert!(s.contains(from), "{from} is not in {s}");
            fs::write(&m, s.replacen(from, to, 1)).unwrap();
        }
    }

    fn uname(&self, s: &str, m: &str) {
        write_exec(
            &self.p("shim/uname"),
            &format!("#!/bin/sh\ncase \"$1\" in -s) echo '{s}' ;; -m) echo '{m}' ;; esac\n"),
        );
    }

    /// install.sh as a host of kind `uname` would run it, against the release folder.
    fn install(&self, uname: (&str, &str), args: &[&str]) -> Output {
        self.uname(uname.0, uname.1);
        let path = format!(
            "{}:{}",
            self.p("shim").display(),
            std::env::var("PATH").unwrap()
        );
        Command::new("sh")
            .arg(root().join("scripts/install.sh"))
            .args(["--prefix", self.p("prefix").to_str().unwrap()])
            .args(args)
            .env("PATH", path)
            .env("TMPDIR", self.p("scratch"))
            .env(
                "MAKIBA_RELEASES_URL",
                format!("file://{}", self.p("rel").display()),
            )
            .output()
            .expect("sh runs")
    }

    fn linux(&self, args: &[&str]) -> Output {
        self.install(("Linux", "x86_64"), args)
    }

    fn installed(&self) -> PathBuf {
        self.p("prefix/bin/makiba")
    }

    /// A refusal leaves nothing: no file in the prefix's bin, no temporary file of the installer.
    fn assert_nothing_placed(&self) {
        let bin = self.p("prefix/bin");
        let left: Vec<_> = fs::read_dir(&bin)
            .map(|d| d.map(|e| e.unwrap().file_name()).collect())
            .unwrap_or_default();
        assert!(left.is_empty(), "left in {}: {left:?}", bin.display());
        let tmp: Vec<_> = fs::read_dir(self.p("scratch"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert!(tmp.is_empty(), "the installer left {tmp:?} in TMPDIR");
    }
}

type Body<'a> = &'a dyn Fn(&str) -> String;

fn fake_makiba(version: &str, target: &str) -> String {
    format!(
        "#!/bin/sh\ncase \"$1\" in --version) echo 'makiba {version}' ;; *) echo 'target {target}' ;; esac\n"
    )
}

// --- the installer ---

#[test]
fn installs_a_good_archive_from_the_latest_and_from_a_named_version() {
    for args in [&[][..], &["--version", VERSION][..]] {
        let e = env();
        e.release(VERSION);
        let out = e.linux(args);
        assert_eq!(code(&out), 0, "{}", text(&out));
        assert!(
            String::from_utf8_lossy(&out.stdout).contains(&format!("makiba {VERSION}")),
            "{}",
            text(&out)
        );
        let mode = fs::metadata(e.installed()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755);
        assert!(
            fs::read_to_string(e.installed())
                .unwrap()
                .contains("target linux-x86_64")
        );
        let run = Command::new(e.installed())
            .arg("--version")
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&run.stdout),
            format!("makiba {VERSION}\n")
        );
        assert_eq!(fs::read_dir(e.p("prefix/bin")).unwrap().count(), 1);
        assert_eq!(fs::read_dir(e.p("scratch")).unwrap().count(), 0);
    }
}

#[test]
fn refuses_an_archive_altered_after_the_manifest_was_made() {
    let e = env();
    e.release(VERSION);
    let a = e.archive(VERSION, "linux-x86_64");
    let mut bytes = fs::read(&a).unwrap();
    bytes.push(0);
    fs::write(&a, bytes).unwrap();
    let out = e.linux(&["--version", VERSION]);
    assert_eq!(code(&out), 6, "{}", text(&out));
    e.assert_nothing_placed();
}

#[test]
fn refuses_a_manifest_without_the_hosts_target() {
    let e = env();
    e.release(VERSION);
    e.edit_manifest(VERSION, "\"linux-x86_64\"", "\"linux-riscv64\"");
    let out = e.linux(&["--version", VERSION]);
    assert_eq!(code(&out), 5, "{}", text(&out));
    assert!(String::from_utf8_lossy(&out.stderr).contains("linux-x86_64"));
    e.assert_nothing_placed();
}

#[test]
fn picks_the_target_of_each_host() {
    let hosts = [
        ("Linux", "x86_64", "linux-x86_64"),
        ("Linux", "aarch64", "linux-aarch64"),
        ("Linux", "arm64", "linux-aarch64"),
        ("Darwin", "arm64", "macos-aarch64"),
        ("Darwin", "aarch64", "macos-aarch64"),
        ("Darwin", "x86_64", "macos-x86_64"),
    ];
    for (s, m, target) in hosts {
        let e = env();
        e.release(VERSION);
        let out = e.install((s, m), &[]);
        assert_eq!(code(&out), 0, "{s} {m}: {}", text(&out));
        assert!(
            fs::read_to_string(e.installed())
                .unwrap()
                .contains(&format!("target {target}")),
            "{s} {m} installed the archive of another target"
        );
    }
}

#[test]
fn refuses_a_host_that_has_no_release() {
    for (s, m) in [
        ("Linux", "riscv64"),
        ("Linux", "i686"),
        ("Darwin", "i386"),
        ("MINGW64_NT-10.0", "x86_64"),
        ("FreeBSD", "amd64"),
    ] {
        let e = env();
        e.release(VERSION);
        let out = e.install((s, m), &[]);
        assert_eq!(code(&out), 3, "{s} {m}: {}", text(&out));
        e.assert_nothing_placed();
    }
}

#[test]
fn refuses_a_manifest_of_another_version() {
    let e = env();
    e.release(VERSION);
    e.edit_manifest(
        VERSION,
        &format!("\"version\": \"{VERSION}\""),
        "\"version\": \"1.2.4\"",
    );
    let out = e.linux(&["--version", VERSION]);
    assert_eq!(code(&out), 5, "{}", text(&out));
    e.assert_nothing_placed();
}

#[test]
fn refuses_a_sha256_that_is_not_64_hex_digits() {
    let e = env();
    e.release(VERSION);
    let m = fs::read_to_string(e.manifest(VERSION)).unwrap();
    let sha = Regex::new(r#""sha256": "([0-9a-f]{64})""#)
        .unwrap()
        .captures(&m)
        .unwrap()[1]
        .to_string();
    e.edit_manifest(VERSION, &sha, "abc123");
    let out = e.linux(&["--version", VERSION]);
    assert_eq!(code(&out), 5, "{}", text(&out));
    e.assert_nothing_placed();
}

#[test]
fn refuses_entries_whose_archives_are_swapped() {
    let e = env();
    e.release(VERSION);
    let a = format!("makiba-{VERSION}-linux-x86_64.tar.gz");
    let b = format!("makiba-{VERSION}-linux-aarch64.tar.gz");
    e.edit_manifest(VERSION, &a, "SWAP");
    e.edit_manifest(VERSION, &b, &a);
    e.edit_manifest(VERSION, "SWAP", &b);
    let out = e.linux(&["--version", VERSION]);
    assert_eq!(code(&out), 5, "{}", text(&out));
    e.assert_nothing_placed();
}

#[test]
fn refuses_an_archive_without_makiba() {
    let e = env();
    e.release_with(VERSION, false, &|_| String::new());
    let out = e.linux(&["--version", VERSION]);
    assert_eq!(code(&out), 7, "{}", text(&out));
    e.assert_nothing_placed();
}

#[test]
fn refuses_a_binary_that_does_not_print_exactly_the_manifests_version() {
    let bodies: [(&str, Body); 3] = [
        ("a longer version", &|_| fake_makiba("1.2.30", "x")),
        ("another version", &|_| fake_makiba("1.2.4", "x")),
        ("a binary that fails", &|_| {
            "#!/bin/sh\nexit 1\n".to_string()
        }),
    ];
    for (name, body) in bodies {
        let e = env();
        e.release_with(VERSION, true, body);
        let out = e.linux(&["--version", VERSION]);
        assert_eq!(code(&out), 7, "{name}: {}", text(&out));
        e.assert_nothing_placed();
    }
}

#[test]
fn refuses_when_the_manifest_cannot_be_fetched() {
    let e = env();
    e.release(VERSION);
    let out = e.linux(&["--version", "9.9.9"]);
    assert_eq!(code(&out), 4, "{}", text(&out));
    e.assert_nothing_placed();
}

#[test]
fn replaces_an_installed_binary_and_keeps_it_when_a_refusal_comes() {
    let e = env();
    e.release(VERSION);
    fs::create_dir_all(e.p("prefix/bin")).unwrap();
    write_exec(&e.installed(), "old\n");
    let a = e.archive(VERSION, "linux-x86_64");
    let good = fs::read(&a).unwrap();
    let mut bad = good.clone();
    bad.push(1);
    fs::write(&a, bad).unwrap();
    let out = e.linux(&["--version", VERSION]);
    assert_eq!(code(&out), 6, "{}", text(&out));
    assert_eq!(fs::read_to_string(e.installed()).unwrap(), "old\n");
    fs::write(&a, good).unwrap();
    let out = e.linux(&["--version", VERSION]);
    assert_eq!(code(&out), 0, "{}", text(&out));
    assert!(
        fs::read_to_string(e.installed())
            .unwrap()
            .contains("target linux-x86_64")
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("makiba down"));
    assert_eq!(fs::read_dir(e.p("prefix/bin")).unwrap().count(), 1);
}

#[test]
fn refuses_when_the_destination_is_a_directory_or_cannot_be_written() {
    let e = env();
    e.release(VERSION);
    fs::create_dir_all(e.installed()).unwrap();
    let out = e.linux(&["--version", VERSION]);
    assert_eq!(code(&out), 8, "{}", text(&out));
    assert!(e.installed().is_dir());
    assert_eq!(fs::read_dir(e.p("prefix/bin")).unwrap().count(), 1);

    let e = env();
    e.release(VERSION);
    fs::write(e.p("prefix/bin"), "a file where the folder should be").unwrap();
    let out = e.linux(&["--version", VERSION]);
    assert_eq!(code(&out), 8, "{}", text(&out));
}

#[test]
fn installs_mode_755_under_a_restrictive_umask() {
    let e = env();
    e.release(VERSION);
    e.uname("Linux", "x86_64");
    let path = format!(
        "{}:{}",
        e.p("shim").display(),
        std::env::var("PATH").unwrap()
    );
    let out = Command::new("sh")
        .args(["-c", "umask 077; exec sh \"$@\"", "sh"])
        .arg(root().join("scripts/install.sh"))
        .args(["--prefix", e.p("prefix").to_str().unwrap()])
        .env("PATH", path)
        .env("TMPDIR", e.p("scratch"))
        .env(
            "MAKIBA_RELEASES_URL",
            format!("file://{}", e.p("rel").display()),
        )
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "{}", text(&out));
    let mode = fs::metadata(e.installed()).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o755);
}

#[test]
fn usage_errors_exit_2() {
    let e = env();
    e.release(VERSION);
    for args in [
        &["--bogus"][..],
        &["--version"][..],
        &["--version", "1.2"][..],
        &["--version", "latest"][..],
        &["--prefix"][..],
    ] {
        let out = Command::new("sh")
            .arg(root().join("scripts/install.sh"))
            .args(args)
            .env("TMPDIR", e.p("scratch"))
            .output()
            .unwrap();
        assert_eq!(code(&out), 2, "{args:?}: {}", text(&out));
    }
    let out = Command::new("sh")
        .arg(root().join("scripts/install.sh"))
        .env_remove("HOME")
        .output()
        .unwrap();
    assert_eq!(code(&out), 2, "{}", text(&out));
}

#[test]
fn refuses_a_host_without_curl_or_wget() {
    let e = env();
    e.release(VERSION);
    let empty = e.p("empty-path");
    fs::create_dir(&empty).unwrap();
    let out = Command::new("/bin/sh")
        .arg(root().join("scripts/install.sh"))
        .args(["--prefix", e.p("prefix").to_str().unwrap()])
        .env("PATH", &empty)
        .output()
        .unwrap();
    assert_eq!(code(&out), 3, "{}", text(&out));
    e.assert_nothing_placed();
}

// --- the manifest script and the tag check ---

#[test]
fn the_manifest_check_accepts_a_made_manifest_and_names_each_fault() {
    let check = |e: &Env, version: &str| {
        let v = e.vdir(VERSION);
        sh(&[
            "scripts/release-manifest.sh",
            "check",
            version,
            v.to_str().unwrap(),
        ])
    };
    let e = env();
    e.release(VERSION);
    let out = check(&e, VERSION);
    assert_eq!(code(&out), 0, "{}", text(&out));
    // The manifest lists every supported target, each with the hash of its archive.
    let m = fs::read_to_string(e.manifest(VERSION)).unwrap();
    for t in TARGETS {
        assert!(m.contains(&format!("\"{t}\"")), "{t} is not in {m}");
    }
    assert_eq!(code(&check(&e, "1.2.4")), 1, "a wrong version");

    let e = env();
    e.release(VERSION);
    e.edit_manifest(VERSION, "\"macos-x86_64\"", "\"macos-riscv64\"");
    assert_eq!(code(&check(&e, VERSION)), 1, "a missing target");

    let e = env();
    e.release(VERSION);
    let mut extra = fs::read_to_string(e.manifest(VERSION)).unwrap();
    extra = extra.replace(
        "\"targets\": {",
        "\"targets\": {\n    \"windows-x86_64\": {\"url\": \"x/makiba-1.2.3-windows-x86_64.tar.gz\", \"sha256\": \"0\"},",
    );
    fs::write(e.manifest(VERSION), extra).unwrap();
    assert_eq!(code(&check(&e, VERSION)), 1, "an extra target");

    let e = env();
    e.release(VERSION);
    let a = e.archive(VERSION, "macos-aarch64");
    let mut bytes = fs::read(&a).unwrap();
    bytes.push(0);
    fs::write(&a, bytes).unwrap();
    assert_eq!(
        code(&check(&e, VERSION)),
        1,
        "an archive changed after the manifest"
    );

    let e = env();
    e.release(VERSION);
    fs::remove_file(e.archive(VERSION, "linux-aarch64")).unwrap();
    assert_eq!(code(&check(&e, VERSION)), 1, "an archive that is gone");

    let e = env();
    e.release(VERSION);
    let a = format!("makiba-{VERSION}-macos-aarch64.tar.gz");
    let b = format!("makiba-{VERSION}-macos-x86_64.tar.gz");
    e.edit_manifest(VERSION, &a, "SWAP");
    e.edit_manifest(VERSION, &b, &a);
    e.edit_manifest(VERSION, "SWAP", &b);
    assert_eq!(code(&check(&e, VERSION)), 1, "swapped entries");
}

#[test]
fn the_manifest_script_will_not_make_a_manifest_with_a_target_missing() {
    let e = env();
    e.release(VERSION);
    fs::remove_file(e.archive(VERSION, "macos-x86_64")).unwrap();
    let v = e.vdir(VERSION);
    let out = sh(&[
        "scripts/release-manifest.sh",
        "make",
        VERSION,
        "x",
        v.to_str().unwrap(),
    ]);
    assert_eq!(code(&out), 1, "{}", text(&out));
}

#[test]
fn the_tag_check_passes_for_the_version_of_cargo_toml_and_fails_for_any_other() {
    let own = env!("CARGO_PKG_VERSION");
    let out = sh(&["scripts/check-tag.sh", &format!("v{own}")]);
    assert_eq!(code(&out), 0, "{}", text(&out));
    for tag in ["v9.9.9", &format!("v{own}1"), own, "v", "latest", "v1.2"] {
        let out = sh(&["scripts/check-tag.sh", tag]);
        assert_eq!(code(&out), 1, "{tag}: {}", text(&out));
    }
    assert_eq!(code(&sh(&["scripts/check-tag.sh"])), 2);
}

// --- the lists agree, and the workflow's rules ---

fn targets_in(s: &str) -> BTreeSet<String> {
    Regex::new(r"\b(?:linux|macos|windows)-(?:x86_64|aarch64|arm64|x86|i686|riscv64)\b")
        .unwrap()
        .find_iter(s)
        .map(|m| m.as_str().to_string())
        .collect()
}

#[test]
fn the_workflow_the_installer_the_manifest_script_and_the_docs_list_the_same_four_targets() {
    let want: BTreeSet<String> = TARGETS.iter().map(|t| t.to_string()).collect();
    for file in [
        "docs/release/release.yml",
        "scripts/install.sh",
        "scripts/release-manifest.sh",
        "docs/installing.md",
    ] {
        let s = read(file);
        assert_eq!(targets_in(&s), want, "{file}");
        assert!(!s.contains("windows-"), "{file} names a windows target");
    }
    // The installer's uname map is checked in its own terms: each target is the answer for some host.
    let install = read("scripts/install.sh");
    for t in TARGETS {
        assert!(
            install.contains(&format!("target={t}")),
            "no host picks {t}"
        );
    }
    // The workflow's matrix has exactly those four, each on a runner of its own.
    let wf = read("docs/release/release.yml");
    let matrix: BTreeSet<String> = Regex::new(r"(?m)^\s*- target: (\S+)$")
        .unwrap()
        .captures_iter(&wf)
        .map(|c| c[1].to_string())
        .collect();
    assert_eq!(matrix, want);
}

#[test]
fn the_release_workflow_is_gated_on_the_tag_and_checks_the_manifest_before_publishing() {
    let wf = read("docs/release/release.yml");
    assert!(
        wf.contains("tags: [\"v[0-9]+.[0-9]+.[0-9]+\"]"),
        "a version tag triggers it"
    );
    assert!(wf.contains("workflow_dispatch:"), "a rehearsal by hand");
    assert!(wf.contains("sh scripts/check-tag.sh \"$GITHUB_REF_NAME\""));
    assert!(
        wf.contains("  build:\n    needs: check-version"),
        "no build before the tag check"
    );
    assert!(wf.contains("cargo build --release --locked --bin makiba"));
    assert!(
        wf.contains("THIRD-PARTY-NOTICES"),
        "the archive carries the notices"
    );
    // The version the binary prints is checked, a step that fails.
    assert!(wf.contains("test \"$(target/release/makiba --version)\" = \"makiba $VERSION\""));
    // The manifest is made and checked before the release is created, and again by the job that creates it.
    let make = wf.find("release-manifest.sh make").expect("make");
    let first_check = wf.find("release-manifest.sh check").expect("check");
    let publish = wf.find("\n  publish:").expect("publish job");
    let create = wf.find("release create").expect("release create");
    assert!(make < first_check && first_check < publish && publish < create);
    assert!(wf[publish..create].contains("release-manifest.sh check"));
    // Only the publish job writes, and only for a pushed tag: the event, not an input.
    assert_eq!(wf.matches("contents: write").count(), 1);
    assert!(wf.find("contents: write").unwrap() > publish);
    assert!(wf[publish..].contains("if: github.event_name == 'push'"));
    assert!(wf.matches("release create").count() == 1);
    assert!(
        !wf.contains("inputs:"),
        "no input can open the publish step"
    );
    // Once the owner activates the workflow, it is this file.
    let active = root().join(".github/workflows/release.yml");
    if active.exists() {
        assert_eq!(
            fs::read_to_string(active).unwrap(),
            wf,
            "the active workflow differs from docs/release/release.yml"
        );
    }
}

#[test]
fn the_scripts_parse() {
    for s in ["install.sh", "release-manifest.sh", "check-tag.sh"] {
        let out = sh(&["-n", &format!("scripts/{s}")]);
        assert_eq!(code(&out), 0, "{s}: {}", text(&out));
    }
}

// --- the binary and the notices ---

#[test]
fn makiba_prints_its_version() {
    let out = Command::new(MAKIBA).arg("--version").output().unwrap();
    assert_eq!(code(&out), 0, "{}", text(&out));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("makiba {}\n", env!("CARGO_PKG_VERSION"))
    );
}

/// The crates of the locked dependency tree that a binary for any of the four targets can contain:
/// `cargo tree` offline over the normal dependencies of each target, the package itself left out.
/// (It also lists crates that run only at build time, such as the derive macros; their notices are
/// kept as well, which is the safe side.)
fn tree_crates() -> BTreeSet<(String, String)> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let line = Regex::new(r"^(\S+) v(\S+)").unwrap();
    let mut set = BTreeSet::new();
    for triple in [
        "x86_64-unknown-linux-gnu",
        "aarch64-unknown-linux-gnu",
        "aarch64-apple-darwin",
        "x86_64-apple-darwin",
    ] {
        let out = Command::new(&cargo)
            .current_dir(root())
            .args([
                "tree",
                "--locked",
                "--offline",
                "-e",
                "normal",
                "--prefix",
                "none",
            ])
            .args(["--format", "{p}", "--target", triple])
            .output()
            .expect("cargo runs");
        assert!(out.status.success(), "cargo tree: {}", text(&out));
        for l in String::from_utf8(out.stdout).unwrap().lines() {
            if let Some(c) = line.captures(l)
                && &c[1] != "makiba"
            {
                set.insert((c[1].to_string(), c[2].to_string()));
            }
        }
    }
    set
}

#[test]
fn the_notices_name_each_crate_of_the_locked_tree_and_no_crate_that_is_not_locked() {
    let notices = read("THIRD-PARTY-NOTICES");
    let named: BTreeSet<(String, String)> = Regex::new(r"(?m)^\* (\S+) (\S+)$")
        .unwrap()
        .captures_iter(&notices)
        .map(|c| (c[1].to_string(), c[2].to_string()))
        .collect();
    let lock = read("Cargo.lock");
    let locked: BTreeSet<(String, String)> =
        Regex::new(r#"(?m)^name = "([^"]+)"\nversion = "([^"]+)""#)
            .unwrap()
            .captures_iter(&lock)
            .map(|c| (c[1].to_string(), c[2].to_string()))
            .collect();
    let tree = tree_crates();
    assert!(tree.len() > 10, "the tree read is {tree:?}");
    let missing: Vec<_> = tree.difference(&named).collect();
    assert!(missing.is_empty(), "no notice for {missing:?}");
    let stale: Vec<_> = named.difference(&locked).collect();
    assert!(
        stale.is_empty(),
        "notices for crates that are not locked: {stale:?}"
    );
    // Each licence text is there, not only the names.
    assert!(notices.matches("Permission is hereby granted").count() >= 10);
    assert!(notices.contains("rusqlite"));
}
