//! Issue 197: `build.rs` reads `skills/` of the checkout being built.
//!
//! Two copies of the repository are made in a temporary folder, each with a skill file of its own.
//! They are built one after the other with one target folder (also temporary, never this
//! checkout's), the second after the first copy was deleted. Both builds succeed, and each binary
//! carries the skill file of its own copy and not the other's. The builds are offline: the
//! dependencies come from the cargo cache this test run already uses.

use std::path::Path;
use std::process::Command;

/// What a copy needs to build `makiba`: the manifest, the lock file, the toolchain, the sources and
/// the files they carry, and the test agent the manifest names as a binary.
const PARTS: [&str; 9] = [
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    "build.rs",
    "src",
    "skills",
    "roles",
    "scripts",
    "tests/common",
];

fn copy(from: &Path, to: &Path) {
    if from.is_dir() {
        std::fs::create_dir_all(to).unwrap();
        for e in std::fs::read_dir(from).unwrap() {
            let e = e.unwrap();
            copy(&e.path(), &to.join(e.file_name()));
        }
    } else {
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::copy(from, to).unwrap();
    }
}

/// A copy of the repository at `dir`, with a skill file holding `marker`.
fn make_copy(dir: &Path, marker: &str) {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    for part in PARTS {
        copy(&repo.join(part), &dir.join(part));
    }
    std::fs::write(dir.join("skills/conveyor/zz-197-marker.md"), marker).unwrap();
}

/// Build `makiba` in `dir` with the target folder `target`; the binary's bytes.
fn build(dir: &Path, target: &Path) -> Vec<u8> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = Command::new(cargo)
        .args(["build", "--offline", "--bin", "makiba", "--target-dir"])
        .arg(target)
        .current_dir(dir)
        .env_remove("CARGO_TARGET_DIR")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "the build in {} failed:\n{}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    std::fs::read(target.join("debug/makiba")).unwrap()
}

fn carries(binary: &[u8], marker: &str) -> bool {
    binary.windows(marker.len()).any(|w| w == marker.as_bytes())
}

#[test]
fn a_second_checkout_builds_with_its_own_skills_after_the_first_is_deleted() {
    let tmp = tempfile::tempdir().unwrap();
    let (first, second, target) = (
        tmp.path().join("first"),
        tmp.path().join("second"),
        tmp.path().join("target"),
    );
    // Built at runtime so that the test binary itself does not carry either marker.
    let mark = |name: &str| format!("issue-197-marker-of-the-{name}-copy");
    let (m1, m2) = (mark("first"), mark("second"));
    make_copy(&first, &m1);
    make_copy(&second, &m2);

    let bin1 = build(&first, &target);
    assert!(
        carries(&bin1, &m1),
        "the first binary carries the first copy's skill"
    );
    assert!(
        !carries(&bin1, &m2),
        "the first binary does not carry the second copy's skill"
    );

    std::fs::remove_dir_all(&first).unwrap();

    let bin2 = build(&second, &target);
    assert!(
        carries(&bin2, &m2),
        "the second binary carries the second copy's skill"
    );
    assert!(
        !carries(&bin2, &m1),
        "the second binary does not carry the first copy's skill"
    );
}
