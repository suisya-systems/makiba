//! Issue 506: `Local::list` answers with the terminals as they were at one moment, never with
//! `NotFound` for a terminal that `end` took while the list was being read.
//!
//! The curator tests (tests/curator.rs, tests/curator_new_notes.rs) poll `list` while makibad ends
//! the curator, and failed now and then with `NotFound("no terminal \"curator\"")` from `list`
//! itself. Here one thread lists in a loop while another ends terminals and launches new ones, for
//! a bounded time: a stress test, not a proof that a given list met a given end. A list that reads
//! the names under one lock and each terminal under another gives `NotFound` when an `end` falls
//! between them (measured: some hundreds of about 136000 lists, every run); a list read under one
//! lock never does.

mod common;

use common::local::Local;
use makiba::base::{Base, LaunchSpec, TerminalRef};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

/// Terminals up at any time: the more a list reads, the wider the window an `end` can fall into.
const UP: usize = 24;
/// How long the two threads race.
const RACE: Duration = Duration::from_secs(4);

fn spec(name: &str) -> LaunchSpec {
    LaunchSpec {
        argv: vec!["sleep".into(), "60".into()],
        cwd: "/".into(),
        name: name.into(),
        tags: BTreeMap::new(),
        env: BTreeMap::new(),
        unset: vec![],
    }
}

#[test]
fn a_list_while_terminals_are_ended_never_answers_not_found() {
    let local = Local::new();
    let mut up: Vec<String> = (0..UP).map(|i| format!("t{i}")).collect();
    for name in &up {
        local.launch(&spec(name)).unwrap();
    }

    // Both threads start together, and the lister stops only after the last end: every list it
    // makes falls while terminals are being ended.
    let (stop, start) = (Arc::new(AtomicBool::new(false)), Arc::new(Barrier::new(2)));
    let lister = {
        let (local, stop, start) = (local.clone(), Arc::clone(&stop), Arc::clone(&start));
        std::thread::spawn(move || {
            let (mut lists, mut errors) = (0u64, Vec::new());
            start.wait();
            while !stop.load(Ordering::SeqCst) {
                match local.list() {
                    Ok(_) => {}
                    Err(e) => errors.push(format!("{e:?}")),
                }
                lists += 1;
            }
            (lists, errors)
        })
    };

    // End the oldest terminal and launch a new one, again and again.
    start.wait();
    let (until, mut next, mut ends) = (Instant::now() + RACE, UP, 0u64);
    while Instant::now() < until {
        let oldest = up.remove(0);
        local.end(&TerminalRef(oldest)).unwrap();
        ends += 1;
        let name = format!("t{next}");
        next += 1;
        local.launch(&spec(&name)).unwrap();
        up.push(name);
    }
    stop.store(true, Ordering::SeqCst);
    let (lists, errors) = lister.join().unwrap();

    for name in &up {
        local.end(&TerminalRef(name.clone())).unwrap();
    }
    assert!(
        lists > 0 && ends > 0,
        "the race ran: {lists} lists, {ends} ends"
    );
    assert!(
        errors.is_empty(),
        "{} of {lists} lists failed while {ends} terminals were ended; the first: {}",
        errors.len(),
        errors[0]
    );
    assert!(local.list().unwrap().is_empty(), "every terminal was ended");
}
