//! Startup dialogs (Issue M3-5, spec section 7): a launch goes from `launched` to `dialog_passed`.
//!
//! While a launched terminal may take a key (`key_allowed`), its screen is read
//! (`Base::read` with no history) and `decide` asks the agent module's policy list whether a known
//! dialog is shown. One key per pass, and only on a screen that has stayed the same for `SETTLE`
//! since the last key: the cursor is moved toward the line to choose, the screen read again, and
//! Enter pressed only when the cursor is on that line and the option lines still match whole. A
//! dialog not on the list gets no key: the launch fails with `worker_spawn_failed` carrying the
//! screen. A terminal that has a status is past its dialogs: stage `dialog_passed`, no key; the one
//! exception is the status a dialog of the list is shown with (folder-trust: blocked, permission,
//! "trust this folder"; P10's second run), and then only that dialog is answered.
//!
//! The matcher is ported from claude-org-runtime 0.1.58 (`dispatcher/spawn_prompt.py`, `decide`
//! and its helpers); the press rule is paddock's (skills/org-escalation, inventory row L-N3).
//! Differences from the source: option lines are matched whole and the dialog's whole menu must
//! be the entry's option lines (the source matched by substring); a screen that is not understood
//! fails the launch once it is settled instead of waiting for a deadline; the end is the status,
//! not a visible composer.

use crate::agents::{self, DialogPolicy};
use crate::base::{Base, BlockKind, State, Status, Terminal};
use crate::db::Result;
use crate::db::writer::StateWriter;
use crate::spec::TAG_SPAWN;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// How long a screen must stay the same before a key or a failure (a first proposal). After every
/// key the time starts again, so a frame drawn before the key never confirms the next one.
pub const SETTLE: Duration = Duration::from_millis(500);

/// The cursor glyphs of a selection row: U+276F and `>`.
const CURSORS: [char; 2] = ['\u{276f}', '>'];
/// Footers of a selection dialog, spaceless and lower case. "esc to cancel" is not one: it is also
/// shown while the agent works (the source's note).
const FOOTERS: [&str; 3] = [
    "entertoconfirm",
    "entertoselect",
    "\u{2191}/\u{2193}tonavigate",
];

/// What the screen asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Nothing to press yet.
    Wait(&'static str),
    /// The one key for the dialog of the policy entry `dialog`.
    Key {
        dialog: &'static str,
        key: &'static str,
    },
    /// A dialog that is not on the list, or a screen that cannot be read safely: no key.
    Unknown(String),
}

/// The one rule for whether `makibad` may press a key for `dialog` (the entry the screen matches
/// whole) into a launched terminal: it is alive, and it has no status or the status `dialog` is
/// shown with (spec section 7; the owner's decision on Issue 28, 2026-11-19).
pub fn key_allowed(t: &Terminal, dialog: Option<&DialogPolicy>) -> bool {
    t.alive
        && match &t.status {
            None => true,
            Some(s) => dialog.is_some_and(|d| shown_with(d, s)),
        }
}

/// `s` is the status `d` is shown with: blocked, permission, and its message (normalized, whole).
fn shown_with(d: &DialogPolicy, s: &Status) -> bool {
    d.status.is_some_and(|m| {
        s.state == State::Blocked
            && s.kind == Some(BlockKind::Permission)
            && s.msg.as_deref().is_some_and(|x| norm(x) == norm(m))
    })
}

/// Whitespace collapsed (no-break spaces included) and lower case. (ponytail: the source also
/// applied NFKC; a compatibility form of an option text then does not match and nothing is
/// pressed.)
fn norm(line: &str) -> String {
    line.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// A normalized row without a surrounding box border.
fn unbox(n: &str) -> &str {
    n.trim_matches(|c| c == '\u{2502}' || c == '|' || c == ' ')
}

fn has_cursor(n: &str) -> bool {
    unbox(n).starts_with(CURSORS)
}

/// A normalized row without its border and one cursor glyph.
fn after_cursor(n: &str) -> &str {
    let s = unbox(n);
    s.strip_prefix(CURSORS).map_or(s, str::trim_start)
}

/// The rest of `s` after an `N.` or `N)` number, if it starts with one.
fn after_number(s: &str) -> Option<&str> {
    let digits = s.len() - s.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    (digits > 0 && matches!(s.as_bytes().get(digits), Some(b'.' | b')')))
        .then(|| s[digits + 1..].trim_start())
}

/// The option text of a normalized row, spaceless: border, one cursor glyph and an `N.` or `N)`
/// number taken off. Spaceless because a base's grid can drop the spaces between words.
fn label(n: &str) -> String {
    let s = after_cursor(n);
    after_number(s).unwrap_or(s).replace(' ', "")
}

/// A policy line as `label` gives a row.
fn key_of(option: &str) -> String {
    norm(option).replace(' ', "")
}

/// The next step for `screen`, from `policies` alone. Pure: whether the screen has settled is the
/// caller's.
pub fn decide(policies: &[DialogPolicy], screen: &[String]) -> Decision {
    let norm: Vec<String> = screen.iter().map(|l| norm(l)).collect();
    let labels: Vec<String> = norm.iter().map(|n| label(n)).collect();
    let blank = |i: usize| unbox(&norm[i]).is_empty();
    // The entries with at least one of their option lines on the screen, and those rows.
    let shown: Vec<(&DialogPolicy, Vec<usize>)> = policies
        .iter()
        .filter_map(|p| {
            let keys: Vec<String> = p.options.iter().map(|o| key_of(o)).collect();
            let rows: Vec<usize> = (0..labels.len())
                .filter(|&i| keys.contains(&labels[i]))
                .collect();
            (!rows.is_empty()).then_some((p, rows))
        })
        .collect();
    let (policy, rows) = match shown.as_slice() {
        [] => {
            let footer = norm
                .iter()
                .any(|n| FOOTERS.iter().any(|f| n.replace(' ', "").contains(f)));
            return if footer {
                Decision::Unknown("a selection dialog that is not on the policy list".into())
            } else {
                Decision::Wait("no dialog on the screen")
            };
        }
        [one] => one,
        more => {
            let names: Vec<&str> = more.iter().map(|(p, _)| p.name).collect();
            return Decision::Unknown(format!(
                "option lines of more than one dialog on the screen: {}",
                names.join(", ")
            ));
        }
    };
    // The menu is the block of rows around the first option line, up to blank rows. Rows above it
    // (output from before the dialog) are not looked at.
    let mut start = rows[0];
    while start > 0 && !blank(start - 1) {
        start -= 1;
    }
    let cursors: Vec<usize> = (start..norm.len())
        .filter(|&i| has_cursor(&norm[i]))
        .collect();
    let cursor = match cursors.as_slice() {
        [] => return Decision::Wait("the dialog shows no cursor yet"),
        [one] => *one,
        _ => return Decision::Unknown("more than one cursor row under the dialog".into()),
    };
    let mut end = start;
    while end + 1 < norm.len() && !blank(end + 1) {
        end += 1;
    }
    if !(start..=end).contains(&cursor) {
        return Decision::Wait("the cursor is not on the dialog's options yet");
    }
    // A numbered option further down, past a blank row, is part of the menu too (Codex review).
    // (ponytail: an unnumbered one there reads as text; folder-trust's options are unnumbered.)
    if (end + 1..norm.len()).any(|i| after_number(after_cursor(&norm[i])).is_some()) {
        return Decision::Unknown(format!(
            "an option line below the option lines of {}",
            policy.name
        ));
    }
    // The option lines still match whole: the menu is exactly the entry's lines, each once.
    let mut menu: Vec<&str> = labels[start..=end].iter().map(String::as_str).collect();
    let mut want: Vec<String> = policy.options.iter().map(|o| key_of(o)).collect();
    menu.sort_unstable();
    want.sort_unstable();
    if rows.iter().any(|r| !(start..=end).contains(r)) || menu != want {
        return Decision::Unknown(format!(
            "the option lines of {} do not match whole",
            policy.name
        ));
    }
    let choose = key_of(policy.choose);
    let Some(target) = (start..=end).find(|&i| labels[i] == choose) else {
        return Decision::Unknown(format!("{} has no line to choose", policy.name));
    };
    let key = match target.cmp(&cursor) {
        std::cmp::Ordering::Equal => "enter",
        std::cmp::Ordering::Greater => "down",
        std::cmp::Ordering::Less => "up",
    };
    Decision::Key {
        dialog: policy.name,
        key,
    }
}

/// A screen as compared between reads: normalized, trailing blank rows dropped.
fn settled_form(screen: &[String]) -> Vec<String> {
    let mut rows: Vec<String> = screen.iter().map(|l| norm(l)).collect();
    while rows.last().is_some_and(|r| r.is_empty()) {
        rows.pop();
    }
    rows
}

struct Row {
    id: i64,
    /// `None` for a launch without a run (Issue 166); `name` is then its role.
    task: Option<String>,
    name: String,
    spawn: String,
    agent: String,
}

#[derive(Default)]
pub struct Dialogs {
    /// Per spawn id: the screen last read since the last key, and since when it is the same.
    seen: BTreeMap<String, (Vec<String>, Instant)>,
}

impl Dialogs {
    pub fn new() -> Dialogs {
        Dialogs::default()
    }

    /// Whether `screen` has stayed the same for `SETTLE` since the last key to `spawn`.
    fn settled(&mut self, spawn: &str, screen: &[String]) -> bool {
        let form = settled_form(screen);
        match self.seen.get(spawn) {
            Some((was, since)) if *was == form => since.elapsed() >= SETTLE,
            _ => {
                self.seen.insert(spawn.to_string(), (form, Instant::now()));
                false
            }
        }
    }

    /// One pass over the launches at `launched`.
    pub fn pass(&mut self, w: &StateWriter, base: &dyn Base) -> Result<()> {
        let rows: Vec<Row> = {
            let mut st = w.conn().prepare(
                "SELECT l.id, r.task_id, COALESCE(r.task_id, l.role), l.spawn_id, l.agent
                 FROM run_launch l LEFT JOIN runs r ON r.id = l.run_id
                 WHERE l.stage = 'launched' ORDER BY l.id",
            )?;
            st.query_map([], |r| {
                Ok(Row {
                    id: r.get(0)?,
                    task: r.get(1)?,
                    name: r.get(2)?,
                    spawn: r.get(3)?,
                    agent: r.get(4)?,
                })
            })?
            .collect::<std::result::Result<_, _>>()?
        };
        self.seen
            .retain(|spawn, _| rows.iter().any(|r| &r.spawn == spawn));
        if rows.is_empty() {
            return Ok(());
        }
        // ponytail: one `list` per pass while a launch is at `launched`; a pass runs every tick.
        let Ok(terminals) = base.list() else {
            return Ok(());
        };
        for row in rows {
            let Some(t) = terminals
                .iter()
                .find(|t| t.tags.get(TAG_SPAWN) == Some(&row.spawn))
            else {
                let why = format!("no terminal carries {TAG_SPAWN}={}", row.spawn);
                fail(w, base, &row, &why, &[])?;
                continue;
            };
            if !t.alive {
                let screen = base.read(&t.id, 0).unwrap_or_default();
                fail(w, base, &row, "the terminal ended at startup", &screen)?;
                continue;
            }
            let policies = agents::by_name(&row.agent)
                .map(|a| a.startup_dialogs().to_vec())
                .unwrap_or_default();
            if let Some(s) = &t.status
                && !policies.iter().any(|d| shown_with(d, s))
            {
                if passed(w, row.id)? {
                    super::crash::point("dialog_passed");
                } else {
                    // Failed meanwhile by somebody else (Issue 163): nothing runs for it.
                    let _ = super::resume::end_terminals(base, &row.spawn);
                }
                continue;
            }
            let Ok(screen) = base.read(&t.id, 0) else {
                continue;
            };
            let decision = decide(&policies, &screen);
            // Every read counts toward settling, a waiting one too: a redraw starts the time again.
            let settled = self.settled(&row.spawn, &screen);
            if matches!(decision, Decision::Wait(_)) || !settled {
                continue;
            }
            match decision {
                Decision::Wait(_) => {}
                Decision::Unknown(why) => fail(w, base, &row, &why, &screen)?,
                Decision::Key { dialog, key } => {
                    // The rule for the dialog shown (a status shown with another dialog: wait),
                    // and again on a fresh look: the status may have changed since the list.
                    // (ponytail: one that arrives while the base waits for quiet before pressing
                    // is not seen; the base has no conditional key.)
                    let d = policies.iter().find(|d| d.name == dialog);
                    if key_allowed(t, d) && base.get(&t.id).is_ok_and(|now| key_allowed(&now, d)) {
                        self.seen.remove(&row.spawn);
                        if let Err(e) = base.keys(&t.id, &[key]) {
                            eprintln!("makibad: dialog key for {}: {e}", row.name);
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

/// Stage `dialog_passed`, only over `launched` (Issue 163): `false` when the launch was moved.
fn passed(w: &StateWriter, id: i64) -> Result<bool> {
    super::crash::hold("dialog_passed");
    let n = w.conn().execute(
        "UPDATE run_launch SET stage = 'dialog_passed' WHERE id = ?1 AND stage = 'launched'",
        [id],
    )?;
    Ok(n > 0)
}

/// Stage `failed` and `worker_spawn_failed`, together (spec section 10), with the run and the
/// notice of M3-6. The terminal is left, unless the launch was failed meanwhile by somebody else.
fn fail(w: &StateWriter, base: &dyn Base, row: &Row, why: &str, screen: &[String]) -> Result<()> {
    let task = row.task.as_deref();
    if !super::verify::fail(w, row.id, task, &row.spawn, "launched", why, screen)? {
        let _ = super::resume::end_terminals(base, &row.spawn);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::claude_code::DIALOGS;

    fn screen(rows: &[&str]) -> Vec<String> {
        rows.iter().map(|r| r.to_string()).collect()
    }

    const CUR: &str = "\u{276f}";

    /// The folder-trust frame of P10, cursor on line `at` (0 or 1).
    fn trust(at: usize, no: &str, yes: &str) -> Vec<String> {
        let mark = |i: usize| if i == at { CUR } else { " " };
        vec![
            "\u{2500}".repeat(40),
            " Accessing workspace:".into(),
            String::new(),
            " /work/repo".into(),
            String::new(),
            " Quick safety check: Is this a project you created or one you trust?".into(),
            String::new(),
            " Security guide".into(),
            String::new(),
            format!(" {} {no}", mark(0)),
            format!(" {} {yes}", mark(1)),
            String::new(),
            " Enter to confirm \u{b7} Esc to cancel".into(),
        ]
    }

    fn channel(at: usize, one: &str, two: &str) -> Vec<String> {
        let mark = |i: usize| if i == at { CUR } else { " " };
        vec![
            "  WARNING: Loading development channels".into(),
            String::new(),
            "  Channels: server:makiba-channel".into(),
            String::new(),
            format!("  {} 1. {one}", mark(0)),
            format!("  {} 2. {two}", mark(1)),
            String::new(),
            "  Enter to confirm \u{b7} Esc to cancel".into(),
        ]
    }

    fn key(dialog: &'static str, key: &'static str) -> Decision {
        Decision::Key { dialog, key }
    }

    #[test]
    fn every_entry_chooses_one_of_its_own_lines() {
        for p in &DIALOGS {
            assert!(p.options.contains(&p.choose), "{}", p.name);
        }
        let names: Vec<&str> = DIALOGS.iter().map(|p| p.name).collect();
        assert_eq!(names, ["folder-trust", "dev-channel"]);
    }

    /// Source cases 12 (move toward the line, then Enter on it) on the P10 frames, numbered or not.
    #[test]
    fn the_cursor_is_moved_toward_the_line_to_choose_then_enter() {
        let (no, yes) = ("No, exit", "Yes, I trust this folder");
        assert_eq!(
            decide(&DIALOGS, &trust(0, no, yes)),
            key("folder-trust", "down")
        );
        assert_eq!(
            decide(&DIALOGS, &trust(1, no, yes)),
            key("folder-trust", "enter")
        );
        // The other order: up.
        assert_eq!(
            decide(&DIALOGS, &trust(1, yes, no)),
            key("folder-trust", "up")
        );
        let (one, two) = ("I am using this for local development", "Exit");
        assert_eq!(
            decide(&DIALOGS, &channel(0, one, two)),
            key("dev-channel", "enter")
        );
        assert_eq!(
            decide(&DIALOGS, &channel(1, one, two)),
            key("dev-channel", "up")
        );
    }

    /// Source case 6, changed: spaces dropped by the grid and case do not matter; a no-break space
    /// after the cursor is a space; a box border is not part of the line.
    #[test]
    fn lines_are_compared_spaceless_and_without_a_border() {
        let s = screen(&[
            "\u{2502} \u{276f}\u{a0}NO,EXIT \u{2502}",
            "\u{2502}   Yes,Itrustthisfolder \u{2502}",
        ]);
        assert_eq!(decide(&DIALOGS, &s), key("folder-trust", "down"));
    }

    /// Case 13: one option line reworded, one added or one missing is not the dialog on the list,
    /// wherever the cursor is.
    #[test]
    fn a_menu_that_is_not_the_entrys_lines_whole_is_unknown() {
        let unknown = |s: &[String]| matches!(decide(&DIALOGS, s), Decision::Unknown(_));
        let yes = "Yes, I trust this folder";
        assert!(
            unknown(&trust(1, "No, leave", yes)),
            "reworded, cursor on the line to choose"
        );
        assert!(unknown(&trust(0, "No, leave", yes)));
        assert!(unknown(&trust(
            0,
            "No, exit",
            "Yes, I trust this folder and its parents"
        )));
        let one = "I am using this for local development";
        assert!(unknown(&channel(0, one, "Quit")));
        // A substring is not the line (the source matched by substring).
        assert!(unknown(&channel(0, &format!("{one} only"), "Exit")));
        // An extra option before, between or after the entry's lines.
        for extra in [0, 1, 2] {
            let mut lines = vec!["   No, exit".to_string(), format!(" {CUR} {yes}")];
            lines.insert(extra, "   Allow everything".into());
            assert!(unknown(&lines), "extra option at {extra}");
        }
        // A numbered option past a blank row below the entry's lines (Codex review).
        let mut lines = channel(0, one, "Exit");
        let at = lines.len() - 2;
        lines.splice(at..at, ["".to_string(), "    3. Allow everything".into()]);
        assert!(unknown(&lines), "{lines:?}");
        // A missing line.
        assert!(unknown(&screen(&[
            " \u{276f} No, exit",
            "",
            " Enter to confirm"
        ])));
    }

    /// Cases 7, 8, 9: two dialogs, a line shown twice, two cursors.
    #[test]
    fn an_ambiguous_screen_is_unknown() {
        let unknown = |s: &[String]| matches!(decide(&DIALOGS, s), Decision::Unknown(_));
        let (no, yes) = ("No, exit", "Yes, I trust this folder");
        let mut both = trust(1, no, yes);
        both.extend(channel(0, "I am using this for local development", "Exit"));
        assert!(unknown(&both));
        let mut twice = trust(1, no, yes);
        twice.extend(trust(1, no, yes));
        assert!(unknown(&twice));
        let mut cursors = trust(1, no, yes);
        cursors[9] = format!(" {CUR} {no}");
        assert!(unknown(&cursors));
        // A cursor row above the dialog (a shell prompt) is not looked at.
        let mut above = vec![format!("{CUR} claude"), String::new()];
        above.extend(trust(0, no, yes));
        assert_eq!(decide(&DIALOGS, &above), key("folder-trust", "down"));
    }

    /// Cases 10, 14, 16: no cursor yet, an unknown selection dialog, nothing at all.
    #[test]
    fn no_dialog_waits_and_an_unknown_selection_dialog_is_unknown() {
        let mut nocursor = trust(0, "No, exit", "Yes, I trust this folder");
        nocursor[9] = "   No, exit".into();
        assert!(matches!(decide(&DIALOGS, &nocursor), Decision::Wait(_)));
        let other = screen(&[
            "Pick a theme",
            " \u{276f} 1. Dark",
            "   2. Light",
            "",
            "Enter to confirm",
        ]);
        assert!(matches!(decide(&DIALOGS, &other), Decision::Unknown(_)));
        let arrows = screen(&["Pick one", "\u{2191}/\u{2193} to navigate"]);
        assert!(matches!(decide(&DIALOGS, &arrows), Decision::Unknown(_)));
        // The composer of Claude Code 2.1.326 is a cursor row too: not a dialog.
        let composer = screen(&[
            "\u{2500}".repeat(40).as_str(),
            "\u{276f} ",
            "\u{2500}".repeat(40).as_str(),
        ]);
        assert!(matches!(decide(&DIALOGS, &composer), Decision::Wait(_)));
        assert!(matches!(decide(&DIALOGS, &[]), Decision::Wait(_)));
        // No policy list: nothing is known.
        let s = trust(1, "No, exit", "Yes, I trust this folder");
        assert!(matches!(decide(&[], &s), Decision::Unknown(_)));
    }

    /// One terminal showing `screen`. `list` reports no status; `get` reports one when `late`
    /// (a status that came between the list and the key). Keys are recorded.
    struct Screen {
        screen: std::sync::Mutex<Vec<String>>,
        late: bool,
        keys: std::sync::Mutex<Vec<String>>,
    }

    impl Screen {
        fn terminal(&self, status: bool) -> Terminal {
            Terminal {
                id: crate::base::TerminalRef("w-t1".into()),
                name: "w-t1".into(),
                tags: [(TAG_SPAWN.to_string(), "s1".to_string())].into(),
                status: status.then_some(crate::base::Status {
                    state: crate::base::State::Idle,
                    kind: None,
                    msg: None,
                }),
                turn: None,
                alive: true,
                exit_code: None,
            }
        }
    }

    impl Base for Screen {
        fn name(&self) -> &'static str {
            "screen"
        }
        fn hello(&self) -> std::result::Result<crate::base::BaseInfo, crate::base::BaseError> {
            unreachable!()
        }
        fn launch(
            &self,
            _: &crate::base::LaunchSpec,
        ) -> std::result::Result<crate::base::TerminalRef, crate::base::BaseError> {
            unreachable!()
        }
        fn list(&self) -> std::result::Result<Vec<Terminal>, crate::base::BaseError> {
            Ok(vec![self.terminal(false)])
        }
        fn get(
            &self,
            _: &crate::base::TerminalRef,
        ) -> std::result::Result<Terminal, crate::base::BaseError> {
            Ok(self.terminal(self.late))
        }
        fn read(
            &self,
            _: &crate::base::TerminalRef,
            _: usize,
        ) -> std::result::Result<Vec<String>, crate::base::BaseError> {
            Ok(self.screen.lock().unwrap().clone())
        }
        fn keys(
            &self,
            _: &crate::base::TerminalRef,
            keys: &[&str],
        ) -> std::result::Result<(), crate::base::BaseError> {
            let mut k = self.keys.lock().unwrap();
            k.extend(keys.iter().map(|s| s.to_string()));
            Ok(())
        }
        fn send(
            &self,
            _: &crate::base::TerminalRef,
            _: &str,
            _: bool,
        ) -> std::result::Result<(), crate::base::BaseError> {
            unreachable!()
        }
        fn end(
            &self,
            _: &crate::base::TerminalRef,
        ) -> std::result::Result<(), crate::base::BaseError> {
            unreachable!()
        }
        fn events(&self) -> std::result::Result<crate::base::EventStream, crate::base::BaseError> {
            unreachable!()
        }
        fn start(&self) -> std::result::Result<(), crate::base::BaseError> {
            unreachable!()
        }
        fn worker_denies(&self) -> crate::base::BaseDenies {
            unreachable!()
        }
    }

    /// One pass per screen over a launch at `launched` whose terminal shows that screen, each
    /// `SETTLE` after the one before; the keys pressed.
    fn passes(screens: &[Vec<String>], late: bool) -> Vec<String> {
        let dir = tempfile::tempdir().unwrap();
        let w = StateWriter::open(&dir.path().join("state.db")).unwrap();
        w.upsert_run(&crate::db::writer::RunUpsert {
            task_id: "t1",
            project_slug: "p",
            status: Some("queued"),
            ..Default::default()
        })
        .unwrap();
        w.conn()
            .execute(
                "INSERT INTO run_launch (run_id, spawn_id, agent, brief, worktree, stage)
                 VALUES (1, 's1', 'fake', 'b', '/w', 'launched')",
                [],
            )
            .unwrap();
        let base = Screen {
            screen: Default::default(),
            late,
            keys: Default::default(),
        };
        let mut d = Dialogs::new();
        for screen in screens {
            if let Some(seen) = d.seen.get_mut("s1") {
                seen.1 -= SETTLE;
            }
            *base.screen.lock().unwrap() = screen.clone();
            d.pass(&w, &base).unwrap();
        }
        base.keys.into_inner().unwrap()
    }

    /// The rule is asked again right before the key: a status seen then means no key.
    #[test]
    fn a_status_that_comes_before_the_key_stops_it() {
        let s = trust(1, "No, exit", "Yes, I trust this folder");
        assert_eq!(passes(&[s.clone(), s.clone()], false), ["enter"]);
        assert!(passes(&[s.clone(), s], true).is_empty());
    }

    /// A read that shows no dialog starts the settling time again: the dialog that comes back
    /// is not answered on the strength of the read before it went (Codex review).
    #[test]
    fn a_screen_without_the_dialog_between_two_reads_of_it_starts_the_time_again() {
        let s = trust(1, "No, exit", "Yes, I trust this folder");
        assert!(passes(&[s.clone(), vec![], s.clone()], false).is_empty());
        assert_eq!(passes(&[s.clone(), vec![], s.clone(), s], false), ["enter"]);
    }

    /// The owner's rule: no status, or the status the dialog itself is shown with, whole.
    #[test]
    fn a_key_is_allowed_with_no_status_or_with_the_dialogs_own_status_only() {
        let [trust_d, channel_d] = &DIALOGS;
        let t = |state, kind, msg: Option<&str>| Terminal {
            id: crate::base::TerminalRef("w".into()),
            name: "w".into(),
            tags: Default::default(),
            status: Some(Status {
                state,
                kind,
                msg: msg.map(String::from),
            }),
            turn: None,
            alive: true,
            exit_code: None,
        };
        let shown = t(
            State::Blocked,
            Some(BlockKind::Permission),
            Some("Trust  this folder"),
        );
        assert!(key_allowed(&shown, Some(trust_d)));
        assert!(!key_allowed(&shown, Some(channel_d)), "another dialog");
        assert!(!key_allowed(&shown, None), "no dialog matched whole");
        let other = [
            t(
                State::Blocked,
                Some(BlockKind::Permission),
                Some("allow Bash?"),
            ),
            t(
                State::Blocked,
                Some(BlockKind::Permission),
                Some("trust this folder?"),
            ),
            t(State::Blocked, Some(BlockKind::Permission), None),
            t(
                State::Blocked,
                Some(BlockKind::Question),
                Some("trust this folder"),
            ),
            t(State::Idle, None, Some("trust this folder")),
            t(State::Working, None, None),
        ];
        for o in &other {
            assert!(!key_allowed(o, Some(trust_d)), "{:?}", o.status);
        }
        let none = Terminal {
            status: None,
            ..shown.clone()
        };
        assert!(key_allowed(&none, Some(channel_d)) && key_allowed(&none, None));
        let ended = Terminal {
            alive: false,
            ..none
        };
        assert!(!key_allowed(&ended, Some(trust_d)));
    }

    /// Case 11: a key only on a screen unchanged for `SETTLE`, counted again after every key.
    #[test]
    fn a_screen_settles_only_when_it_stays_the_same() {
        let mut d = Dialogs::new();
        let a = screen(&["a", ""]);
        assert!(!d.settled("s", &a));
        assert!(!d.settled("s", &screen(&["a"])), "too soon");
        d.seen.get_mut("s").unwrap().1 -= SETTLE;
        assert!(
            d.settled("s", &screen(&["a", "", ""])),
            "trailing blank rows do not count"
        );
        assert!(!d.settled("s", &screen(&["b"])), "a change starts again");
    }
}
