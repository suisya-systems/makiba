//! M3-4 (Issue 27): the launch decides what reaches a worker. The daemon runs in this process on
//! `Local` (as tests/monitor_role.rs), so the test holds the base and the agent inherits this
//! process's environment. A queued run's launch waits at `settings_ready`, as M3-2 leaves it; the
//! test agent is given by `MAKIBA_FAKE_AGENT` (spec section 2) and records its environment and
//! its arguments (its `env` step).

mod common;

use common::local::Local;
use makiba::base::{Base, TerminalRef};
use makiba::daemon;
use makiba::db::writer::{RunUpsert, StateWriter};
use rusqlite::{Connection, OpenFlags};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

const AGENT: &str = env!("CARGO_BIN_EXE_makiba-fake-agent");

/// Spec section 6.4: removed for every agent, and the base's own.
const REMOVED: [&str; 17] = [
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDECODE",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_PID",
    "CLAUDE_TMPDIR",
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
    "HC_SESSION",
    "HC_SOCKET",
    "HC_SCOPE",
    "HC_TERMINAL_ID",
    "HC_TERMINAL_SOCKET",
];

/// Poll `f` for up to 20 seconds; `None` when it never answered.
fn wait<T>(mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let until = Instant::now() + Duration::from_secs(20);
    while Instant::now() < until {
        if let Some(v) = f() {
            return Some(v);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    None
}

fn reader(dir: &Path) -> Option<Connection> {
    Connection::open_with_flags(dir.join("state.db"), OpenFlags::SQLITE_OPEN_READ_ONLY).ok()
}

fn one<T: rusqlite::types::FromSql>(dir: &Path, sql: &str) -> Option<T> {
    reader(dir)?.query_row(sql, [], |r| r.get(0)).ok()
}

fn sha256_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Every run of exactly 64 hex digits in `text`: what a key looks like (spec section 4).
fn keys_in(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_ascii_hexdigit())
        .filter(|w| w.len() == 64)
        .map(String::from)
        .collect()
}

/// Ends the terminal when the test ends, passed or failed.
struct Ended(Local, TerminalRef);

impl Drop for Ended {
    fn drop(&mut self) {
        let _ = self.0.end(&self.1);
    }
}

#[test]
fn a_launched_worker_has_its_tags_no_key_in_its_environment_and_its_token_in_its_arguments() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let dir = root.join("state");
    let work = root.join("work");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    let record = root.join("record");

    // This test's own environment sets every name the launch must keep from the agent.
    let agent = serde_json::json!([AGENT, record, "env", "sleep:30000"]).to_string();
    // SAFETY: the only test of this binary, and no thread of it has started yet.
    unsafe {
        for name in REMOVED {
            std::env::set_var(name, "set-by-the-test");
        }
        std::env::set_var("GH_CONFIG_DIR", root.join("gh"));
        std::env::set_var("MAKIBA_FAKE_AGENT", &agent);
    }

    {
        let w = StateWriter::open(&dir.join("state.db")).unwrap();
        w.upsert_run(&RunUpsert {
            task_id: "t1",
            project_slug: "p",
            // Not `in_use`: an `in_use` run with no terminal is recorded as lost (M2-5).
            status: Some("queued"),
            ..Default::default()
        })
        .unwrap();
        w.conn()
            .execute(
                "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree, stage)
                 VALUES ((SELECT id FROM runs WHERE task_id = 't1'), 'spawn-t1', 'document',
                         'fake', 'the brief', ?1, 'settings_ready')",
                [work.to_string_lossy()],
            )
            .unwrap();
    }

    let local = Local::new();
    let _ended = Ended(local.clone(), TerminalRef("w-t1".into()));
    let for_daemon = local.clone();
    let daemon_dir = dir.clone();
    std::thread::spawn(move || daemon::run(&daemon_dir, &|| Box::new(for_daemon.clone())));

    let launched = wait(|| {
        (one::<String>(&dir, "SELECT stage FROM run_launch").as_deref() == Some("launched"))
            .then_some(())
    });
    assert!(
        launched.is_some(),
        "the launch did not reach `launched`: {:?}",
        one::<String>(
            &dir,
            "SELECT stage || ' ' || coalesce(failed_reason, '') FROM run_launch"
        )
    );

    // Base::get right after launch shows the four tags.
    let terminal = local
        .list()
        .unwrap()
        .into_iter()
        .find(|t| t.name == "w-t1")
        .expect("a terminal named w-t1");
    let got = local.get(&terminal.id).unwrap();
    let org = dir.to_string_lossy().into_owned();
    let want: BTreeMap<String, String> = [
        ("makiba.role", "worker"),
        ("makiba.task_id", "t1"),
        ("makiba.spawn", "spawn-t1"),
        ("makiba.org", org.as_str()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    assert_eq!(got.tags, want);

    // What the agent recorded of its environment and its arguments.
    let rows = wait(|| {
        let text = std::fs::read_to_string(&record).ok()?;
        text.lines()
            .any(|l| l == "env-end")
            .then(|| text.lines().map(String::from).collect::<Vec<_>>())
    })
    .unwrap_or_default();
    assert!(!rows.is_empty(), "the test agent recorded nothing");
    let env: BTreeMap<String, String> = rows
        .iter()
        .filter_map(|r| r.strip_prefix("env "))
        .filter_map(|r| r.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let args: Vec<&str> = rows.iter().filter_map(|r| r.strip_prefix("arg ")).collect();
    for name in REMOVED {
        assert!(!env.contains_key(name), "{name} reached the agent");
    }
    assert_eq!(env.get("GH_CONFIG_DIR").map(String::as_str), Some(""));

    let stored: Vec<String> = {
        let conn = reader(&dir).unwrap();
        let mut q = conn
            .prepare("SELECT key_sha256 FROM agents WHERE agent_id = 'w-t1'")
            .unwrap();
        q.query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert_eq!(stored.len(), 1, "one key for the worker: {stored:?}");

    // Its recorded argument list carries the token, and the token hashes to the stored hash.
    let mut in_args = keys_in(&args.join(" "));
    in_args.sort();
    in_args.dedup();
    assert_eq!(in_args.len(), 1, "one token in the arguments: {args:?}");
    let token = &in_args[0];
    assert_eq!(sha256_hex(token), stored[0]);

    // The environment carries no key.
    for (k, v) in &env {
        assert!(
            !k.contains(token.as_str()) && !v.contains(token.as_str()),
            "the token is in {k}"
        );
        for candidate in keys_in(v) {
            assert_ne!(sha256_hex(&candidate), stored[0], "a key is in {k}");
        }
    }
}
