//! Calling the person back (Issue 272, docs/design/person-call.md): the one owner of telling the
//! person that something waits for them.
//!
//! What waits for the person: every `pending` entry `escalated` (by the secretary, or a stop
//! `makibad` wrote), and every entry of kind `merge` still `open` (it waits for the person's key).
//! An `open` question is the secretary's, so a worker never reaches the person through here: the
//! call stays the secretary's, and the contact stays one.
//!
//! The channel is a command of the person's: the organization setting `notify_command`, the
//! absolute path of an executable, run with no shell as `<path> <title>` in a process group of its
//! own, the body on stdin from a file, stderr into a file, stdout to /dev/null. A path an agent
//! could write is refused at the set and again before every call (`check`), since `makibad` runs
//! outside every sandbox.
//!
//! The ledger is the events: `notify_sent` and `notify_failed` with `to` `person` (outside the
//! delivery ledger: the person has no box). Whether an entry was called is read from them each
//! tick, never kept as a latch. An entry is called at most once on success; a failure counts only
//! against the setting it was made with (events after the setting's newest `config_set`), three
//! at most, `RETRY` apart.
//!
//! In memory beside the ledger: the one call in flight (or its result not yet written) and the
//! start of the last call (`GAP`). The process is polled, never waited on, so a command cannot
//! hold the tick; at `TIMEOUT` its whole group is killed. A call not recorded when `makibad`
//! stops is made again after the restart (at least once).

use crate::db::writer::NewEvent;
use crate::db::{Result, StateWriter};
use crate::status_local::Mask;
use rusqlite::Connection;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// The setting's key (`config`, the organization's).
pub const KEY: &str = "notify_command";
/// The recipient written in the events.
pub const TO: &str = "person";
/// The last line of every body.
pub const LAST_LINE: &str = "Answer in the secretary's conversation, or see makiba status.";
/// Tries per entry under one setting.
const TRIES: usize = 3;
/// Entry lines in one body; the rest are counted.
const LINES: usize = 5;
/// Characters of a question's first line kept.
const CUT: usize = 120;
/// Bytes of stderr kept, after masking.
const STDERR_KEEP: usize = 500;
/// Bytes of stderr read.
const STDERR_READ: u64 = 64 * 1024;
/// The largest file the command (and what it starts) may write, its stderr file included:
/// `RLIMIT_FSIZE`, so a noisy command cannot fill the state folder's file system. A write past it
/// fails, and the default action of `SIGXFSZ` ends the writer.
const FILE_LIMIT: u64 = 1024 * 1024;
/// The longest value of the setting.
const VALUE_MAX: usize = 4096;

/// Milliseconds from `var`, else `default`.
fn ms(var: &str, default: u64) -> Duration {
    Duration::from_millis(
        std::env::var(var)
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(default),
    )
}

/// Rule 2 of design section 2.1: the text of a value, before anything is looked at on disk.
fn text_rule(value: &str) -> std::result::Result<(), String> {
    if !value.starts_with('/') {
        return Err(format!("{KEY} is an absolute path (it starts with /)"));
    }
    if value.contains(['\0', '\n', '\r']) {
        return Err(format!("{KEY} holds no NUL, newline or carriage return"));
    }
    if value.len() > VALUE_MAX {
        return Err(format!("{KEY} is at most {VALUE_MAX} bytes"));
    }
    Ok(())
}

/// `path` resolved, when it exists.
fn resolved(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// `~/x` with `HOME`; any other text as it is.
fn expand_home(entry: &str) -> PathBuf {
    match (entry.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(entry),
    }
}

/// Every place an agent's sandbox may write, as `makibad` wrote it: the `sandbox.filesystem.
/// allowWrite` entries of every settings file in `<state>/settings` (one per launch; those of
/// closed launches are kept too, which only refuses more), each resolved, so a workplace that is
/// a link is seen at its target. Fail closed: a settings file that cannot be read or parsed, or an
/// entry that is not an absolute path, refuses every command.
fn sandbox_places(state: &Path) -> std::result::Result<Vec<PathBuf>, String> {
    let dir = state.join("settings");
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(format!(
                "the launches' settings ({}) cannot be read: {e}",
                dir.display()
            ));
        }
    };
    let mut out = Vec::new();
    for entry in entries {
        let file = entry
            .map_err(|e| {
                format!(
                    "the launches' settings ({}) cannot be read: {e}",
                    dir.display()
                )
            })?
            .path();
        if file.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let v: Value = std::fs::read_to_string(&file)
            .map_err(|e| e.to_string())
            .and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))
            .map_err(|e| format!("the settings {} cannot be read: {e}", file.display()))?;
        let Some(list) = v.pointer("/sandbox/filesystem/allowWrite") else {
            continue;
        };
        let list = list
            .as_array()
            .ok_or_else(|| format!("the settings {}: allowWrite is not a list", file.display()))?;
        for item in list {
            let p = item
                .as_str()
                .map(expand_home)
                .filter(|p| p.is_absolute())
                .ok_or_else(|| {
                    format!(
                        "the settings {}: allowWrite entry {item} is not an absolute path",
                        file.display()
                    )
                })?;
            out.push(resolved(&p));
        }
    }
    Ok(out)
}

/// The workplace of every run not closed, resolved.
fn open_worktrees(c: &Connection) -> std::result::Result<Vec<PathBuf>, String> {
    let mut q = c
        .prepare(
            "SELECT DISTINCT l.worktree FROM run_launch l JOIN runs r ON r.id = l.run_id
             WHERE r.status NOT IN ('completed', 'failed', 'abandoned') AND l.worktree IS NOT NULL",
        )
        .map_err(|e| format!("the open runs cannot be read: {e}"))?;
    let rows = q
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| format!("the open runs cannot be read: {e}"))?;
    rows.map(|r| {
        r.map(|p| resolved(Path::new(&p)))
            .map_err(|e| format!("the open runs cannot be read: {e}"))
    })
    .collect()
}

/// The places an agent writes, resolved: the roots of spec section 2, the system's temporary
/// folders, every `allowWrite` entry `makibad` wrote into a launch's sandbox, and the workplace of
/// every open run.
fn agent_places(
    c: &Connection,
    state: &Path,
) -> std::result::Result<Vec<(PathBuf, &'static str)>, String> {
    let p = super::prepare::Places::from_env(state);
    // The places read from the launches first: the refusal then names the narrowest place.
    let mut out: Vec<(PathBuf, &'static str)> = sandbox_places(state)?
        .into_iter()
        .map(|s| (s, "a place a launch's sandbox may write"))
        .collect();
    out.extend(
        open_worktrees(c)?
            .into_iter()
            .map(|t| (t, "the workplace of an open run")),
    );
    out.extend([
        (resolved(state), "the state folder"),
        (resolved(&p.workers), "the worktrees"),
        (resolved(&p.docs), "the document folders"),
        (resolved(&p.repos), "the repositories"),
        (
            resolved(&super::curator::knowledge_dir()),
            "the knowledge folder",
        ),
    ]);
    for t in [
        PathBuf::from("/tmp"),
        PathBuf::from("/private/tmp"),
        PathBuf::from("/var/tmp"),
        std::env::temp_dir(),
    ] {
        out.push((resolved(&t), "a temporary folder"));
    }
    Ok(out)
}

/// Rules 3 to 5 of design section 2.1, on a value that passed rule 2: the resolved file exists,
/// is a regular file, is under no place an agent writes, and it and every folder above it are
/// the user's or root's and writable by nobody else (a sticky folder excepted).
fn disk_rule(c: &Connection, value: &str, state: &Path) -> std::result::Result<PathBuf, String> {
    let file =
        std::fs::canonicalize(value).map_err(|e| format!("{value} cannot be resolved: {e}"))?;
    let meta = std::fs::metadata(&file).map_err(|e| format!("{}: {e}", file.display()))?;
    if !meta.is_file() {
        return Err(format!("{} is not a regular file", file.display()));
    }
    for (place, what) in agent_places(c, state)? {
        if !place.as_os_str().is_empty() && file.starts_with(&place) {
            return Err(format!(
                "{} is under {what} ({}), which an agent may write",
                file.display(),
                place.display()
            ));
        }
    }
    // SAFETY: geteuid has no preconditions.
    let uid = unsafe { libc::geteuid() };
    let mut at = Some(file.as_path());
    while let Some(p) = at {
        let m = std::fs::metadata(p).map_err(|e| format!("{}: {e}", p.display()))?;
        if m.uid() != uid && m.uid() != 0 {
            return Err(format!(
                "{} is owned by another user (uid {})",
                p.display(),
                m.uid()
            ));
        }
        let mode = m.permissions().mode();
        let sticky_dir = m.is_dir() && mode & 0o1000 != 0;
        if mode & 0o022 != 0 && !sticky_dir {
            return Err(format!(
                "{} is writable by group or others (mode {:o})",
                p.display(),
                mode & 0o7777
            ));
        }
        at = p.parent();
    }
    Ok(file)
}

/// The rule of a set: empty is off; anything else passes every rule of design section 2.1.
pub fn check_set(c: &Connection, value: &str, state: &Path) -> std::result::Result<(), String> {
    if value.is_empty() {
        return Ok(());
    }
    text_rule(value)?;
    disk_rule(c, value, state).map(|_| ())
}

/// Every control character but newline as a space.
fn plain(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() && c != '\n' { ' ' } else { c })
        .collect()
}

/// `s` cut to at most `n` bytes, at a character boundary.
fn cut_bytes(s: &str, n: usize) -> &str {
    if s.len() <= n {
        return s;
    }
    let mut end = n;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// One entry that waits for the person.
struct Waiting {
    id: i64,
    merge: bool,
    question: String,
}

/// The line of an entry: masked whole, control characters replaced, its first line, then cut.
fn entry_line(mask: &Mask, e: &Waiting) -> String {
    if e.merge {
        return format!(
            "#{} a merge waits for your key: makiba merge approve {}",
            e.id, e.id
        );
    }
    let text = plain(&mask.text(&e.question));
    let first = text.lines().next().unwrap_or_default();
    let shown = if first.chars().count() > CUT {
        format!("{}...", first.chars().take(CUT).collect::<String>())
    } else {
        first.to_string()
    };
    format!("#{} {shown}", e.id)
}

/// The title and the body of a call over `entries` (oldest first).
fn texts(mask: &Mask, entries: &[Waiting]) -> (String, String) {
    let title = format!("makiba secretary: {} waiting for you", entries.len());
    let mut body = String::new();
    for e in entries.iter().take(LINES) {
        body.push_str(&entry_line(mask, e));
        body.push('\n');
    }
    if entries.len() > LINES {
        body.push_str(&format!("and {} more\n", entries.len() - LINES));
    }
    body.push_str(LAST_LINE);
    body.push('\n');
    (mask.text(&title), body)
}

/// The setting as stored: its value and the id of its `config_set`; `None` when never set.
fn setting(w: &StateWriter) -> Result<Option<(String, i64)>> {
    use rusqlite::OptionalExtension;
    Ok(w.conn()
        .query_row(
            "SELECT COALESCE(json_extract(payload_json, '$.value'), ''), id FROM events
             WHERE kind = 'config_set' AND json_extract(payload_json, '$.project') = ''
               AND json_extract(payload_json, '$.key') = ?1
             ORDER BY id DESC LIMIT 1",
            [KEY],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?)
}

/// What the events say of one entry under the setting `setting` (the id of its `config_set`):
/// called, failures made under that setting (their payload's `setting`), and whether the newest
/// of those is younger than `retry`. A call that started under an older setting and failed after
/// the person set the command again does not count against the new one.
#[derive(Default)]
struct Record {
    called: bool,
    failures: usize,
    recent: bool,
}

fn records(w: &StateWriter, setting: i64, retry: Duration) -> Result<HashMap<i64, Record>> {
    let mut out: HashMap<i64, Record> = HashMap::new();
    let mut q = w.conn().prepare(
        "SELECT kind, payload_json,
                (julianday('now') - julianday(occurred_at)) * 86400000.0
         FROM events WHERE kind IN ('notify_sent', 'notify_failed')
           AND json_extract(payload_json, '$.to') = ?1",
    )?;
    let rows = q.query_map([TO], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, f64>(2)?,
        ))
    })?;
    for row in rows {
        let (kind, payload, age_ms) = row?;
        let v: Value = serde_json::from_str(&payload).unwrap_or(Value::Null);
        let ids = v["pending_ids"].as_array().cloned().unwrap_or_default();
        for p in ids.iter().filter_map(Value::as_i64) {
            let r = out.entry(p).or_default();
            if kind == "notify_sent" {
                r.called = true;
            } else if v["setting"].as_i64() == Some(setting) {
                r.failures += 1;
                if age_ms < retry.as_millis() as f64 {
                    r.recent = true;
                }
            }
        }
    }
    Ok(out)
}

/// The entries that wait for the person, oldest first.
fn waiting(w: &StateWriter) -> Result<Vec<Waiting>> {
    let mut q = w.conn().prepare(
        "SELECT id, kind, question FROM pending
         WHERE status = 'escalated' OR (kind = 'merge' AND status = 'open') ORDER BY id",
    )?;
    let rows = q.query_map([], |r| {
        Ok(Waiting {
            id: r.get(0)?,
            merge: r.get::<_, String>(1)? == "merge",
            question: r.get(2)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The entries due under the setting `since`: waiting for the person, never called, fewer than
/// `TRIES` failures under it, the newest of them at least `retry` old.
fn due(w: &StateWriter, since: i64, retry: Duration) -> Result<Vec<Waiting>> {
    let records = records(w, since, retry)?;
    Ok(waiting(w)?
        .into_iter()
        .filter(|e| {
            records
                .get(&e.id)
                .is_none_or(|r| !r.called && r.failures < TRIES && !r.recent)
        })
        .collect())
}

/// A call whose process runs.
struct Flight {
    child: Child,
    pgid: i32,
    ids: Vec<i64>,
    command: String,
    /// The id of the `config_set` the call was started under.
    setting: i64,
    title: String,
    body: String,
    started: Instant,
    files: [PathBuf; 2],
}

/// A finished call whose event is not yet written.
struct Done {
    kind: &'static str,
    payload: Value,
    files: Vec<PathBuf>,
}

/// The call pass of `makibad`'s loop.
pub struct Call {
    state: PathBuf,
    gap: Duration,
    retry: Duration,
    timeout: Duration,
    flight: Option<Flight>,
    done: Option<Done>,
    last_start: Option<Instant>,
    seq: u64,
}

impl Call {
    /// The test seams are read here, when `makibad` starts.
    pub fn new(state: &Path) -> Call {
        Call {
            state: state.to_path_buf(),
            gap: ms("MAKIBA_CALL_GAP_MS", 30_000),
            retry: ms("MAKIBA_CALL_RETRY_MS", 60_000),
            timeout: ms("MAKIBA_CALL_TIMEOUT_MS", 10_000),
            flight: None,
            done: None,
            last_start: None,
            seq: 0,
        }
    }

    /// One tick: write a finished call, poll the one in flight, or start one.
    pub fn pass(&mut self, w: &StateWriter) -> Result<()> {
        self.write(w)?;
        if self.flight.is_some() {
            self.poll(w)?;
            return self.write(w);
        }
        let Some((command, since)) = setting(w)? else {
            return Ok(());
        };
        if command.is_empty() {
            return Ok(());
        }
        if self.last_start.is_some_and(|t| t.elapsed() < self.gap) {
            return Ok(());
        }
        let first = due(w, since, self.retry)?;
        if first.is_empty() {
            return Ok(());
        }
        self.last_start = Some(Instant::now());
        // The resolved file is what runs: a link checked here cannot be pointed elsewhere
        // between the check and the start.
        let program = match text_rule(&command)
            .and_then(|()| disk_rule(w.conn(), &command, &self.state))
        {
            Ok(p) => p,
            Err(why) => {
                let mask = Mask::new(w.conn());
                let ids: Vec<i64> = first.iter().map(|e| e.id).collect();
                self.done = Some(Done {
                    kind: "notify_failed",
                    payload: json!({"to": TO, "pending_ids": ids, "command": command,
                                    "setting": since,
                                    "reason": format!("unsafe: {}", mask.text(&why)), "stderr": ""}),
                    files: Vec::new(),
                });
                return self.write(w);
            }
        };
        // The setting and the entries are read again, and the process started, inside one write
        // transaction: an answer, a withdrawal or a new setting that the door commits meanwhile
        // is either seen here or waits until the process has started.
        let started = w.transaction(|w| {
            if setting(w)?.as_ref().map(|(c, id)| (c.as_str(), *id)) != Some((&command, since)) {
                return Ok(None);
            }
            let entries = due(w, since, self.retry)?;
            if entries.is_empty() {
                return Ok(None);
            }
            let mask = Mask::new(w.conn());
            let ids: Vec<i64> = entries.iter().map(|e| e.id).collect();
            let (title, body) = texts(&mask, &entries);
            let spawned = self.spawn(&program, &title, &body);
            Ok(Some((ids, title, body, spawned)))
        })?;
        let Some((ids, title, body, spawned)) = started else {
            return Ok(());
        };
        match spawned {
            Ok((child, files)) => {
                let pgid = child.id() as i32;
                self.flight = Some(Flight {
                    child,
                    pgid,
                    ids,
                    command,
                    setting: since,
                    title,
                    body,
                    started: Instant::now(),
                    files,
                });
                Ok(())
            }
            Err((e, files)) => {
                self.done = Some(Done {
                    kind: "notify_failed",
                    payload: json!({"to": TO, "pending_ids": ids, "command": command,
                                    "setting": since,
                                    "reason": format!("spawn: {e}"), "stderr": ""}),
                    files,
                });
                self.write(w)
            }
        }
    }

    /// The call's two files, in `<state>/tmp/call/` (mode 0700), and its process started.
    #[allow(clippy::type_complexity)]
    fn spawn(
        &mut self,
        program: &Path,
        title: &str,
        body: &str,
    ) -> std::result::Result<(Child, [PathBuf; 2]), (String, Vec<PathBuf>)> {
        let folder = super::prepare::tmp_root(&self.state.join("settings")).join("call");
        let fail = |e: std::io::Error, files: Vec<PathBuf>| (e.to_string(), files);
        std::fs::create_dir_all(&folder).map_err(|e| fail(e, Vec::new()))?;
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| fail(e, Vec::new()))?;
        self.seq += 1;
        let stem = format!("{}-{}", std::process::id(), self.seq);
        let input = folder.join(format!("{stem}.in"));
        let err = folder.join(format!("{stem}.err"));
        let files = vec![input.clone(), err.clone()];
        let open = |p: &Path| {
            OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(p)
        };
        {
            use std::io::Write;
            let mut f = open(&input).map_err(|e| fail(e, files.clone()))?;
            f.write_all(body.as_bytes())
                .map_err(|e| fail(e, files.clone()))?;
        }
        let stdin = File::open(&input).map_err(|e| fail(e, files.clone()))?;
        let stderr = open(&err).map_err(|e| fail(e, files.clone()))?;
        let mut cmd = Command::new(program);
        cmd.arg(title)
            .stdin(Stdio::from(stdin))
            .stdout(Stdio::null())
            .stderr(Stdio::from(stderr))
            .process_group(0);
        // SAFETY: between fork and exec only `setrlimit`, which is async-signal-safe, is called,
        // and no memory is allocated.
        unsafe {
            cmd.pre_exec(|| {
                let limit = libc::rlimit {
                    rlim_cur: FILE_LIMIT as libc::rlim_t,
                    rlim_max: FILE_LIMIT as libc::rlim_t,
                };
                if libc::setrlimit(libc::RLIMIT_FSIZE, &limit) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = cmd.spawn().map_err(|e| fail(e, files.clone()))?;
        Ok((child, [input, err]))
    }

    /// The call in flight, once: ended, timed out, or still running.
    fn poll(&mut self, w: &StateWriter) -> Result<()> {
        let Some(f) = self.flight.as_mut() else {
            return Ok(());
        };
        let reason = match f.child.try_wait() {
            Ok(Some(status)) => match (status.code(), status.signal()) {
                (Some(0), _) => None,
                (Some(n), _) => Some(format!("exit {n}")),
                (None, Some(s)) => Some(format!("signal {s}")),
                (None, None) => Some("exit unknown".to_string()),
            },
            Ok(None) if f.started.elapsed() < self.timeout => return Ok(()),
            Ok(None) => {
                kill_group(f);
                Some("timeout".to_string())
            }
            Err(e) => {
                kill_group(f);
                Some(format!("wait: {e}"))
            }
        };
        let Some(f) = self.flight.take() else {
            return Ok(());
        };
        let files = f.files.to_vec();
        self.done = Some(match reason {
            None => Done {
                kind: "notify_sent",
                payload: json!({"to": TO, "pending_ids": f.ids, "command": f.command,
                                "setting": f.setting, "title": f.title, "body": f.body}),
                files,
            },
            Some(reason) => {
                let mask = Mask::new(w.conn());
                Done {
                    kind: "notify_failed",
                    payload: json!({"to": TO, "pending_ids": f.ids, "command": f.command,
                                    "setting": f.setting, "reason": reason,
                                    "stderr": stderr_of(&mask, &f.files[1])}),
                    files,
                }
            }
        });
        Ok(())
    }

    /// The finished call's event, written once; its files removed after.
    fn write(&mut self, w: &StateWriter) -> Result<()> {
        let Some(d) = self.done.as_ref() else {
            return Ok(());
        };
        w.append_event(&NewEvent {
            kind: d.kind,
            actor: Some("makibad"),
            payload: Some(&d.payload),
            ..Default::default()
        })?;
        let Some(d) = self.done.take() else {
            return Ok(());
        };
        for f in &d.files {
            let _ = std::fs::remove_file(f);
        }
        if d.kind == "notify_failed" {
            eprintln!(
                "makibad: the call to the person failed ({}): {}",
                d.payload["reason"].as_str().unwrap_or_default(),
                d.payload["command"].as_str().unwrap_or_default()
            );
            self.log_given_up(w, &d.payload)?;
        }
        Ok(())
    }

    /// One log line for each entry of `payload` that has now failed `TRIES` times.
    fn log_given_up(&self, w: &StateWriter, payload: &Value) -> Result<()> {
        let Some(since) = payload["setting"].as_i64() else {
            return Ok(());
        };
        let records = records(w, since, self.retry)?;
        for id in payload["pending_ids"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_i64)
        {
            if records.get(&id).is_some_and(|r| r.failures == TRIES) {
                eprintln!(
                    "makibad: pending entry {id} failed to reach the person {TRIES} times; it is not called again until {KEY} is set again"
                );
            }
        }
        Ok(())
    }

    /// At a stop: a call still running is ended with its group and not recorded; a finished one
    /// not yet written is tried once more.
    pub fn stop(&mut self, w: &StateWriter) {
        if let Some(mut f) = self.flight.take() {
            kill_group(&mut f);
            for p in &f.files {
                let _ = std::fs::remove_file(p);
            }
        }
        if let Err(e) = self.write(w) {
            eprintln!("makibad: the call's record is lost at the stop: {e}");
        }
    }
}

/// On every way out of `makibad`'s loop, a stop or an error return alike: a call still running
/// is ended with its group, so no command outlives the process that bounds its time.
impl Drop for Call {
    fn drop(&mut self) {
        if let Some(mut f) = self.flight.take() {
            kill_group(&mut f);
            for p in &f.files {
                let _ = std::fs::remove_file(p);
            }
        }
    }
}

/// Kill the call's whole process group and reap its child.
fn kill_group(f: &mut Flight) {
    // SAFETY: a negative pid names the process group the call was started in (its own).
    unsafe { libc::kill(-f.pgid, libc::SIGKILL) };
    let _ = f.child.kill();
    let _ = f.child.wait();
}

/// stderr from its file: masked whole, control characters but newline replaced, then cut.
fn stderr_of(mask: &Mask, path: &Path) -> String {
    let mut raw = Vec::new();
    if let Ok(f) = File::open(path) {
        let _ = f.take(STDERR_READ).read_to_end(&mut raw);
    }
    let text = plain(&mask.text(&String::from_utf8_lossy(&raw)));
    cut_bytes(&text, STDERR_KEEP).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_text_rule_refuses_relative_and_control_characters() {
        assert!(text_rule("relative").is_err());
        assert!(text_rule("/a\nb").is_err());
        assert!(text_rule("/a\0b").is_err());
        assert!(text_rule(&format!("/{}", "a".repeat(VALUE_MAX))).is_err());
        assert!(text_rule("/usr/bin/notify").is_ok());
    }

    #[test]
    fn cutting_keeps_a_character_whole() {
        assert_eq!(cut_bytes("aé", 2), "a");
        assert_eq!(cut_bytes("ab", 5), "ab");
        assert_eq!(plain("a\0b\tc\nd"), "a b c\nd");
    }

    /// A folder only the user can write, under the crate's `target/` (not under `/tmp`).
    fn private_folder() -> tempfile::TempDir {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
        std::fs::create_dir_all(&base).unwrap();
        let d = tempfile::Builder::new()
            .prefix("call-unit-")
            .tempdir_in(&base)
            .unwrap();
        std::fs::set_permissions(d.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        d
    }

    /// The rule hands back the resolved file, which is what a call runs: a link checked once
    /// cannot be pointed at another file before the start.
    #[test]
    fn the_rule_gives_the_resolved_file_that_is_run() {
        let d = private_folder();
        let dir = std::fs::canonicalize(d.path()).unwrap();
        let real = dir.join("real");
        std::fs::write(&real, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o700)).unwrap();
        let link = dir.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let state = dir.join("state");
        std::fs::create_dir_all(&state).unwrap();
        let w = StateWriter::open(&state.join("state.db")).unwrap();
        match disk_rule(w.conn(), &link.to_string_lossy(), &state) {
            Ok(p) => assert_eq!(p, real),
            // A sandbox that shows root's files as another user's refuses `/` itself.
            Err(e) => assert!(
                std::fs::metadata("/").unwrap().uid() != 0,
                "refused where root owns /: {e}"
            ),
        }
    }

    /// A folder `dir` with a state folder (its database made), and a script in a folder
    /// `outside` reached through a link `link` beside the state folder: (dir, state, writer,
    /// link, script).
    fn linked_outside() -> (tempfile::TempDir, PathBuf, StateWriter, PathBuf, PathBuf) {
        let d = private_folder();
        let dir = std::fs::canonicalize(d.path()).unwrap();
        let state = dir.join("state");
        std::fs::create_dir_all(state.join("settings")).unwrap();
        let w = StateWriter::open(&state.join("state.db")).unwrap();
        let outside = dir.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let script = outside.join("call");
        std::fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let link = state.join("work");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        (d, state, w, link, script)
    }

    /// An open run whose workplace is a link to a folder outside every root: a script there is
    /// refused, since the run's worker writes it.
    #[test]
    fn a_script_in_an_open_runs_linked_workplace_is_refused() {
        let (_d, state, w, link, script) = linked_outside();
        w.upsert_run(&crate::db::writer::RunUpsert {
            task_id: "t1",
            project_slug: "p",
            status: Some("in_use"),
            ..Default::default()
        })
        .unwrap();
        w.conn()
            .execute(
                "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree, stage)
                 VALUES ((SELECT id FROM runs WHERE task_id = 't1'), 's1', 'document', 'fake',
                         'b', ?1, 'settings_ready')",
                [link.to_string_lossy()],
            )
            .unwrap();
        let e = disk_rule(w.conn(), &script.to_string_lossy(), &state).unwrap_err();
        assert!(e.contains("the workplace of an open run"), "{e}");
    }

    /// A launch's sandbox whose allowWrite entry is a link to a folder outside every root: a
    /// script there is refused; and a settings file that cannot be read refuses every command.
    #[test]
    fn a_script_under_a_linked_allow_write_entry_is_refused_and_an_unreadable_settings_file_refuses_all()
     {
        let (_d, state, w, link, script) = linked_outside();
        std::fs::write(
            state.join("settings").join("t1.json"),
            json!({"sandbox": {"filesystem": {"allowWrite": [link.to_string_lossy()]}}})
                .to_string(),
        )
        .unwrap();
        let e = disk_rule(w.conn(), &script.to_string_lossy(), &state).unwrap_err();
        assert!(e.contains("a place a launch's sandbox may write"), "{e}");

        std::fs::write(state.join("settings").join("t1.json"), "{").unwrap();
        let e = disk_rule(w.conn(), &script.to_string_lossy(), &state).unwrap_err();
        assert!(e.contains("cannot be read"), "{e}");
    }

    /// A failure made under an older setting does not count against a newer one.
    #[test]
    fn failures_count_only_under_the_setting_they_ran_with() {
        let d = tempfile::tempdir().unwrap();
        let w = StateWriter::open(&d.path().join("state.db")).unwrap();
        let set = |v: &str| {
            w.append_event(&NewEvent {
                kind: "config_set",
                actor: Some("person"),
                payload: Some(&json!({"project": "", "key": KEY, "value": v})),
                ..Default::default()
            })
            .unwrap()
        };
        let old = set("/old");
        let new = set("/new");
        w.append_event(&NewEvent {
            kind: "notify_failed",
            actor: Some("makibad"),
            payload: Some(&json!({"to": TO, "pending_ids": [5], "command": "/old",
                                  "setting": old, "reason": "exit 1", "stderr": ""})),
            ..Default::default()
        })
        .unwrap();
        let retry = Duration::from_secs(60);
        assert_eq!(records(&w, old, retry).unwrap()[&5].failures, 1);
        let under_new = records(&w, new, retry).unwrap();
        assert_eq!(under_new.get(&5).map_or(0, |r| r.failures), 0);
        assert!(!under_new.get(&5).is_some_and(|r| r.recent));
    }

    /// A command that writes without end to stderr is stopped at the file limit, so the state
    /// folder's file system is not filled.
    #[test]
    fn a_noisy_command_cannot_write_past_the_file_limit() {
        let d = tempfile::tempdir().unwrap();
        let script = d.path().join("noisy");
        std::fs::write(&script, "head -c 5000000 /dev/zero >&2\n").unwrap();
        let mut call = Call::new(d.path());
        let (mut child, files) = call
            .spawn(Path::new("/bin/sh"), &script.to_string_lossy(), "")
            .unwrap_or_else(|(e, _)| panic!("cannot start: {e}"));
        let status = child.wait().unwrap();
        let size = std::fs::metadata(&files[1]).unwrap().len();
        assert!(
            size <= FILE_LIMIT,
            "stderr grew to {size} bytes ({status:?})"
        );
        assert!(!status.success(), "{status:?}");
    }

    /// Dropping the pass, as any return from the loop does, ends a running call's whole group.
    #[test]
    fn dropping_the_pass_ends_a_running_call_and_its_group() {
        let d = tempfile::tempdir().unwrap();
        let pid_file = d.path().join("pid");
        let child = Command::new("/bin/sh")
            .arg("-c")
            .arg(format!(
                "sleep 60 & echo $! > '{}'; wait",
                pid_file.display()
            ))
            .stdout(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        let until = Instant::now() + Duration::from_secs(10);
        let pid: i32 = loop {
            if let Some(p) = std::fs::read_to_string(&pid_file)
                .ok()
                .and_then(|s| s.trim().parse().ok())
            {
                break p;
            }
            assert!(Instant::now() < until, "the grandchild did not start");
            std::thread::sleep(Duration::from_millis(20));
        };
        let mut call = Call::new(d.path());
        call.flight = Some(Flight {
            pgid: child.id() as i32,
            child,
            ids: vec![1],
            command: "/bin/sh".into(),
            setting: 1,
            title: String::new(),
            body: String::new(),
            started: Instant::now(),
            files: [d.path().join("a.in"), d.path().join("a.err")],
        });
        drop(call);
        let until = Instant::now() + Duration::from_secs(5);
        // SAFETY: signal 0 only checks that the process exists.
        while unsafe { libc::kill(pid, 0) } == 0 && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        // SAFETY: as above.
        let alive = unsafe { libc::kill(pid, 0) } == 0;
        if alive {
            // SAFETY: the grandchild this test started.
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
        assert!(!alive, "the grandchild {pid} outlived the pass");
    }
}
