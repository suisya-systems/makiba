//! `makiba up` (M4-4): starts the base cleanly. The base is a stand-in `hc` that records its
//! environment, `claude` is a stand-in that prints a version, and every folder is a temporary one;
//! `makibad` runs on the in-process stand-in base without a socket (`MAKIBA_DOOR=none`) and is
//! ended by the test that started it.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

const IN_RANGE: &str = "2.1.326 (Claude Code)";

struct Setup {
    root: tempfile::TempDir,
}

impl Setup {
    fn new(claude_prints: &str) -> Setup {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        fs::create_dir(&bin).unwrap();
        script(&bin.join("claude"), &format!("echo '{claude_prints}'"));
        // The stand-in base: on `server start` it records the environment it was started with.
        let rec = root.path().join("hc.rec");
        script(
            &bin.join("hc"),
            &format!(
                "echo \"CALL $*\" >> {rec}\nenv | sort >> {rec}",
                rec = rec.display()
            ),
        );
        Setup { root }
    }

    fn path(&self, p: &str) -> PathBuf {
        self.root.path().join(p)
    }

    fn up(&self, extra: &[(&str, &str)]) -> Output {
        let mut c = Command::new(env!("CARGO_BIN_EXE_makiba"));
        c.arg("up")
            .env_clear()
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.path("bin").display()),
            )
            .env("HOME", self.path("home"))
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("XDG_STATE_HOME", self.path("state"))
            .env("HC_BIN", self.path("bin/hc"))
            .env("MAKIBA_BASE", "stub")
            .env("MAKIBA_DOOR", "none")
            .env("MAKIBA_PORT", "0");
        for (k, v) in extra {
            c.env(k, v);
        }
        c.output().unwrap()
    }

    fn lock(&self) -> PathBuf {
        self.path("state/makiba/makibad.lock")
    }

    fn daemon_pid(&self) -> Option<i32> {
        fs::read_to_string(self.lock()).ok()?.trim().parse().ok()
    }

    fn key(&self) -> PathBuf {
        self.path("config/makiba/person.key")
    }
}

impl Drop for Setup {
    /// A `makibad` this test started is ended by this test.
    fn drop(&mut self) {
        if let Some(pid) = self.daemon_pid() {
            unsafe { libc::kill(pid, libc::SIGTERM) };
            let until = Instant::now() + Duration::from_secs(10);
            while Instant::now() < until && unsafe { libc::kill(pid, 0) } == 0 {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

fn script(path: &Path, body: &str) {
    fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn mode(p: &Path) -> u32 {
    fs::metadata(p).unwrap().permissions().mode() & 0o777
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

#[test]
fn inside_an_agent_up_refuses_and_starts_nothing() {
    let s = Setup::new(IN_RANGE);
    let out = s.up(&[("CLAUDE_CODE_SESSION_ID", "abc")]);
    assert!(!out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("login shell"), "{}", text(&out));
    assert!(!s.path("hc.rec").exists(), "the base was started");
    assert!(!s.lock().exists(), "makibad was started");
    assert!(!s.key().exists(), "the person key was made");
}

#[test]
fn a_launched_agent_has_none_of_the_callers_variables() {
    let s = Setup::new(IN_RANGE);
    let tokens = [
        "GH_TOKEN",
        "GITHUB_TOKEN",
        "GH_ENTERPRISE_TOKEN",
        "GITHUB_ENTERPRISE_TOKEN",
    ];
    let extra: Vec<(&str, &str)> = tokens.iter().map(|t| (*t, "secret")).collect();
    let out = s.up(&extra);
    assert!(out.status.success(), "{}", text(&out));
    let rec = fs::read_to_string(s.path("hc.rec")).expect("the base was not started");
    assert!(rec.contains("CALL server start"), "{rec}");
    assert!(rec.contains("HC_SESSION=makiba"), "{rec}");
    for t in tokens {
        assert!(
            !rec.contains(&format!("{t}=")),
            "{t} reached the base: {rec}"
        );
    }
    assert!(
        rec.contains(&format!(
            "XDG_CONFIG_HOME={}",
            s.path("config/makiba/base").display()
        )),
        "{rec}"
    );
    assert!(
        !s.path("config/herdr-core").exists(),
        "the person's own configuration was written"
    );
    assert_eq!(
        fs::read_to_string(s.path("config/makiba/base/herdr-core/config.toml")).unwrap(),
        "notes = false\n"
    );
}

#[test]
fn a_second_up_starts_nothing_new() {
    let s = Setup::new(IN_RANGE);
    assert!(s.up(&[]).status.success());
    let pid = s.daemon_pid().expect("makibad is not running");
    let again = s.up(&[]);
    assert!(again.status.success(), "{}", text(&again));
    assert_eq!(s.daemon_pid(), Some(pid), "a second makibad was started");
    assert!(text(&again).contains("already running"), "{}", text(&again));
}

#[test]
fn the_person_key_is_private_and_a_second_up_keeps_it() {
    let s = Setup::new(IN_RANGE);
    assert!(s.up(&[]).status.success());
    assert_eq!(mode(&s.key()), 0o600);
    assert_eq!(mode(s.key().parent().unwrap()), 0o700);
    let first = fs::read_to_string(s.key()).unwrap();
    assert!(!first.trim().is_empty());
    assert!(s.up(&[]).status.success());
    assert_eq!(fs::read_to_string(s.key()).unwrap(), first);
}

#[test]
fn a_claude_outside_the_range_stops_up_before_anything_starts() {
    let s = Setup::new("9.9.9 (Claude Code)");
    let out = s.up(&[]);
    assert!(!out.status.success(), "{}", text(&out));
    let said = text(&out);
    assert!(said.contains("9.9.9"), "{said}");
    assert!(said.contains(makiba::agents::version::SUPPORTED), "{said}");
    assert!(!s.path("hc.rec").exists(), "the base was started");
    assert!(!s.lock().exists(), "makibad was started");
}
