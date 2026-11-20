//! The curator at the threshold (Issue 39, carried from claude-org-ja `check_curate_threshold.py`).
//!
//! After a close (`run_closed`, written by the door) the raw notes of the knowledge folder are
//! counted; at `RAW_THRESHOLD` a launch of the curator that belongs to no run is added
//! (`launch::add_launch`, Issue 166), one at a time. A curator that sent its report to the
//! secretary is ended and its key revoked (Issue 177); one past its time limit is ended, its key
//! revoked and the secretary told. Spec sections 2, 5.3 and 10.
//!
//! The base is the truth for "one at a time": a curator terminal of this organization, or a
//! curator launch not yet verified, means no second launch, so a restarted `makibad` needs no
//! stored flag. The count is made once after every start of `makibad` and once per new close.

use crate::base::{Base, Terminal};
use crate::db::Result;
use crate::db::writer::{NewEvent, StateWriter};
use crate::spec::{self, TAG_ORG, TAG_ROLE, TAG_SPAWN};
use rusqlite::OptionalExtension;
use serde_json::json;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Raw notes at which the curator is launched (the carried `RAW_THRESHOLD`).
pub const RAW_THRESHOLD: usize = 5;
/// The curator's time limit (the owner's decision, Issue 39), from its verification.
pub const TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// The test seam that sets the time limit in milliseconds (spec section 2).
pub const TIMEOUT_ENV: &str = "MAKIBA_CURATOR_TIMEOUT_MS";
/// Where the knowledge notes live (spec section 2).
pub const KNOWLEDGE_ENV: &str = "MAKIBA_KNOWLEDGE_DIR";
/// How often a pass runs at most.
const EVERY: Duration = Duration::from_secs(1);

const BRIEF: &str = "Run one curation cycle over the notes in this folder (raw/ and raw/archive/) as your rules say. When it is done, send the result to the secretary with send (there is no task, so not report), once, as your last step: that send ends your terminal.";

/// `$MAKIBA_KNOWLEDGE_DIR`, default `~/makiba-knowledge`.
pub fn knowledge_dir() -> PathBuf {
    if let Some(d) = std::env::var_os(KNOWLEDGE_ENV).filter(|d| !d.is_empty()) {
        return d.into();
    }
    std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join("makiba-knowledge"))
        .unwrap_or_else(|| PathBuf::from("makiba-knowledge"))
}

/// The time limit: the seam when it holds a number, otherwise `TIMEOUT`.
pub fn timeout_from_env() -> Duration {
    std::env::var(TIMEOUT_ENV)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .map(Duration::from_millis)
        .unwrap_or(TIMEOUT)
}

/// Regular files directly in `<knowledge>/raw/`: not subdirectories (`archive/`), not names that
/// start with `.`. No `raw/` is 0 and a file that vanishes during the scan is skipped (a move to
/// `archive/`); any other read error is an error, never a silent 0, so no launch is suppressed
/// unseen.
pub fn count_raw(knowledge: &Path) -> io::Result<usize> {
    let raw = knowledge.join("raw");
    match std::fs::metadata(&raw) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => return Ok(0),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    }
    let mut n = 0;
    for entry in std::fs::read_dir(&raw)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        match std::fs::metadata(entry.path()) {
            Ok(m) if m.is_file() => n += 1,
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

pub struct Curator {
    org: String,
    knowledge: PathBuf,
    timeout: Duration,
    agent: String,
    /// The newest `run_closed` event already counted for; -1 before the first count.
    seen: i64,
    next: Instant,
    /// Ended curator terminals whose revoke (and notice) the database refused: `(spawn, past)`,
    /// written by the next pass, as the base no longer lists them.
    unsettled: Vec<(String, bool)>,
}

impl Curator {
    /// From the environment, for the organization of `state`.
    pub fn from_env(state: &Path) -> Curator {
        let org = std::fs::canonicalize(state).unwrap_or_else(|_| state.to_path_buf());
        let agent = std::env::var(super::door::LAUNCH_AGENT_ENV)
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "claude_code".into());
        Curator::new(
            &org.to_string_lossy(),
            knowledge_dir(),
            timeout_from_env(),
            agent,
        )
    }

    pub fn new(org: &str, knowledge: PathBuf, timeout: Duration, agent: String) -> Curator {
        Curator {
            org: org.to_string(),
            knowledge,
            timeout,
            agent,
            seen: -1,
            next: Instant::now(),
            unsettled: Vec::new(),
        }
    }

    /// One pass, at most once a second: records the launches that started, ends the curator
    /// terminals that are finished or past the limit, and launches one when a close was not yet
    /// counted for and the raw notes are at the threshold.
    pub fn pass(&mut self, w: &StateWriter, base: &dyn Base) -> Result<()> {
        if Instant::now() < self.next {
            return Ok(());
        }
        self.next = Instant::now() + EVERY;
        self.record_started(w)?;
        for (spawn, past) in std::mem::take(&mut self.unsettled) {
            if let Err(e) = self.finish(w, &spawn, past) {
                eprintln!("makibad: cannot revoke the ended curator's keys: {e}");
                self.unsettled.push((spawn, past));
            }
        }
        // The base cannot list: nothing is decided, the next pass tries again.
        let Ok(terminals) = base.list() else {
            return Ok(());
        };
        let mut running = false;
        let ours: Vec<&Terminal> = terminals.iter().filter(|t| self.is_ours(t)).collect();
        for t in ours {
            running |= !self.settle(w, base, t)?;
        }
        let latest: i64 = w.conn().query_row(
            "SELECT COALESCE(max(id), 0) FROM events WHERE kind = 'run_closed'",
            [],
            |r| r.get(0),
        )?;
        if latest == self.seen {
            return Ok(());
        }
        let in_flight: bool = w.conn().query_row(
            "SELECT EXISTS (SELECT 1 FROM run_launch
                            WHERE role = 'curator' AND stage NOT IN ('verified', 'failed'))",
            [],
            |r| r.get(0),
        )?;
        // This close is answered, whatever the answer: a curator that runs is not asked again.
        // A launch the database refused leaves it unanswered, for the next pass.
        let before = std::mem::replace(&mut self.seen, latest);
        if running || in_flight {
            return Ok(());
        }
        match count_raw(&self.knowledge) {
            Err(e) => eprintln!(
                "makibad: cannot count the raw notes in {}: {e}",
                self.knowledge.display()
            ),
            Ok(n) if n >= RAW_THRESHOLD => {
                if let Err(e) = super::launch::add_launch(
                    w,
                    spec::Role::Curator,
                    &self.knowledge,
                    BRIEF,
                    None,
                    &self.agent,
                ) {
                    eprintln!("makibad: cannot add the curator launch: {e}");
                    self.seen = before;
                }
            }
            Ok(_) => {}
        }
        Ok(())
    }

    fn is_ours(&self, t: &Terminal) -> bool {
        t.tags.get(TAG_ROLE).map(String::as_str) == Some("curator")
            && t.tags.get(TAG_ORG) == Some(&self.org)
    }

    /// `curator_started` for every curator launch that has a `worker_spawned`, once.
    fn record_started(&self, w: &StateWriter) -> Result<()> {
        let spawns: Vec<String> = {
            let mut st = w.conn().prepare(
                "SELECT l.spawn_id FROM run_launch l
                 WHERE l.role = 'curator'
                   AND EXISTS (SELECT 1 FROM events e WHERE e.kind = 'worker_spawned'
                               AND json_extract(e.payload_json, '$.spawn_id') = l.spawn_id)
                   AND NOT EXISTS (SELECT 1 FROM events e WHERE e.kind = 'curator_started'
                                   AND json_extract(e.payload_json, '$.spawn_id') = l.spawn_id)",
            )?;
            st.query_map([], |r| r.get(0))?
                .collect::<std::result::Result<_, _>>()?
        };
        for spawn in spawns {
            w.append_event(&NewEvent {
                kind: "curator_started",
                actor: Some("makibad"),
                payload: Some(&json!({"spawn_id": spawn})),
                ..Default::default()
            })?;
        }
        Ok(())
    }

    /// A curator terminal that reported (its keys revoked first), is finished (its program exited,
    /// the base keeps it and its name) or is past the time limit is ended, its key revoked; only
    /// the last with a notice to the secretary.
    /// Returns whether it is gone. A failed end is tried again by the next pass.
    fn settle(&mut self, w: &StateWriter, base: &dyn Base, t: &Terminal) -> Result<bool> {
        let spawn = t.tags.get(TAG_SPAWN).cloned().unwrap_or_default();
        // Ended already; what is left is written at the start of a pass.
        if self.unsettled.iter().any(|(s, _)| *s == spawn) {
            return Ok(true);
        }
        if t.alive && self.reported(w, &spawn)? {
            // Keys first: a failed end is tried again by the next pass, the report still there.
            w.transaction(|w| super::resume::revoke_keys(w, &spawn))?;
            if let Err(e) = base.end(&t.id) {
                eprintln!("makibad: cannot end the curator terminal {}: {e}", t.id.0);
                return Ok(false);
            }
            return Ok(true);
        }
        let past = t.alive && self.age_ms(w, &spawn)?.is_some_and(|a| a >= self.limit());
        if t.alive && !past {
            return Ok(false);
        }
        if let Err(e) = base.end(&t.id) {
            eprintln!("makibad: cannot end the curator terminal {}: {e}", t.id.0);
            return Ok(false);
        }
        if let Err(e) = self.finish(w, &spawn, past) {
            self.unsettled.push((spawn, past));
            return Err(e);
        }
        Ok(true)
    }

    /// After the end: the keys revoked and, past the time limit, the notice, in one transaction.
    fn finish(&self, w: &StateWriter, spawn: &str, past: bool) -> Result<()> {
        w.transaction(|w| {
            super::resume::revoke_keys(w, spawn)?;
            if past {
                let body = format!(
                    "The curator was ended after its time limit of {} minutes. Its cycle may be unfinished: the raw notes were not all curated.",
                    self.timeout.as_secs() / 60
                );
                super::verify::notify_secretary(w, None, &body)?;
            }
            Ok(())
        })
    }

    /// Whether the curator of launch `spawn` sent its report (Issue 177): a message of the curator
    /// to the secretary recorded since the launch was added (its key is issued after that), once
    /// the launch is `verified` or `failed`, so the end is never taken for a failed start. One
    /// curator runs at a time, so a message since its launch is its own.
    fn reported(&self, w: &StateWriter, spawn: &str) -> Result<bool> {
        Ok(w.conn().query_row(
            "SELECT EXISTS (SELECT 1 FROM run_launch l, messages m JOIN events e ON e.id = m.event_id
                            WHERE l.spawn_id = ?1 AND l.stage IN ('verified', 'failed')
                              AND m.sender = ?2 AND m.recipient = ?3 AND m.kind = 'message'
                              AND e.occurred_at >= l.created_at)",
            [spawn, spec::AGENT_CURATOR, spec::AGENT_SECRETARY],
            |r| r.get(0),
        )?)
    }

    fn limit(&self) -> i64 {
        self.timeout.as_millis() as i64
    }

    /// Milliseconds the curator of launch `spawn` has run: since it was verified; for a failed
    /// launch (its terminal is left), since `worker_spawned`. `None` while the launch is still
    /// under way: verification has its own time limit (verify.rs), and a slow start must not use
    /// up the curator's.
    fn age_ms(&self, w: &StateWriter, spawn: &str) -> Result<Option<i64>> {
        Ok(w.conn()
            .query_row(
                "SELECT CAST((julianday('now') - julianday(CASE l.stage
                        WHEN 'verified' THEN l.verified_at
                        WHEN 'failed' THEN COALESCE(
                          (SELECT max(e.occurred_at) FROM events e WHERE e.kind = 'worker_spawned'
                           AND json_extract(e.payload_json, '$.spawn_id') = l.spawn_id),
                          l.created_at) END)) * 86400000 AS INTEGER)
                 FROM run_launch l WHERE l.spawn_id = ?1",
                [spawn],
                |r| r.get(0),
            )
            .optional()?
            .flatten())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn notes(names: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let raw = dir.path().join("raw");
        fs::create_dir_all(raw.join("archive")).unwrap();
        for n in names {
            fs::write(raw.join(n), "note").unwrap();
        }
        dir
    }

    fn five() -> Vec<String> {
        (1..=5).map(|i| format!("n{i}.md")).collect()
    }

    fn count(dir: &tempfile::TempDir) -> usize {
        count_raw(dir.path()).unwrap()
    }

    #[test]
    fn four_notes_are_below_the_threshold_and_five_reach_it() {
        let names = five();
        let four = notes(&names[..4].iter().map(String::as_str).collect::<Vec<_>>());
        assert!(count(&four) < RAW_THRESHOLD);
        let all = notes(&names.iter().map(String::as_str).collect::<Vec<_>>());
        assert_eq!(count(&all), RAW_THRESHOLD);
    }

    #[test]
    fn archive_and_hidden_entries_are_not_counted() {
        let dir = notes(&["a.md", ".gitkeep", ".hidden.md"]);
        fs::write(dir.path().join("raw/archive/old.md"), "x").unwrap();
        fs::create_dir_all(dir.path().join("raw/sub")).unwrap();
        fs::write(dir.path().join("raw/sub/inner.md"), "x").unwrap();
        assert_eq!(count(&dir), 1);
    }

    #[test]
    fn no_raw_folder_is_zero() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(count_raw(dir.path()).unwrap(), 0);
        assert_eq!(count_raw(&dir.path().join("missing")).unwrap(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_raw_folder_is_an_error_not_a_zero() {
        use std::os::unix::fs::PermissionsExt;
        let dir = notes(&["a.md"]);
        let raw = dir.path().join("raw");
        fs::set_permissions(&raw, fs::Permissions::from_mode(0o000)).unwrap();
        let readable = fs::read_dir(&raw).is_ok(); // root reads it anyway
        let got = count_raw(dir.path());
        fs::set_permissions(&raw, fs::Permissions::from_mode(0o755)).unwrap();
        if !readable {
            assert!(got.is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_file_that_vanished_is_skipped() {
        // A link whose target is gone stands for a file moved away between listing and reading.
        let dir = notes(&["a.md"]);
        std::os::unix::fs::symlink("/nonexistent/gone", dir.path().join("raw/b.md")).unwrap();
        assert_eq!(count(&dir), 1);
    }

    /// Issue 181: a write the database refuses while another connection holds the lock is not lost.
    fn locked(path: &Path) -> rusqlite::Connection {
        let lock = crate::db::open(path).unwrap();
        lock.execute_batch("BEGIN IMMEDIATE").unwrap();
        lock
    }

    #[test]
    fn a_refused_launch_leaves_the_close_unanswered() {
        let knowledge = notes(&five().iter().map(String::as_str).collect::<Vec<_>>());
        let db = tempfile::tempdir().unwrap();
        let path = db.path().join("state.db");
        let w = StateWriter::open(&path).unwrap();
        let mut c = Curator::new("org", knowledge.path().into(), TIMEOUT, "fake".into());
        w.append_event(&NewEvent {
            kind: "run_closed",
            ..Default::default()
        })
        .unwrap();
        let lock = locked(&path);
        c.pass(&w, &crate::base::stub::Stub::new()).unwrap();
        drop(lock);
        let launches = || -> i64 {
            w.conn()
                .query_row("SELECT count(*) FROM run_launch", [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(launches(), 0);
        c.next = Instant::now();
        c.pass(&w, &crate::base::stub::Stub::new()).unwrap();
        assert_eq!(launches(), 1);
    }

    #[test]
    fn a_refused_revoke_after_the_end_is_written_by_the_next_pass() {
        use crate::base::{LaunchSpec, stub::Stub};
        let knowledge = notes(&[]);
        let db = tempfile::tempdir().unwrap();
        let path = db.path().join("state.db");
        let w = StateWriter::open(&path).unwrap();
        let mut c = Curator::new("org", knowledge.path().into(), TIMEOUT, "fake".into());
        let base = Stub::new();
        let t = base
            .launch(&LaunchSpec {
                argv: vec!["x".into()],
                cwd: knowledge.path().into(),
                name: "c-s1".into(),
                tags: std::collections::BTreeMap::from([
                    (TAG_ROLE.to_string(), "curator".to_string()),
                    (TAG_ORG.to_string(), "org".to_string()),
                    (TAG_SPAWN.to_string(), "c-s1".to_string()),
                ]),
                env: Default::default(),
                unset: vec![],
            })
            .unwrap();
        base.exit(&t, 0);
        let lock = locked(&path);
        assert!(c.pass(&w, &base).is_err());
        drop(lock);
        assert!(base.list().unwrap().is_empty(), "the terminal was ended");
        assert_eq!(c.unsettled, vec![("c-s1".to_string(), false)]);
        c.next = Instant::now();
        c.pass(&w, &base).unwrap();
        assert!(c.unsettled.is_empty());
    }
}
