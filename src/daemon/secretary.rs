//! The secretary at `makiba up` (Issue 43, M6-2, carried from paddock's `org-start.sh`).
//!
//! `makiba up` starts `makibad` with `MAKIBA_SECRETARY=1`; `makibad` then decides once, after its start
//! pass, whether to launch one: the base is the truth for "already runs". A live terminal of this
//! organization named `secretary`, whose launch is not `failed`, means none is launched; a launch
//! of the secretary still under way is waited for. A finished one, or one whose launch was failed
//! (`makiba up` fails them when the base was stopped: a kept terminal is a shell after that), has
//! its keys revoked and is ended. Then one launch without a run is added (`launch::add_launch`,
//! Issue 166) in `<state>/secretary/`, which holds nothing: its rules and settings are passed from
//! outside it (spec sections 2 and 9).

use crate::base::{Base, Terminal};
use crate::db::Result;
use crate::db::writer::StateWriter;
use crate::spec::{self, TAG_ORG, TAG_SPAWN};
use rusqlite::OptionalExtension;
use std::path::{Path, PathBuf};

/// Set to `1` by `makiba up` on the `makibad` it starts, unless `--no-secretary`.
pub const SECRETARY_ENV: &str = "MAKIBA_SECRETARY";

const BRIEF: &str = "You are the secretary of this makiba organization: the one agent the person talks to. Your rules are appended to your system prompt. Wait for the person's first request.";

pub struct Secretary {
    org: String,
    folder: PathBuf,
    agent: String,
    decided: bool,
}

impl Secretary {
    /// For the organization of `state`, with the agent module `dispatch` uses.
    pub fn from_env(state: &Path) -> Secretary {
        let org = std::fs::canonicalize(state).unwrap_or_else(|_| state.to_path_buf());
        let agent = std::env::var(super::door::LAUNCH_AGENT_ENV)
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "claude_code".into());
        Secretary {
            org: org.to_string_lossy().into_owned(),
            folder: state.join("secretary"),
            agent,
            decided: false,
        }
    }

    /// Until it has decided: launches the secretary unless one runs.
    pub fn pass(&mut self, w: &StateWriter, base: &dyn Base) -> Result<()> {
        if self.decided {
            return Ok(());
        }
        let in_flight: bool = w.conn().query_row(
            "SELECT EXISTS (SELECT 1 FROM run_launch
                            WHERE role = 'secretary' AND stage NOT IN ('verified', 'failed'))",
            [],
            |r| r.get(0),
        )?;
        if in_flight {
            return Ok(());
        }
        let Ok(terminals) = base.list() else {
            return Ok(());
        };
        for t in terminals.iter().filter(|t| self.is_ours(t)) {
            let spawn = t.tags.get(TAG_SPAWN).cloned().unwrap_or_default();
            let failed = w
                .conn()
                .query_row(
                    "SELECT stage = 'failed' FROM run_launch WHERE spawn_id = ?1",
                    [&spawn],
                    |r| r.get::<_, bool>(0),
                )
                .optional()?
                .unwrap_or(false);
            if t.alive && !failed {
                self.decided = true;
                return Ok(());
            }
            // Keys first: a failed end is tried again by the next pass.
            w.transaction(|w| super::resume::revoke_keys(w, &spawn))?;
            if let Err(e) = base.end(&t.id) {
                eprintln!(
                    "makibad: cannot end the old secretary terminal {}: {e}",
                    t.id.0
                );
                return Ok(());
            }
        }
        if let Err(e) = super::launch::add_launch(
            w,
            spec::Role::Secretary,
            &self.folder,
            BRIEF,
            None,
            &self.agent,
        ) {
            eprintln!("makibad: cannot add the secretary launch: {e}");
            return Ok(());
        }
        self.decided = true;
        Ok(())
    }

    fn is_ours(&self, t: &Terminal) -> bool {
        t.name == spec::AGENT_SECRETARY && t.tags.get(TAG_ORG) == Some(&self.org)
    }
}
