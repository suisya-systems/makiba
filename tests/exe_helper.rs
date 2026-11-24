//! `common::exe` makes an executable the tests can run (Issues 374, 379): the bytes, mode 0o755,
//! a replacement of what was at the path, and no side file left in the folder. The last test runs
//! many threads that each make and run executables while the others fork, the pattern that failed
//! with ETXTBSY when the write was in this process.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

fn run(p: &Path) -> String {
    let out = Command::new(p).output().unwrap();
    assert!(out.status.success(), "{}: {:?}", p.display(), out);
    String::from_utf8(out.stdout).unwrap()
}

fn mode(p: &Path) -> u32 {
    std::fs::metadata(p).unwrap().permissions().mode() & 0o777
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[test]
fn write_makes_an_executable_with_the_bytes_and_leaves_nothing_else() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("tool");
    let body = "#!/bin/sh\necho first\n";
    common::exe::write(&p, body);
    assert_eq!(std::fs::read_to_string(&p).unwrap(), body);
    assert_eq!(mode(&p), 0o755);
    assert_eq!(run(&p), "first\n");
    assert_eq!(names(dir.path()), ["tool"]);
}

#[test]
fn write_replaces_an_executable_that_is_there() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("tool");
    common::exe::write(&p, "#!/bin/sh\necho first\n");
    common::exe::write(&p, b"#!/bin/sh\necho second\n");
    assert_eq!(run(&p), "second\n");
    assert_eq!(mode(&p), 0o755);
    assert_eq!(names(dir.path()), ["tool"]);
}

#[test]
fn copy_makes_an_executable_copy() {
    let dir = tempfile::tempdir().unwrap();
    let from = dir.path().join("source");
    std::fs::write(&from, "#!/bin/sh\necho copied\n").unwrap();
    let to = dir.path().join("copy");
    common::exe::copy(&from, &to);
    assert_eq!(mode(&to), 0o755);
    assert_eq!(run(&to), "copied\n");
    assert_eq!(names(dir.path()), ["copy", "source"]);
}

#[test]
fn executables_made_while_other_threads_fork_all_run() {
    let dir = tempfile::tempdir().unwrap();
    std::thread::scope(|s| {
        for t in 0..8 {
            let dir = dir.path();
            s.spawn(move || {
                for i in 0..40 {
                    let p = dir.join(format!("t{t}-{i}"));
                    common::exe::write(&p, format!("#!/bin/sh\necho {t}-{i}\n"));
                    assert_eq!(run(&p), format!("{t}-{i}\n"));
                }
            });
        }
    });
}
