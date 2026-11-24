//! Making an executable in a test (Issues 374, 379). Every test that makes a file and then runs it
//! does it through this module; `tools/check_test_executables.py` (run by the tests job through
//! `tests/test_check_test_executables.py`) refuses any other place under `tests/` that sets an
//! executable mode, copies a file with `fs::copy` or runs `chmod`.
//!
//! Why: a descriptor this process opens for writing is inherited by every child that another test
//! thread forks, until that child execs. Exec of a file some process holds open for writing fails
//! with ETXTBSY ("Text file busy"). A rename does not help: the inherited descriptor still points
//! at the renamed inode. So this process never opens the executable for writing. The bytes go to a
//! staging file outside the target folder (which is never run), a `cp` child writes the
//! executable from it, and once `cp` has exited no process holds a write descriptor to it. The
//! mode and the rename go by path and open nothing. A stress program measured 1445 failures in
//! 20000 rounds with an in-process write and rename, and 0 in 20000 with `cp` (Issue 374).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

/// Put `bytes` at `path` as an executable file (mode 0o755), replacing what is there.
pub fn write(path: &Path, bytes: impl AsRef<[u8]>) {
    let staging = tempfile::tempdir().unwrap();
    let staged = staging.path().join("bytes");
    fs::write(&staged, bytes).unwrap();
    copy(&staged, path);
}

/// Put a copy of the file `from` at `to` as an executable file (mode 0o755), replacing what is
/// there.
pub fn copy(from: &Path, to: &Path) {
    let name = to
        .file_name()
        .unwrap_or_else(|| panic!("{} names no file", to.display()))
        .to_string_lossy();
    let side = to.with_file_name(format!(
        ".{name}.exe-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let cp = Command::new("cp")
        .arg("--")
        .arg(from)
        .arg(&side)
        .stdin(Stdio::null())
        .status()
        .unwrap();
    assert!(
        cp.success(),
        "cp {} {}: {cp}",
        from.display(),
        side.display()
    );
    fs::set_permissions(&side, fs::Permissions::from_mode(0o755)).unwrap();
    fs::rename(&side, to).unwrap();
}
