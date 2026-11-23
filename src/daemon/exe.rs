//! The executable `makibad` runs, and the one at its path on disk (Issue 340).
//!
//! A rebuild or an install puts a new file at the path of a running `makiba` (a running executable
//! cannot be written in place), and the old `makibad` goes on running the old one while the door's
//! clients and the hooks run the new one. A build is told by the SHA-256 of the executable's
//! content: the package version does not change between builds.
//!
//! - `running_digest`: the digest of the file this process runs (`/proc/self/exe`, which names it
//!   after the path was replaced), filled once by the watcher thread; hashing a debug binary takes
//!   seconds, so it never runs on the daemon's tick thread. The door's socket answers carry it.
//! - `own_digest`: the same, computed on demand by a client of the door when a call failed.
//! - `Watch`: the watcher thread looks at the path every second. A file whose identity (device,
//!   inode, size, modification time) has held still for one look is hashed through an opened
//!   descriptor, and the result is kept only when the file did not change while it was read. Same
//!   content: a log line and nothing else. Other content: once per process, `anomaly_raised` of
//!   the rule `binary_replaced` and a notice to the secretary, written by `pass` on the daemon's
//!   thread. The next start clears it (`clear_at_start`): the file on disk is what runs then.

use crate::db::writer::NewEvent;
use crate::db::{Result, StateWriter};
use crate::spec::AGENT_SECRETARY;
use rusqlite::params;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The anomaly's rule name. It is not one of `anomalies::Rule`, so the anomaly pass leaves it.
pub const RULE: &str = "binary_replaced";
/// How often the watcher looks at the path.
const LOOK: Duration = Duration::from_secs(1);

static RUNNING: OnceLock<Option<String>> = OnceLock::new();

/// The file this process runs: `/proc/self/exe` on Linux, which keeps naming it after its path
/// was given to another file; elsewhere the path it was started from.
fn running_file() -> Option<PathBuf> {
    if cfg!(target_os = "linux") {
        Some(PathBuf::from("/proc/self/exe"))
    } else {
        std::env::current_exe().ok()
    }
}

fn hash(r: &mut impl Read) -> std::io::Result<String> {
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

fn digest_of(path: &Path) -> Option<String> {
    hash(&mut std::fs::File::open(path).ok()?).ok()
}

/// The digest of the executable this process runs, computed now. For a client of the door, once a
/// call failed.
pub fn own_digest() -> Option<String> {
    digest_of(&running_file()?)
}

/// `makibad`'s digest once the watcher has hashed it; `None` before, or when it could not.
pub fn running_digest() -> Option<&'static str> {
    RUNNING.get().and_then(|d| d.as_deref())
}

/// The first twelve hex digits: enough to tell two builds apart in a line.
pub fn short(digest: &str) -> &str {
    digest.get(..12).unwrap_or(digest)
}

/// The line a client adds to a failed call when `makibad` answered with another `build` than its
/// own; `None` when the builds are the same, or either is unknown.
pub fn other_build_line(answer: &Value) -> Option<String> {
    let theirs = answer["build"].as_str()?;
    let mine = own_digest()?;
    (theirs != mine).then(|| {
        format!(
            "makibad runs another build of makiba ({}) than this makiba ({}); makiba down and \
             makiba up bring them together.",
            short(theirs),
            short(&mine)
        )
    })
}

/// What the watcher saw: the file at `path` is another build than the one running.
struct Replaced {
    path: PathBuf,
    running: String,
    on_disk: String,
    /// Seconds since the epoch.
    seen_at: f64,
}

/// The identity of the file at a path, as far as a look can tell it changed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Ident {
    dev: u64,
    ino: u64,
    size: u64,
    mtime: i64,
    mtime_nsec: i64,
}

impl Ident {
    fn of(m: &std::fs::Metadata) -> Ident {
        Ident {
            dev: m.dev(),
            ino: m.ino(),
            size: m.size(),
            mtime: m.mtime(),
            mtime_nsec: m.mtime_nsec(),
        }
    }
}

/// The digest of the file at `path` when it held still while it was read: the descriptor's
/// identity and the path's are the same `want` before and after.
fn stable_digest(path: &Path, want: Ident) -> Option<String> {
    let mut f = std::fs::File::open(path).ok()?;
    if Ident::of(&f.metadata().ok()?) != want {
        return None;
    }
    let d = hash(&mut f).ok()?;
    let after = Ident::of(&f.metadata().ok()?);
    let at_path = Ident::of(&std::fs::metadata(path).ok()?);
    (after == want && at_path == want).then_some(d)
}

pub struct Watch {
    found: Arc<Mutex<Option<Replaced>>>,
    written: bool,
}

impl Watch {
    /// Start the watcher thread on the path this process was started from. It ends when `stop`
    /// is set, or once it has found another build.
    pub fn start(stop: Arc<AtomicBool>) -> Watch {
        let found = Arc::new(Mutex::new(None));
        let w = Watch {
            found: Arc::clone(&found),
            written: false,
        };
        let Ok(path) = std::env::current_exe() else {
            eprintln!("makibad: cannot tell the path of its own executable; it is not watched");
            return w;
        };
        // The running file's identity, before anything may replace it.
        let running_ident = running_file()
            .and_then(|f| std::fs::metadata(f).ok())
            .map(|m| Ident::of(&m));
        let spawned = std::thread::Builder::new()
            .name("exe-watch".into())
            .spawn(move || watch(&path, running_ident, &stop, &found));
        if let Err(e) = spawned {
            eprintln!("makibad: cannot start the executable's watcher: {e}");
        }
        w
    }

    /// Write what the watcher found, once: the anomaly and the notice in one transaction. A write
    /// that fails is tried again on the next call.
    pub fn pass(&mut self, w: &StateWriter) -> Result<()> {
        if self.written {
            return Ok(());
        }
        let found = self.found.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(r) = found.as_ref() else {
            return Ok(());
        };
        w.transaction(|w| raise(w, r))?;
        self.written = true;
        eprintln!(
            "makibad: anomaly {RULE}: {} is now build {}, this makibad runs build {}",
            r.path.display(),
            short(&r.on_disk),
            short(&r.running)
        );
        Ok(())
    }
}

fn watch(
    path: &Path,
    running_ident: Option<Ident>,
    stop: &AtomicBool,
    found: &Mutex<Option<Replaced>>,
) {
    let running = RUNNING
        .get_or_init(|| running_file().and_then(|f| digest_of(&f)))
        .clone();
    let Some(running) = running else {
        eprintln!("makibad: cannot read its own executable; it is not watched");
        return;
    };
    // The identity last judged (the running file's at first), and the one seen at the last look.
    let mut judged = running_ident;
    let mut last: Option<Ident> = running_ident;
    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(LOOK);
        let Some(now) = std::fs::metadata(path).ok().map(|m| Ident::of(&m)) else {
            last = None;
            continue;
        };
        let settled = last == Some(now);
        last = Some(now);
        if judged == Some(now) || !settled {
            continue;
        }
        let Some(on_disk) = stable_digest(path, now) else {
            continue;
        };
        judged = Some(now);
        if on_disk == running {
            eprintln!(
                "makibad: {} was replaced by a file of the same content",
                path.display()
            );
            continue;
        }
        let seen_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64());
        *found.lock().unwrap_or_else(PoisonError::into_inner) = Some(Replaced {
            path: path.to_path_buf(),
            running,
            on_disk,
            seen_at,
        });
        return;
    }
}

/// The anomaly's event and the notice to the secretary (as `recover.rs` writes its run-less
/// `unknown_terminal`). The caller wraps it in a transaction.
fn raise(w: &StateWriter, r: &Replaced) -> Result<()> {
    let seen: String = w.conn().query_row(
        "SELECT strftime('%Y-%m-%dT%H:%M:%fZ', ?1, 'unixepoch')",
        [r.seen_at],
        |row| row.get(0),
    )?;
    let actor = Some("makibad");
    let gate = super::notify_gate::decide(RULE);
    let mut payload = json!({
        "task_id": null, "rule": RULE, "pid": std::process::id(),
        "path": r.path.display().to_string(), "running": r.running, "on_disk": r.on_disk,
    });
    if !gate.send {
        payload["suppressed"] = gate.reason.into();
    }
    w.append_event(&NewEvent {
        kind: "anomaly_raised",
        actor,
        payload: Some(&payload),
        occurred_at: Some(&seen),
        ..Default::default()
    })?;
    if !gate.send {
        return Ok(());
    }
    let sent = w.append_event(&NewEvent {
        kind: "message_sent",
        actor,
        occurred_at: Some(&seen),
        ..Default::default()
    })?;
    let body = format!(
        "Anomaly {RULE}: makibad (pid {}) runs the makiba it was started from, build {}; {} on \
         disk is now another file, build {}, seen at {seen}. The hooks and the door's clients run \
         the file on disk. makiba down and makiba up bring them together.",
        std::process::id(),
        short(&r.running),
        r.path.display(),
        short(&r.on_disk),
    );
    w.conn().execute(
        "INSERT INTO messages (event_id, sender, recipient, kind, body)
         VALUES (?1, 'makibad', ?2, 'notice', ?3)",
        params![sent, AGENT_SECRETARY, body],
    )?;
    let message = w.conn().last_insert_rowid();
    w.conn().execute(
        "UPDATE events SET payload_json = json_object('message_id', ?1) WHERE id = ?2",
        params![message, sent],
    )?;
    w.conn().execute(
        "INSERT INTO event_deliveries (source_event_id, recipient, status, attempt)
         VALUES (?1, ?2, 'pending', 0)",
        params![sent, AGENT_SECRETARY],
    )?;
    Ok(())
}

/// At a start: clear every open `binary_replaced`, with the payload it was raised with. The new
/// `makibad` runs the file on disk, so what the anomaly said no longer holds.
pub fn clear_at_start(w: &StateWriter) -> Result<()> {
    let mut q = w.conn().prepare(
        "SELECT kind, payload_json FROM events
         WHERE kind IN ('anomaly_raised', 'anomaly_cleared')
           AND json_extract(payload_json, '$.rule') = ?1 ORDER BY id",
    )?;
    let rows = q.query_map([RULE], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    let mut open: Vec<Value> = Vec::new();
    for row in rows {
        let (kind, payload) = row?;
        let mut p: Value = serde_json::from_str(&payload).unwrap_or(Value::Null);
        if let Some(m) = p.as_object_mut() {
            m.remove("suppressed");
        }
        open.retain(|o| *o != p);
        if kind == "anomaly_raised" {
            open.push(p);
        }
    }
    drop(q);
    w.transaction(|w| {
        for p in &open {
            w.append_event(&NewEvent {
                kind: "anomaly_cleared",
                actor: Some("makibad"),
                payload: Some(p),
                ..Default::default()
            })?;
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_short_form_is_twelve_digits() {
        assert_eq!(short("0123456789abcdef"), "0123456789ab");
        assert_eq!(short("abc"), "abc");
    }

    #[test]
    fn a_file_changed_while_read_has_no_digest() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        std::fs::write(&p, b"one").unwrap();
        let id = Ident::of(&std::fs::metadata(&p).unwrap());
        assert!(stable_digest(&p, id).is_some());
        std::fs::write(&p, b"other content").unwrap();
        assert_eq!(stable_digest(&p, id), None);
    }

    #[test]
    fn an_answer_without_build_adds_nothing() {
        assert_eq!(other_build_line(&json!({"ok": false})), None);
    }

    #[test]
    fn an_answer_of_another_build_says_so() {
        let line = other_build_line(&json!({"ok": false, "build": "f".repeat(64)})).unwrap();
        assert!(line.contains("ffffffffffff"), "{line}");
        assert!(line.contains("makiba down and makiba up"), "{line}");
        let mine = own_digest().unwrap();
        assert_eq!(other_build_line(&json!({"ok": false, "build": mine})), None);
    }
}
