//! The build of `makiba` a process runs (Issue 340): the SHA-256 of its executable's content,
//! since the package version does not change between builds. Used by `makibad`'s watch of its own
//! executable (`daemon::exe`) and by the door's clients when a call failed; no database here, so
//! the person's command can use it (tests/door.rs).

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};

/// The file this process runs: `/proc/self/exe` on Linux, which keeps naming it after its path
/// was given to another file; elsewhere the path it was started from.
pub fn running_file() -> Option<PathBuf> {
    if cfg!(target_os = "linux") {
        Some(PathBuf::from("/proc/self/exe"))
    } else {
        std::env::current_exe().ok()
    }
}

/// The SHA-256 (hex) of what `r` reads to its end.
pub fn hash(r: &mut impl Read) -> std::io::Result<String> {
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// The SHA-256 (hex) of the file at `path`.
pub fn digest_of(path: &Path) -> Option<String> {
    hash(&mut std::fs::File::open(path).ok()?).ok()
}

/// The digest of the executable this process runs, computed now. For a client of the door, once a
/// call failed.
pub fn own_digest() -> Option<String> {
    digest_of(&running_file()?)
}

/// The first twelve hex digits: enough to tell two builds apart in a line.
pub fn short(digest: &str) -> &str {
    digest.get(..12).unwrap_or(digest)
}

/// The line a client adds to a failed call when `makibad` answered with another `build` than its
/// own; `None` when the builds are the same, or either is unknown.
pub fn other_build_line(answer: &Value) -> Option<String> {
    let theirs = answer["build"].as_str()?;
    let mine = own_digest()?;
    (theirs != mine).then(|| {
        format!(
            "makibad runs another build of makiba ({}) than this makiba ({}); makiba down and \
             makiba up bring them together.",
            short(theirs),
            short(&mine)
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_short_form_is_twelve_digits() {
        assert_eq!(short("0123456789abcdef"), "0123456789ab");
        assert_eq!(short("abc"), "abc");
    }

    #[test]
    fn an_answer_without_build_adds_nothing() {
        assert_eq!(other_build_line(&json!({"ok": false})), None);
    }

    #[test]
    fn an_answer_of_another_build_says_so() {
        let line = other_build_line(&json!({"ok": false, "build": "f".repeat(64)})).unwrap();
        assert!(line.contains("ffffffffffff"), "{line}");
        assert!(line.contains("makiba down and makiba up"), "{line}");
        let mine = own_digest().unwrap();
        assert_eq!(other_build_line(&json!({"ok": false, "build": mine})), None);
    }
}
