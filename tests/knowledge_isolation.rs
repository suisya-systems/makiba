//! Issue 312: a test that starts `makibad` uses a temporary knowledge folder of its own, through
//! the shared helpers (`tests/common/makibad.rs`), and a start without one fails there at once.
//! With the variable unset `makibad` falls back to `~/makiba-knowledge`; when its `raw/` holds 5
//! notes, the makibad under test launches the curator and the test sees notices it did not expect.

mod common;

use common::makibad;
use makiba::base::Base;
use makiba::base::stub::Stub;
use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

/// A `makibad` that is killed when the test ends.
struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A HOME whose `makiba-knowledge/raw/` holds 5 notes: the threshold of the curator.
fn home_with_notes(root: &Path) -> PathBuf {
    let home = root.join("home");
    let raw = home.join("makiba-knowledge/raw");
    std::fs::create_dir_all(&raw).unwrap();
    for n in 0..makiba::daemon::curator::RAW_THRESHOLD {
        std::fs::write(raw.join(format!("note-{n}.md")), "a note").unwrap();
    }
    home
}

/// `makiba daemon` on the stand-in base in `root`, with `home` as HOME and every person's place
/// under it.
fn org(root: &Path, home: &Path) -> makibad::Makibad {
    let state = root.join("state");
    std::fs::create_dir_all(&state).unwrap();
    let mut c = makibad::daemon(root);
    c.env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env("MAKIBA_STATE_DIR", &state)
        .env("MAKIBA_DOOR", "none")
        .env("MAKIBA_PORT", "0")
        .env("MAKIBA_PERSON_KEY", root.join("key/person.key"))
        .env("MAKIBA_BASE", "stub")
        .env("MAKIBA_WORKERS_DIR", root.join("workers"))
        .env("MAKIBA_REPOS_DIR", root.join("repos"))
        .env("MAKIBA_DOCS_DIR", root.join("docs"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    c
}

/// Launches of the curator `makibad` recorded in `root`'s state, once its schema is there.
fn curator_launches(root: &Path) -> Option<i64> {
    let conn = Connection::open_with_flags(
        root.join("state/state.db"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .ok()?;
    conn.busy_timeout(Duration::from_secs(5)).ok()?;
    conn.query_row(
        "SELECT count(*) FROM run_launch WHERE role = 'curator'",
        [],
        |r| r.get(0),
    )
    .ok()
}

fn curator_notices(root: &Path) -> i64 {
    let conn = Connection::open_with_flags(
        root.join("state/state.db"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    conn.busy_timeout(Duration::from_secs(5)).unwrap();
    conn.query_row(
        "SELECT count(*) FROM messages WHERE kind = 'notice' AND body LIKE '%curator%'",
        [],
        |r| r.get(0),
    )
    .unwrap()
}

fn panic_text(e: Box<dyn std::any::Any + Send>) -> String {
    e.downcast_ref::<String>()
        .cloned()
        .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default()
}

/// The definition of done: with 5 raw notes in the knowledge folder of the HOME the test sees,
/// the makibad the helper starts launches no curator. The same HOME without the helper's folder
/// launches one, so the test would see it.
#[test]
fn a_makibad_started_by_the_helper_does_not_read_the_homes_knowledge() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let home = home_with_notes(&root);
    let (helped, bare) = (root.join("helped"), root.join("bare"));

    let _helped = Daemon(org(&helped, &home).spawn().unwrap());
    // The control: the same command with the helper's variable taken away, run unchecked.
    let mut control = org(&bare, &home);
    control.env_remove(makibad::KNOWLEDGE_ENV);
    let mut raw = std::process::Command::new("true");
    control.with(|c| std::mem::swap(c, &mut raw));
    let _bare = Daemon(raw.spawn().unwrap());

    let until = Instant::now() + Duration::from_secs(20);
    while curator_launches(&bare).unwrap_or(0) == 0 {
        assert!(
            Instant::now() < until,
            "the control makibad, which reads the HOME's knowledge, launched no curator"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    // The helped one started at the same time; give it the control's time and more.
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(
        curator_launches(&helped),
        Some(0),
        "the helped makibad launched a curator from the HOME's knowledge folder"
    );
    assert_eq!(curator_notices(&helped), 0);
    assert!(
        makibad::knowledge(&helped)
            .read_dir()
            .unwrap()
            .next()
            .is_none(),
        "its own knowledge folder stays empty"
    );
}

/// A start of the binary without the variable fails in the helper, with the message, before
/// anything runs.
#[test]
fn the_binary_without_the_variable_fails_with_the_message() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let home = home_with_notes(&root);
    for clear in [false, true] {
        let mut c = org(&root, &home);
        if clear {
            c.env_clear().env("HOME", &home);
        } else {
            c.env_remove(makibad::KNOWLEDGE_ENV);
        }
        // Inherited from this process, the variable still does not count.
        let spawn = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| c.spawn()));
        let e = spawn.expect_err("spawn without the variable panics");
        assert_eq!(panic_text(e), makibad::MISSING);
        let output = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| c.output()));
        assert_eq!(panic_text(output.err().unwrap()), makibad::MISSING);
        let status = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| c.status()));
        assert_eq!(panic_text(status.err().unwrap()), makibad::MISSING);
    }
    assert!(
        !root.join("state/state.db").exists(),
        "no makibad was started"
    );
    assert!(makibad::MISSING.contains("sets MAKIBA_KNOWLEDGE_DIR to a temporary folder"));
}

/// In process: a start with the variable unset, or set by the test itself instead of the helper
/// (as to the HOME's folder), fails with the message before the daemon starts; a folder held by
/// `use_knowledge` is accepted only while it is held, and the process's folder comes back after.
#[test]
fn in_process_without_the_helpers_variable_fails_with_the_message() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let home = home_with_notes(&root);
    let dir = root.join("state");
    std::fs::create_dir_all(&dir).unwrap();
    let start = || {
        std::panic::catch_unwind(|| {
            makibad::run_until(
                &dir,
                &|| Box::new(Stub::new()) as Box<dyn Base>,
                Arc::new(AtomicBool::new(true)),
            )
        })
    };
    let process = makibad::process_knowledge();
    assert!(process.is_dir());
    assert_eq!(
        std::env::var_os(makibad::KNOWLEDGE_ENV).as_deref(),
        Some(process.as_os_str())
    );

    // SAFETY: no other test of this binary starts a daemon in its process or reads the variable.
    unsafe { std::env::remove_var(makibad::KNOWLEDGE_ENV) };
    assert_eq!(panic_text(start().unwrap_err()), makibad::MISSING);
    // SAFETY: as above.
    unsafe { std::env::set_var(makibad::KNOWLEDGE_ENV, home.join("makiba-knowledge")) };
    assert_eq!(panic_text(start().unwrap_err()), makibad::MISSING);
    assert!(!dir.join("state.db").exists(), "no daemon was started");

    let own = root.join("own-knowledge");
    {
        let held = makibad::use_knowledge(&own);
        assert_eq!(held.path(), own);
        makibad::check_env();
    }
    // The held folder is let go with its holder: the process's folder is the one again.
    assert_eq!(
        std::env::var_os(makibad::KNOWLEDGE_ENV).as_deref(),
        Some(process.as_os_str())
    );
    makibad::check_env();
    // SAFETY: as above.
    unsafe { std::env::set_var(makibad::KNOWLEDGE_ENV, &own) };
    assert_eq!(panic_text(start().unwrap_err()), makibad::MISSING);
    // SAFETY: as above.
    unsafe { std::env::set_var(makibad::KNOWLEDGE_ENV, process) };
}

/// Every test target starts `makibad` only through the helpers: no target calls
/// `makiba::daemon::run*` or runs `makiba daemon` / `makiba up` itself. Code only, not comments.
/// The scan is the library's (`makiba::hooks::test_commit`, Issue 322), the one the commit-msg
/// hook runs on a test commit's declared tests; this file is scanned too.
#[test]
fn no_test_target_starts_makibad_outside_the_helpers() {
    use makiba::hooks::test_commit::{direct_starts, is_test_target};
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let tests = root.join("tests");
    let mut targets: Vec<String> = Vec::new();
    for e in std::fs::read_dir(&tests).unwrap().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        for rel in [format!("tests/{name}"), format!("tests/{name}/main.rs")] {
            if is_test_target(&rel) && root.join(&rel).is_file() {
                targets.push(rel);
            }
        }
    }
    targets.sort();
    assert!(targets.len() > 10, "the test targets were found");
    assert!(!is_test_target("tests/common/main.rs"));
    assert!(!is_test_target("tests/common/makibad.rs"));
    let mut found = Vec::new();
    for t in &targets {
        let text = std::fs::read_to_string(root.join(t)).unwrap();
        for (line, code) in direct_starts(&text) {
            found.push(format!("{t}:{line}: {code}"));
        }
    }
    assert!(
        found.is_empty(),
        "start makibad through tests/common/makibad.rs, not directly:\n{}",
        found.join("\n")
    );
    // The scan itself still finds what it found before the move.
    let call = format!(
        "    makiba::{}{}::run_until(&dir, &base, stop);",
        "dae", "mon"
    );
    assert_eq!(
        direct_starts(&format!("fn a() {{\n{call}\n}}\n")),
        vec![(2, call.trim().to_string())]
    );
    let import = format!("use makiba::{}{}::{{curator, run_until}};", "dae", "mon");
    assert_eq!(direct_starts(&import).len(), 1);
    assert!(
        direct_starts(&format!("// {call}")).is_empty(),
        "a comment is not code"
    );
    assert!(direct_starts("    makibad::run_until(&dir, &base, stop);").is_empty());
}
