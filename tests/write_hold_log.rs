//! Issue 561, change 3: every write transaction on `state.db` is timed, and a hold of the write
//! lock of 1 s or more is logged with its call site:
//! `state.db: the write lock was held {secs:.1} s by {site}`, where the site is the caller's
//! `file:line`, or `<label> (<file>:<line>)` for a transaction begun with `begin_as`.
//!
//! The lines are read through `with_hold_log` (makibad's own go to its stderr, makibad.log).

use makiba::db::writer::{NewEvent, StateWriter};
use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const HOLD: Duration = Duration::from_millis(1200);
const PREFIX: &str = "state.db: the write lock was held ";

struct Ledger {
    path: PathBuf,
    lines: Arc<Mutex<Vec<String>>>,
    _tmp: tempfile::TempDir,
}

fn ledger() -> (StateWriter, Ledger) {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("state.db");
    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&lines);
    let w = StateWriter::open(&path)
        .unwrap()
        .with_hold_log(move |line| sink.lock().unwrap().push(line.to_string()));
    (
        w,
        Ledger {
            path,
            lines,
            _tmp: tmp,
        },
    )
}

impl Ledger {
    fn lines(&self) -> Vec<String> {
        self.lines.lock().unwrap().clone()
    }

    fn events_x(&self) -> i64 {
        events_x(&self.path)
    }
}

fn events_x(path: &Path) -> i64 {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .unwrap()
        .query_row("SELECT count(*) FROM events WHERE kind = 'x'", [], |r| {
            r.get(0)
        })
        .unwrap()
}

fn append(w: &StateWriter) -> makiba::db::Result<()> {
    w.append_event(&NewEvent {
        kind: "x",
        ..Default::default()
    })
    .map(|_| ())
}

/// The one line logged: it has the prefix, ends with ` s by <site>`, and its seconds are at least
/// the hold.
fn one_line_for(l: &Ledger, site: &str) {
    let lines = l.lines();
    assert_eq!(lines.len(), 1, "lines: {lines:?}");
    let line = &lines[0];
    let rest = line
        .strip_prefix(PREFIX)
        .unwrap_or_else(|| panic!("no prefix: {line:?}"));
    let suffix = format!(" s by {site}");
    let secs = rest
        .strip_suffix(&suffix)
        .unwrap_or_else(|| panic!("{line:?} does not end with {suffix:?}"));
    let secs: f64 = secs
        .parse()
        .unwrap_or_else(|e| panic!("{secs:?} is not a number: {e}"));
    assert!(secs >= 1.2, "held {secs} s: {line:?}");
}

#[test]
fn a_transaction_held_over_a_second_is_logged_with_its_call_site() {
    let (w, l) = ledger();
    #[rustfmt::skip]
    let (r, line) = (w.transaction(|w| { append(w)?; std::thread::sleep(HOLD); Ok(()) }), line!());
    assert!(r.is_ok(), "{r:?}");
    one_line_for(&l, &format!("tests/write_hold_log.rs:{line}"));
    assert_eq!(l.events_x(), 1);
}

#[test]
fn a_rolled_back_transaction_held_over_a_second_is_logged() {
    let (w, l) = ledger();
    #[rustfmt::skip]
    let (r, line) = (w.transaction(|w| -> makiba::db::Result<()> { append(w)?; std::thread::sleep(HOLD); Err(makiba::db::Error::InTransaction("test")) }), line!());
    assert!(r.is_err());
    assert_eq!(l.events_x(), 0, "a rolled back write stands");
    one_line_for(&l, &format!("tests/write_hold_log.rs:{line}"));
}

#[test]
fn begin_as_names_its_label_and_site() {
    let (w, l) = ledger();
    let (r, line) = (w.begin_as("door peek"), line!());
    r.unwrap();
    append(&w).unwrap();
    std::thread::sleep(HOLD);
    w.commit().unwrap();
    one_line_for(&l, &format!("door peek (tests/write_hold_log.rs:{line})"));
    assert_eq!(l.events_x(), 1);
}

#[test]
fn begin_as_inside_a_transaction_fails() {
    let (w, l) = ledger();
    w.begin().unwrap();
    assert!(w.begin_as("door x").is_err());
    w.rollback().unwrap();
    assert!(w.conn().is_autocommit());
    assert_eq!(l.lines(), Vec::<String>::new());
}

#[test]
fn a_short_transaction_and_a_joined_one_log_nothing() {
    let (w, l) = ledger();
    w.transaction(append).unwrap();
    w.transaction(|w| {
        append(w)?;
        w.transaction(append)
    })
    .unwrap();
    assert_eq!(l.events_x(), 3);
    assert_eq!(l.lines(), Vec::<String>::new());
}

#[test]
fn a_writer_dropped_inside_a_slow_transaction_logs_the_hold() {
    let (w, l) = ledger();
    let (r, line) = (w.begin(), line!());
    r.unwrap();
    append(&w).unwrap();
    std::thread::sleep(HOLD);
    drop(w);
    one_line_for(&l, &format!("tests/write_hold_log.rs:{line}"));
    assert_eq!(l.events_x(), 0, "a dropped transaction's write stands");
}
