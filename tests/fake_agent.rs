//! M0-4: the test agent. A script is followed through `Base`: status in order through `get`, keys
//! and lines into the record file. Runs on `Local` by default and on the real base with
//! `MAKIBA_TEST_REAL_BASE=1`.

mod common;

use common::running_base;
use makiba::base::{Base, BlockKind, LaunchSpec, State, TerminalRef};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

const AGENT: &str = env!("CARGO_BIN_EXE_makiba-fake-agent");

fn launch(base: &dyn Base, name: &str, record: &Path, script: &[&str]) -> TerminalRef {
    let mut argv = vec![
        AGENT.to_string(),
        "--record".into(),
        record.display().to_string(),
    ];
    argv.extend(script.iter().map(|s| s.to_string()));
    base.launch(&LaunchSpec {
        argv,
        cwd: std::env::temp_dir(),
        name: name.into(),
        tags: BTreeMap::new(),
        env: BTreeMap::new(),
        unset: Vec::new(),
    })
    .unwrap()
}

/// Poll until `f` returns a value, for up to 30 seconds.
fn wait<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let until = Instant::now() + Duration::from_secs(30);
    while Instant::now() < until {
        if let Some(v) = f() {
            return v;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("timed out waiting for {what}");
}

fn status_of(base: &dyn Base, t: &TerminalRef) -> Option<(State, Option<BlockKind>)> {
    base.get(t).unwrap().status.map(|s| (s.state, s.kind))
}

fn record(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(String::from)
        .collect()
}

/// `working, blocked(permission), done`, held until a line (working) or a key (blocked) arrives, is seen in that order.
#[test]
fn a_script_of_working_blocked_done_is_seen_in_order_through_get() {
    let base = running_base();
    base.start().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let rec = dir.path().join("record");
    let t = launch(
        &*base,
        "fake-status",
        &rec,
        &[
            "working",
            "wait-line",
            "blocked:permission",
            "wait-key",
            "done",
            "hang",
        ],
    );

    let mut seen = Vec::new();
    for expected in [
        (State::Working, None),
        (State::Blocked, Some(BlockKind::Permission)),
        (State::Done, None),
    ] {
        wait(&format!("{expected:?}"), || {
            (status_of(&*base, &t) == Some(expected)).then_some(())
        });
        seen.push(expected);
        match expected.0 {
            State::Working => base.send(&t, "go", false).unwrap(),
            // A blocked terminal takes keys: a send would answer its dialog and is refused.
            State::Blocked => base.keys(&t, &["enter"]).unwrap(),
            _ => {}
        }
    }
    assert_eq!(
        seen,
        [
            (State::Working, None),
            (State::Blocked, Some(BlockKind::Permission)),
            (State::Done, None)
        ]
    );
    wait("both lines recorded", || {
        (record(&rec).len() == 2).then_some(())
    });
    assert_eq!(record(&rec), ["line go", "key enter"]);
    base.end(&t).unwrap();
}

/// Keys sent to a scripted dialog arrive in the record file in order, and move its cursor.
#[test]
fn keys_sent_to_a_scripted_dialog_arrive_in_the_record_file_in_order() {
    let base = running_base();
    base.start().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let rec = dir.path().join("record");
    let t = launch(&*base, "fake-dialog", &rec, &["dialog", "hang"]);

    wait("the dialog's cursor line", || {
        base.read(&t, 50)
            .unwrap()
            .iter()
            .any(|l| l.starts_with('❯') && l.contains("1."))
            .then_some(())
    });
    base.keys(&t, &["down"]).unwrap();
    base.keys(&t, &["up"]).unwrap();
    base.keys(&t, &["down", "enter"]).unwrap();

    wait("all four keys recorded", || {
        (record(&rec).len() == 4).then_some(())
    });
    assert_eq!(
        record(&rec),
        ["key down", "key up", "key down", "key enter"]
    );
    wait("the choice shown", || {
        base.read(&t, 50)
            .unwrap()
            .iter()
            .any(|l| l.contains("dialog chose 2"))
            .then_some(())
    });
    base.end(&t).unwrap();
}
