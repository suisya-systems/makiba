//! M0-3: the stand-in for the base, and the real base for CI. The same test body runs on the
//! stand-in (default) and on the real base (`MAKIBA_TEST_REAL_BASE=1`).

mod common;

use common::{LINE, base, persons_folders, real_wanted, snapshot};
use makiba::base::stub::Stub;
use makiba::base::{Base, BaseError, BaseEvent, LaunchSpec, State, TerminalEvent, TerminalRef};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

fn spec(name: &str) -> LaunchSpec {
    LaunchSpec {
        argv: vec!["sh".into(), "-c".into(), format!("echo {LINE}; sleep 60")],
        cwd: std::env::temp_dir(),
        name: name.into(),
        tags: BTreeMap::new(),
        env: BTreeMap::new(),
        unset: Vec::new(),
    }
}

fn wait_for_line(base: &dyn Base, t: &TerminalRef) -> bool {
    let until = Instant::now() + Duration::from_secs(30);
    while Instant::now() < until {
        if base
            .read(t, 50)
            .is_ok_and(|lines| lines.iter().any(|l| l.contains(LINE)))
        {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

/// Launch a terminal that prints a line, read the line, end it.
#[test]
fn a_terminal_prints_a_line_which_is_read_and_then_ended() {
    let base = base();
    base.start().unwrap();
    assert_eq!(
        base.name(),
        if real_wanted() { "herdr-core" } else { "stub" }
    );
    let t = base.launch(&spec("fixture-line")).unwrap();
    assert!(wait_for_line(&*base, &t), "the printed line was not read");
    base.end(&t).unwrap();
    assert!(matches!(base.get(&t), Err(BaseError::NotFound(_))));
}

/// With the real base the place is its own and under a temp dir; the stand-in has none.
#[test]
fn a_real_base_runs_in_a_place_of_its_own_under_a_temp_dir() {
    let base = base();
    match (&base.place, real_wanted()) {
        (None, false) => {}
        (Some(place), true) => {
            assert_eq!(place.session, format!("makiba-test-{}", std::process::id()));
            let temp = std::env::temp_dir();
            for folder in place.folders() {
                assert!(
                    folder.starts_with(&temp),
                    "{} is not under {}",
                    folder.display(),
                    temp.display()
                );
            }
            for person in persons_folders() {
                assert!(
                    !place.folders().iter().any(|f| f.starts_with(&person)),
                    "the place is inside a person's folder {}",
                    person.display()
                );
            }
        }
        (place, real) => panic!("place {place:?} does not match real={real}"),
    }
}

/// A run never touches the folders a person's own base would use.
#[test]
fn a_run_leaves_the_persons_folders_as_they_were() {
    let before = snapshot(&persons_folders());
    {
        let base = base();
        let t = base.launch(&spec("fixture-isolation")).unwrap();
        assert!(wait_for_line(&*base, &t));
        base.end(&t).unwrap();
    }
    assert_eq!(before, snapshot(&persons_folders()));
}

/// Dropping the real base removes its place; nothing of it is left on disk.
#[test]
fn dropping_the_fixture_removes_the_place() {
    let place = {
        let base = base();
        let t = base.launch(&spec("fixture-drop")).unwrap();
        base.end(&t).unwrap();
        base.place.clone()
    };
    if let Some(place) = place {
        assert!(!place.root.exists(), "{} was left", place.root.display());
    }
}

// --- the stand-in's own behaviour: what the interface means ---

#[test]
fn the_stand_in_refuses_an_unsupported_version_at_hello() {
    let ok = Stub::new().hello().unwrap();
    assert_eq!(ok.version, makiba::base::version::PINNED);
    let err = Stub::new().with_version("0.1.22").hello().unwrap_err();
    assert!(
        matches!(err, BaseError::UnsupportedVersion { .. }),
        "{err:?}"
    );
}

#[test]
fn a_send_only_if_idle_is_refused_unless_the_terminal_is_idle() {
    let stub = Stub::scripted(&[LINE]);
    let t = stub.launch(&spec("a")).unwrap();
    stub.send(&t, "hello", true).unwrap(); // a launched terminal is idle
    stub.set_state(&t, State::Working);
    assert!(matches!(
        stub.send(&t, "hello", true),
        Err(BaseError::StatusMismatch(_))
    ));
    stub.send(&t, "hello", false).unwrap();
    assert!(stub.read(&t, 50).unwrap().iter().any(|l| l == "hello"));
}

#[test]
fn an_exited_terminal_is_finished_and_an_ended_one_is_not_found() {
    let stub = Stub::scripted(&[LINE]);
    let t = stub.launch(&spec("a")).unwrap();
    stub.exit(&t, 3);
    let seen = stub.get(&t).unwrap();
    assert!(!seen.alive);
    assert_eq!(seen.exit_code, Some(3));
    assert!(matches!(
        stub.send(&t, "x", false),
        Err(BaseError::Finished(_))
    ));
    stub.end(&t).unwrap();
    assert!(matches!(stub.get(&t), Err(BaseError::NotFound(_))));
    assert!(matches!(stub.read(&t, 1), Err(BaseError::NotFound(_))));
    assert!(matches!(stub.end(&t), Err(BaseError::NotFound(_))));
}

#[test]
fn list_holds_only_terminals_tagged_with_this_organization() {
    let stub = Stub::scripted(&[LINE]).organization("/state/a");
    let mut mine = spec("mine");
    mine.tags.insert("makiba.org".into(), "/state/a".into());
    let mut other = spec("other");
    other.tags.insert("makiba.org".into(), "/state/b".into());
    stub.launch(&mine).unwrap();
    stub.launch(&other).unwrap();
    let names: Vec<_> = stub.list().unwrap().into_iter().map(|t| t.name).collect();
    assert_eq!(names, ["mine"]);
}

#[test]
fn events_start_with_a_resync_and_then_follow_in_order_without_ending() {
    let stub = Stub::scripted(&[LINE]);
    let mut events = stub.events().unwrap();
    assert!(matches!(events.next(), Some(BaseEvent::Resync(list)) if list.is_empty()));
    let t = stub.launch(&spec("a")).unwrap();
    stub.set_state(&t, State::Working);
    stub.exit(&t, 0);
    stub.end(&t).unwrap();
    let kinds: Vec<&str> = (0..4)
        .map(|_| match events.next().expect("the stream never ends") {
            BaseEvent::Event(TerminalEvent::Created(_)) => "created",
            BaseEvent::Event(TerminalEvent::StatusChanged(_)) => "status",
            BaseEvent::Event(TerminalEvent::Exited(_)) => "exited",
            BaseEvent::Event(TerminalEvent::Removed(_)) => "removed",
            BaseEvent::Resync(_) => "resync",
        })
        .collect();
    assert_eq!(kinds, ["created", "status", "exited", "removed"]);
}
