//! Issue 502: the secretary stores a handover without being replaced, and the secretary that
//! `makiba up` launches resumes from it, followed by the records part, until a launch that carried
//! it is verified.
//!
//! No daemon and no socket, as tests/renew_resume_text.rs: the requests go through the door
//! (`Door::handle`, as an agent's do), and `makibad`'s secretary pass runs on the stand-in base,
//! which lists no terminal. "Down and up" is what `makiba up` writes into the ledger after a stop
//! of the base: every secretary launch not failed is failed with the reason `the base was
//! stopped`, then a `commands` row (actor `person`, verb `up`, `accepted`) that the running
//! `makibad`'s pass takes up. A launch that reached its secretary is marked `verified` with its
//! `verified_at`, as the verification pass writes it. The new secretary's task text is its
//! launch's `brief` in `run_launch`.

use makiba::base::stub::Stub;
use makiba::daemon::door::{Door, issue_key};
use makiba::daemon::secretary::Secretary;
use makiba::db::writer::{RunUpsert, StateWriter};
use makiba::spec::{ErrorKind, Role};
use serde_json::{Value, json};
use std::sync::Arc;

/// Today's fixed resume line (spec section 5.3).
const RESUME: &str = "You are the secretary of this makiba organization, launched to take over from the secretary before you. Your rules are appended to your system prompt. First call the inbox tool: messages that waited for the secretary are there. Workers keep running; do not dispatch their tasks again. Then resume from what follows and tell the person you are back.";

/// The head of the records part.
const MARK: &str = "--- Read from the records (state.db) at this launch, not from the handover ---";

/// The first-start brief begins so.
const FIRST: &str = "You are the secretary of this makiba organization: the one agent";

const H: &str = "Decided by word today: t1 waits for the person's go. In progress: whether to split t2 in two; leaning yes.";
const R: &str = "Renewed: the person asked for a short summary each evening.";

fn none_left() -> String {
    format!("{RESUME}\n\nNo handover was left. What state.db holds open:")
}

struct Org {
    _tmp: tempfile::TempDir,
    w: StateWriter,
    door: Door,
    s: Secretary,
    base: Stub,
}

/// A database with run `t1` ("Write the docs") at `review` and one open pending entry of it; then
/// the first `up`, whose launch is verified.
fn org() -> Org {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let w = StateWriter::open(&dir.join("state.db")).unwrap();
    w.ensure_project("p", None, None).unwrap();
    w.upsert_run(&RunUpsert {
        task_id: "t1",
        project_slug: "p",
        title: Some("Write the docs"),
        status: Some("queued"),
        ..Default::default()
    })
    .unwrap();
    w.conn()
        .execute("UPDATE runs SET status = 'review' WHERE task_id = 't1'", [])
        .unwrap();
    w.conn()
        .execute(
            "INSERT INTO pending (run_id, kind, question, status)
             VALUES ((SELECT id FROM runs WHERE task_id = 't1'), 'question', 'Merge it?', 'open')",
            [],
        )
        .unwrap();
    let door = Door::new(
        StateWriter::open(&dir.join("state.db")).unwrap(),
        Arc::new(Stub::new()),
        "fake".into(),
    );
    let s = Secretary::from_env(dir);
    let mut o = Org {
        _tmp: tmp,
        w,
        door,
        s,
        base: Stub::new(),
    };
    let (spawn, brief) = o.up();
    assert!(brief.starts_with(FIRST), "{brief}");
    o.verify(&spawn);
    o
}

impl Org {
    fn launches(&self) -> i64 {
        self.w
            .conn()
            .query_row(
                "SELECT count(*) FROM run_launch WHERE role = 'secretary'",
                [],
                |r| r.get(0),
            )
            .unwrap()
    }

    /// (spawn_id, brief) of the newest secretary launch.
    fn newest(&self) -> (String, String) {
        self.w
            .conn()
            .query_row(
                "SELECT spawn_id, brief FROM run_launch WHERE role = 'secretary'
                 ORDER BY id DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("no secretary launch")
    }

    fn pass(&mut self) {
        self.s.pass(&self.w, &self.base).unwrap();
    }

    /// Down after a stop of the base, then `makiba up` asking the running makibad: one new
    /// secretary launch, and the `up` row done. Its (spawn_id, brief).
    fn up(&mut self) -> (String, String) {
        let before = self.launches();
        self.w
            .conn()
            .execute(
                "UPDATE run_launch SET stage = 'failed', failed_reason = 'the base was stopped'
                 WHERE role = 'secretary' AND stage != 'failed'",
                [],
            )
            .unwrap();
        self.w
            .conn()
            .execute(
                "INSERT INTO commands (actor, verb, args_json, status)
                 VALUES ('person', 'up', '{\"secretary\":true}', 'accepted')",
                [],
            )
            .unwrap();
        let up = self.w.conn().last_insert_rowid();
        self.pass();
        assert_eq!(self.launches(), before + 1, "up launched no secretary");
        let status: String = self
            .w
            .conn()
            .query_row("SELECT status FROM commands WHERE id = ?1", [up], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(status, "done");
        self.newest()
    }

    /// The launch's first turn (or working status) was seen.
    fn verify(&self, spawn: &str) {
        self.w
            .conn()
            .execute(
                "UPDATE run_launch SET stage = 'verified',
                   verified_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
                 WHERE spawn_id = ?1",
                [spawn],
            )
            .unwrap();
    }

    /// The launch was never verified.
    fn fail(&self, spawn: &str) {
        self.w
            .conn()
            .execute(
                "UPDATE run_launch SET stage = 'failed', failed_reason = 'x', verified_at = NULL
                 WHERE spawn_id = ?1",
                [spawn],
            )
            .unwrap();
    }

    /// A request of the current secretary (a key of its own, bound to the newest launch).
    fn secretary(&self, verb: &str, args: Value) -> makiba::daemon::door::Response {
        let (spawn, _) = self.newest();
        let key = issue_key(&self.w, Role::Secretary, "secretary", None, Some(&spawn)).unwrap();
        self.door.handle(Some(&key), verb, args, None, true)
    }

    fn store(&self, text: &str) {
        let r = self
            .secretary("handover", json!({"handover": text}))
            .unwrap_or_else(|e| panic!("handover: {e:?}"));
        assert_eq!(r, json!({"handover": "stored"}));
    }

    fn renew_status(&self) -> String {
        self.w
            .conn()
            .query_row(
                "SELECT status FROM commands WHERE verb = 'renew' AND actor = 'secretary'
                 ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap()
    }

    /// The secretary asks to be renewed with `text`; passes until its launch is added. That
    /// launch's spawn_id.
    fn renew(&mut self, text: &str) -> String {
        let r = self
            .secretary("renew", json!({"handover": text}))
            .unwrap_or_else(|e| panic!("renew: {e:?}"));
        assert_eq!(r, json!({"renewal": "accepted"}));
        let before = self.launches();
        for _ in 0..3 {
            self.pass();
        }
        assert_eq!(
            self.launches(),
            before + 1,
            "the renewal launched no secretary"
        );
        let (spawn, brief) = self.newest();
        assert!(brief.starts_with(&format!("{RESUME}\n\n{text}")), "{brief}");
        spawn
    }
}

/// The handover the secretary stored reaches the secretary `up` launches, with the records part
/// after it; once that launch is verified, the next `up` does not give it again.
#[test]
fn a_stored_handover_reaches_the_secretary_up_launches_and_only_once() {
    let mut o = org();

    let empty = o.secretary("handover", json!({"handover": ""}));
    match empty {
        Err(e) => assert_eq!(e.kind, ErrorKind::Usage, "{e:?}"),
        Ok(v) => panic!("an empty handover was taken: {v}"),
    }
    o.store(H);
    let (verb, status): (String, String) =
        o.w.conn()
            .query_row(
                "SELECT verb, status FROM commands WHERE actor = 'secretary'
               AND json_extract(args_json, '$.handover') = ?1",
                [H],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
    assert_eq!((verb.as_str(), status.as_str()), ("handover", "done"));

    let (spawn, brief) = o.up();
    assert!(brief.starts_with(&format!("{RESUME}\n\n{H}")), "{brief}");
    let at = brief
        .find(MARK)
        .unwrap_or_else(|| panic!("no records part: {brief}"));
    assert!(at > RESUME.len() + H.len(), "{brief}");
    let records = &brief[at..];
    assert!(
        records.contains("Runs waiting for review:\n- t1: Write the docs\n"),
        "{brief}"
    );
    assert!(records.contains("(open, task t1): Merge it?"), "{brief}");

    o.verify(&spawn);
    let (_, brief) = o.up();
    assert!(!brief.contains(H), "{brief}");
    assert!(brief.starts_with(&none_left()), "{brief}");
}

/// A launch that carried the handover and was never verified did not use it: the next `up`
/// gives it again.
#[test]
fn a_handover_whose_launch_never_verified_is_given_again() {
    let mut o = org();
    o.store(H);
    let (spawn, brief) = o.up();
    assert!(brief.starts_with(&format!("{RESUME}\n\n{H}")), "{brief}");
    o.fail(&spawn);
    let (_, brief) = o.up();
    assert!(brief.starts_with(&format!("{RESUME}\n\n{H}")), "{brief}");
}

/// A renewal after a stored handover carries its own; once its launch is verified, `up` gives
/// neither. No handover is stored while the renewal is under way.
#[test]
fn a_renewal_after_a_stored_handover_leaves_up_nothing_to_repeat() {
    let mut o = org();
    o.store(H);
    let r = o
        .secretary("renew", json!({"handover": R}))
        .unwrap_or_else(|e| panic!("renew: {e:?}"));
    assert_eq!(r, json!({"renewal": "accepted"}));
    match o.secretary("handover", json!({"handover": "During the renewal."})) {
        Err(e) => assert_eq!(e.kind, ErrorKind::Conflict, "{e:?}"),
        Ok(v) => panic!("a handover was stored during a renewal: {v}"),
    }
    let before = o.launches();
    for _ in 0..3 {
        o.pass();
    }
    assert_eq!(o.launches(), before + 1);
    let (spawn, brief) = o.newest();
    assert!(brief.starts_with(&format!("{RESUME}\n\n{R}")), "{brief}");
    o.verify(&spawn);
    o.pass();
    assert_eq!(o.renew_status(), "done");

    let (_, brief) = o.up();
    assert!(!brief.contains(H) && !brief.contains(R), "{brief}");
    assert!(brief.starts_with(&none_left()), "{brief}");
}

/// A renewal whose launch failed never delivered its handover: `up` gives it, once.
#[test]
fn a_failed_renewals_handover_is_given_by_up() {
    let mut o = org();
    let spawn = o.renew(R);
    o.fail(&spawn);
    o.pass();
    assert_eq!(o.renew_status(), "failed");

    let (spawn, brief) = o.up();
    assert!(brief.starts_with(&format!("{RESUME}\n\n{R}")), "{brief}");
    o.verify(&spawn);
    let (_, brief) = o.up();
    assert!(!brief.contains(R), "{brief}");
    assert!(brief.starts_with(&none_left()), "{brief}");
}

/// makibad's relaunch after a failed renewal carries that renewal's handover; once the relaunch
/// is verified, `up` does not give it again.
#[test]
fn a_relaunch_that_carried_a_handover_used_it_for_up() {
    let mut o = org();
    let spawn = o.renew(R);
    o.fail(&spawn);
    o.pass();
    assert_eq!(o.renew_status(), "failed");

    // No `up` row: the pass relaunches the secretary that is down.
    let before = o.launches();
    for _ in 0..3 {
        o.pass();
    }
    assert_eq!(o.launches(), before + 1, "makibad did not relaunch");
    let relaunch: (String, i64) =
        o.w.conn()
            .query_row(
                "SELECT json_extract(args_json, '$.handover'), json_extract(args_json, '$.from')
             FROM commands WHERE actor = 'makibad' AND verb = 'renew' ORDER BY id DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
    let renewal: i64 =
        o.w.conn()
            .query_row(
                "SELECT id FROM commands WHERE actor = 'secretary' AND verb = 'renew'",
                [],
                |r| r.get(0),
            )
            .unwrap();
    assert_eq!(relaunch, (R.to_string(), renewal));
    let (spawn, brief) = o.newest();
    assert!(brief.starts_with(&format!("{RESUME}\n\n{R}")), "{brief}");
    o.verify(&spawn);
    o.pass();

    let (_, brief) = o.up();
    assert!(!brief.contains(R), "{brief}");
    assert!(brief.starts_with(&none_left()), "{brief}");
}
