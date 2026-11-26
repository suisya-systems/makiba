//! Issue 382: `makiba up` that finds the base stopped runs `hc server start`, and `hc` starts its
//! server under the name `herdr-core`. When an older `herdr-core` is found first, the core that
//! starts is of another version than the pinned one. `up` then refuses before it resumes a run or
//! starts `makibad`, names the version, the pinned one and the server's file, and does not stop
//! the core.
//!
//! The server is a stand-in: this test binary, copied as `herdr-core` into a folder that `PATH`
//! names first and run as its ignored test `stand_in_core`, which answers `hello` with a chosen
//! version on the base's socket. `hc` and `claude` are stand-in scripts; every folder is a
//! temporary one. The stand-in `hc` finds `herdr-core` on `PATH`; that stands for an `hc` that
//! finds an older server first, not for how the real `hc` looks it up (probe P15 measures that).

mod common;

use common::makibad;
use makiba::base::version::PINNED;
use makiba::spec::ErrorKind;
use serde_json::{Value, json};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::Output;
use std::time::{Duration, Instant};

/// What `claude --version` prints for the lowest supported version.
fn in_range() -> String {
    let (a, b, c) = makiba::agents::version::LOWEST;
    format!("{a}.{b}.{c} (Claude Code)")
}

/// The release before the pinned one.
fn older() -> String {
    let p: Vec<u64> = PINNED.split('.').map(|x| x.parse().unwrap()).collect();
    match (p[0], p[1], p[2]) {
        (a, b, c) if c > 0 => format!("{a}.{b}.{}", c - 1),
        (a, b, _) if b > 0 => format!("{a}.{}.0", b - 1),
        (a, _, _) => format!("{}.0.0", a - 1),
    }
}

/// The stand-in server, when this binary runs as it (the variables below are set); otherwise
/// nothing. It waits `STANDIN_CORE_DELAY_MS`, binds `STANDIN_CORE_SOCKET`, writes its pid to
/// `STANDIN_CORE_DIR/pid`, appends the method of every request to `STANDIN_CORE_DIR/methods`,
/// answers `hello` with `STANDIN_CORE_VERSION` and anything else with an error, and exits after 60 s.
#[test]
#[ignore = "run only as the stand-in server of the other tests"]
fn stand_in_core() {
    let (Ok(version), Ok(socket), Ok(dir)) = (
        std::env::var("STANDIN_CORE_VERSION"),
        std::env::var("STANDIN_CORE_SOCKET"),
        std::env::var("STANDIN_CORE_DIR"),
    ) else {
        return;
    };
    let delay: u64 = std::env::var("STANDIN_CORE_DELAY_MS")
        .ok()
        .and_then(|d| d.parse().ok())
        .unwrap_or(0);
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(60));
        std::process::exit(0);
    });
    std::thread::sleep(Duration::from_millis(delay));
    let dir = PathBuf::from(dir);
    let listener = UnixListener::bind(&socket).unwrap();
    fs::write(dir.join("pid"), std::process::id().to_string()).unwrap();
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let (version, methods) = (version.clone(), dir.join("methods"));
        std::thread::spawn(move || {
            let mut writer = stream.try_clone().unwrap();
            for line in BufReader::new(stream).lines() {
                let Ok(line) = line else { return };
                let Ok(r) = serde_json::from_str::<Value>(&line) else {
                    return;
                };
                let method = r["method"].as_str().unwrap_or("").to_string();
                if let Ok(mut f) = fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&methods)
                {
                    let _ = writeln!(f, "{method}");
                }
                let reply = if method == "hello" {
                    json!({"jsonrpc": "2.0", "id": r["id"], "result": {"version": version}})
                } else {
                    json!({"jsonrpc": "2.0", "id": r["id"],
                           "error": {"code": -32601, "message": "not in this stand-in"}})
                };
                if writeln!(writer, "{reply}").is_err() {
                    return;
                }
            }
        });
    }
}

struct Setup {
    root: tempfile::TempDir,
}

impl Setup {
    /// A root with `old/herdr-core` (the stand-in server answering `version`, after `delay_ms`),
    /// `new/bin/hc` (which starts `herdr-core` as `PATH` finds it, and waits for its socket when
    /// `wait`) and `new/bin/claude`. Nothing listens at the base's socket.
    fn new(version: &str, delay_ms: u64, wait: bool) -> Setup {
        let root = tempfile::tempdir().unwrap();
        let p = |s: &str| root.path().join(s);
        for d in ["old", "new/bin", "core", "run/herdr-core"] {
            fs::create_dir_all(p(d)).unwrap();
        }
        common::exe::copy(&std::env::current_exe().unwrap(), &p("old/herdr-core"));
        common::exe::write(
            &p("new/bin/claude"),
            format!("#!/bin/sh\necho '{}'\n", in_range()),
        );
        let wait_loop = if wait {
            "i=0\nwhile [ ! -S \"$sock\" ] && [ $i -lt 100 ]; do sleep 0.1; i=$((i+1)); done\n"
        } else {
            ""
        };
        common::exe::write(
            &p("new/bin/hc"),
            format!(
                "#!/bin/sh\necho \"CALL $*\" >> '{rec}'\n\
                 [ \"$1 $2\" = 'server start' ] || exit 0\n\
                 sock=\"$XDG_RUNTIME_DIR/herdr-core/$HC_SESSION.sock\"\n\
                 STANDIN_CORE_VERSION='{version}' STANDIN_CORE_SOCKET=\"$sock\" \
                 STANDIN_CORE_DIR='{core}' STANDIN_CORE_DELAY_MS={delay_ms} \
                 nohup herdr-core --exact stand_in_core --ignored --nocapture --test-threads=1 \
                 </dev/null >/dev/null 2>&1 &\n\
                 {wait_loop}exit 0\n",
                rec = p("hc.rec").display(),
                core = p("core").display(),
            ),
        );
        Setup { root }
    }

    fn path(&self, p: &str) -> PathBuf {
        self.root.path().join(p)
    }

    /// `makiba up` from a login shell of this root, and how long it took.
    fn up(&self) -> (Output, Duration) {
        let mut c = makibad::up(self.root.path());
        c.env_clear()
            .env(makibad::KNOWLEDGE_ENV, makibad::knowledge(self.root.path()))
            .env(
                "PATH",
                format!(
                    "{}:{}:/usr/bin:/bin",
                    self.path("old").display(),
                    self.path("new/bin").display()
                ),
            )
            .env("HOME", self.path("home"))
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("XDG_STATE_HOME", self.path("state"))
            .env("XDG_RUNTIME_DIR", self.path("run"))
            .env("HC_BIN", self.path("new/bin/hc"))
            .env("MAKIBA_DOOR", "none")
            .env("MAKIBA_PORT", "0");
        let start = Instant::now();
        let out = c.output().unwrap();
        (out, start.elapsed())
    }

    fn pid(&self, file: &str) -> Option<i32> {
        fs::read_to_string(self.path(file))
            .ok()
            .and_then(|s| s.trim().parse().ok())
    }

    /// The stand-in server's pid, once it has bound its socket.
    fn server_pid(&self) -> i32 {
        let until = Instant::now() + Duration::from_secs(10);
        while Instant::now() < until {
            if let Some(pid) = self.pid("core/pid") {
                return pid;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("the stand-in server wrote no pid");
    }

    fn server_file(&self) -> PathBuf {
        fs::canonicalize(self.path("old/herdr-core")).unwrap()
    }
}

impl Drop for Setup {
    /// The stand-in server, and a `makibad` an `up` without the refusal started, are ended here.
    fn drop(&mut self) {
        for file in ["core/pid", "state/makiba/makibad.lock"] {
            let Some(pid) = self.pid(file) else { continue };
            unsafe { libc::kill(pid, libc::SIGTERM) };
            let until = Instant::now() + Duration::from_secs(10);
            while Instant::now() < until && unsafe { libc::kill(pid, 0) } == 0 {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

fn said(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// What every refusal of an older started server shows: the exit code, the versions, the way
/// out, the file (on Linux), and no `makibad` started.
fn assert_refused(s: &Setup, out: &Output) {
    let all = said(out);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(ErrorKind::Unavailable.exit_code()),
        "{all}"
    );
    for part in [
        older().as_str(),
        PINNED,
        "down --stop-base",
        "install-base.sh",
    ] {
        assert!(err.contains(part), "{part:?} is not named: {all}");
    }
    #[cfg(target_os = "linux")]
    {
        let file = s.server_file().display().to_string();
        assert!(
            err.contains(&file),
            "the server's file {file} is not named: {all}"
        );
    }
    assert!(
        !s.path("state/makiba/makibad.log").exists(),
        "makibad was started: {all}"
    );
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("makibad: started"),
        "{all}"
    );
}

#[test]
fn an_older_server_that_hc_started_is_refused_and_named() {
    let s = Setup::new(&older(), 0, true);
    let (out, took) = s.up();
    let all = said(&out);
    assert!(took < Duration::from_secs(15), "took {took:?}: {all}");
    assert_refused(&s, &out);
    assert_eq!(
        fs::read_to_string(s.path("hc.rec")).unwrap(),
        "CALL server start\n",
        "{all}"
    );
    // `up` never stops a core, also one it started.
    let pid = s.server_pid();
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        0,
        "the server is gone: {all}"
    );
    let methods = fs::read_to_string(s.path("core/methods")).unwrap_or_default();
    assert!(!methods.lines().any(|m| m == "server.stop"), "{methods}");
}

#[test]
fn an_older_server_that_listens_after_hc_returns_is_refused() {
    let s = Setup::new(&older(), 1000, false);
    let (out, _) = s.up();
    assert_refused(&s, &out);
}

#[test]
fn the_pinned_server_that_hc_started_is_not_refused() {
    let s = Setup::new(PINNED, 0, true);
    let (out, _) = s.up();
    let all = said(&out);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !err.contains(&s.server_file().display().to_string()),
        "{all}"
    );
    assert!(!err.contains("started a base of session"), "{all}");
    // `up` went past the check: only `start_daemon` makes makibad's log.
    assert!(s.path("state/makiba/makibad.log").exists(), "{all}");
}
