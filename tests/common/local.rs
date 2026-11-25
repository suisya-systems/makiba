//! `Local`: a `Base` that really runs a terminal's argv in a child process, for tests that need a
//! program to behave (the stand-in `Stub` does not run argv). Output is read through a pipe, so
//! OSC 7501 records in it become the terminal's status and every other line the screen. An erase
//! of the display (ESC `[2J`) empties the screen, as on a terminal grid; `read` with 0 lines
//! returns the whole screen (a real base's screen with no history), with N the last N lines.
//! Input goes to the child's stdin. Tests use it when no real base is wanted.
//!
//! `events()` (M2-2) queues one event per observed change: `Created` at launch, `StatusChanged` for
//! every OSC 7501 record the program prints (a repeat of the same state included, so none is lost
//! to polling the latest status), `Exited` when the program is gone and `Removed` by `end`. A
//! `Local` is `Clone`: the clones share the terminals, so a test can hand one to the daemon and
//! keep one to script with. The terminals are killed when the last clone is dropped.
#![allow(dead_code)]

use makiba::base::version::PINNED;
use makiba::base::{
    Base, BaseDenies, BaseError, BaseEvent, BaseInfo, BlockKind, Detail, EventStream, LaunchSpec,
    State, Status, Terminal, TerminalEvent, TerminalRef,
};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Default)]
struct Seen {
    status: Option<Status>,
    screen: Vec<String>,
}

struct Proc {
    child: Child,
    stdin: ChildStdin,
    seen: Arc<Mutex<Seen>>,
    tags: BTreeMap<String, String>,
}

type Subscribers = Arc<Mutex<Vec<Sender<BaseEvent>>>>;

#[derive(Default, Clone)]
pub struct Local {
    procs: Arc<Mutex<BTreeMap<String, Proc>>>,
    subscribers: Subscribers,
}

/// Queue `event` for every open stream. Never called with the `procs` lock held.
fn emit(subscribers: &Subscribers, event: TerminalEvent) {
    let event = BaseEvent::Event(event);
    subscribers
        .lock()
        .unwrap()
        .retain(|s| s.send(event.clone()).is_ok());
}

fn err(variant: fn(Detail) -> BaseError, kind: &str, message: String) -> BaseError {
    variant(Detail {
        kind: kind.into(),
        message,
        hint: None,
    })
}

/// Read `ESC ] 7501 ; state=S:app=A[:kind=K[:msg=M]] BEL` out of `line`, returning the status
/// and the rest of the line. `state=none` is a record of no status: `Some(None)`.
fn take_record(line: &str) -> (Option<Option<Status>>, String) {
    const OPEN: &str = "\x1b]7501;";
    let Some(start) = line.find(OPEN) else {
        return (None, line.to_string());
    };
    let Some(len) = line[start..].find('\x07') else {
        return (None, line.to_string());
    };
    let body = &line[start + OPEN.len()..start + len];
    let rest = format!("{}{}", &line[..start], &line[start + len + 1..]);
    let field = |name: &str| {
        body.split(':')
            .find_map(|f| f.strip_prefix(name)?.strip_prefix('='))
    };
    let state = match field("state") {
        Some("idle") => State::Idle,
        Some("working") => State::Working,
        Some("blocked") => State::Blocked,
        Some("done") => State::Done,
        Some("error") => State::Error,
        Some("none") => return (Some(None), rest),
        _ => return (None, rest),
    };
    let kind = match field("kind") {
        Some("permission") => Some(BlockKind::Permission),
        Some("question") => Some(BlockKind::Question),
        Some("auth") => Some(BlockKind::Auth),
        _ => None,
    };
    let msg = body.split_once(":msg=").map(|(_, m)| m.to_string());
    (Some(Some(Status { state, kind, msg })), rest)
}

/// Erase in display, all of it.
const ERASE: &str = "\x1b[2J";

fn key_bytes(key: &str) -> Vec<u8> {
    match key {
        "enter" => b"\n".to_vec(),
        "up" => b"\x1b[A".to_vec(),
        "down" => b"\x1b[B".to_vec(),
        "right" => b"\x1b[C".to_vec(),
        "left" => b"\x1b[D".to_vec(),
        "esc" => b"\x1b".to_vec(),
        "space" => b" ".to_vec(),
        other => other.as_bytes().to_vec(),
    }
}

impl Local {
    pub fn new() -> Local {
        Local::default()
    }

    fn with<T>(
        &self,
        t: &TerminalRef,
        f: impl FnOnce(&mut Proc) -> Result<T, BaseError>,
    ) -> Result<T, BaseError> {
        let mut procs = self.procs.lock().unwrap();
        let p = procs.get_mut(&t.0).ok_or_else(|| {
            err(
                BaseError::NotFound,
                "terminal",
                format!("no terminal {:?}", t.0),
            )
        })?;
        f(p)
    }

    /// Queue a `Resync` with a fresh `list` for every open stream, as a base does after a
    /// reconnect (M2-5). The subscribers stay locked across the list, so no event falls between.
    pub fn resync(&self) {
        let mut subscribers = self.subscribers.lock().unwrap();
        let event = BaseEvent::Resync(self.list().expect("list"));
        subscribers.retain(|s| s.send(event.clone()).is_ok());
    }

    fn write(&self, t: &TerminalRef, bytes: &[u8]) -> Result<(), BaseError> {
        self.with(t, |p| {
            p.stdin
                .write_all(bytes)
                .and_then(|_| p.stdin.flush())
                .map_err(|e| err(BaseError::Finished, "finished", e.to_string()))
        })
    }
}

impl Drop for Local {
    fn drop(&mut self) {
        if Arc::strong_count(&self.procs) > 1 {
            return; // another clone still uses the terminals
        }
        for p in self.procs.lock().unwrap().values_mut() {
            let _ = p.child.kill();
            let _ = p.child.wait();
        }
    }
}

/// What `get` and `list` answer for the terminal `t`, read while the caller holds the lock.
fn snapshot(t: &TerminalRef, p: &mut Proc) -> Result<Terminal, BaseError> {
    let exit = p
        .child
        .try_wait()
        .map_err(|e| err(BaseError::Other, "wait", e.to_string()))?;
    Ok(Terminal {
        id: t.clone(),
        name: t.0.clone(),
        tags: p.tags.clone(),
        status: p.seen.lock().unwrap().status.clone(),
        turn: None,
        alive: exit.is_none(),
        exit_code: exit.and_then(|e| e.code()),
    })
}

impl Base for Local {
    fn name(&self) -> &'static str {
        "local"
    }

    fn hello(&self) -> Result<BaseInfo, BaseError> {
        Ok(BaseInfo {
            name: "local".into(),
            version: PINNED.into(),
        })
    }

    fn launch(&self, spec: &LaunchSpec) -> Result<TerminalRef, BaseError> {
        let (program, args) = spec
            .argv
            .split_first()
            .ok_or_else(|| err(BaseError::Other, "argv", "empty argv".into()))?;
        let mut cmd = Command::new(program);
        cmd.args(args)
            .current_dir(&spec.cwd)
            .envs(&spec.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        for v in &spec.unset {
            cmd.env_remove(v);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| err(BaseError::Other, "launch", format!("{program}: {e}")))?;
        let stdin = child.stdin.take().expect("piped");
        let stdout = child.stdout.take().expect("piped");
        let seen = Arc::new(Mutex::new(Seen::default()));
        {
            let mut procs = self.procs.lock().unwrap();
            if procs.contains_key(&spec.name) {
                let _ = child.kill();
                return Err(err(
                    BaseError::Other,
                    "duplicate",
                    format!("a terminal named {:?} exists", spec.name),
                ));
            }
            procs.insert(
                spec.name.clone(),
                Proc {
                    child,
                    stdin,
                    seen: Arc::clone(&seen),
                    tags: spec.tags.clone(),
                },
            );
        }
        let id = TerminalRef(spec.name.clone());
        let snapshot = {
            let (id, name, tags) = (id.clone(), spec.name.clone(), spec.tags.clone());
            move |status: Option<Status>, alive: bool, exit_code: Option<i32>| Terminal {
                id: id.clone(),
                name: name.clone(),
                tags: tags.clone(),
                status,
                turn: None,
                alive,
                exit_code,
            }
        };
        emit(
            &self.subscribers,
            TerminalEvent::Created(snapshot(None, true, None)),
        );
        let (procs, subscribers, name) = (
            Arc::clone(&self.procs),
            Arc::clone(&self.subscribers),
            spec.name.clone(),
        );
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).split(b'\n').flatten() {
                let (status, mut rest) = take_record(&String::from_utf8_lossy(&line));
                let erased = rest
                    .rfind(ERASE)
                    .map(|at| rest = rest[at + ERASE.len()..].into());
                let mut s = seen.lock().unwrap();
                if erased.is_some() {
                    s.screen.clear();
                }
                if let Some(status) = &status {
                    s.status = status.clone();
                }
                // A blank line is a row of the screen; a line that held only a record is not.
                if !rest.is_empty() || (status.is_none() && erased.is_none()) {
                    s.screen.push(rest);
                }
                drop(s);
                if let Some(status) = status {
                    emit(
                        &subscribers,
                        TerminalEvent::StatusChanged(snapshot(status, true, None)),
                    );
                }
            }
            // The output ended: wait for the exit code, unless `end` took the terminal first.
            let code = loop {
                let mut procs = procs.lock().unwrap();
                let Some(p) = procs.get_mut(&name) else {
                    return;
                };
                match p.child.try_wait() {
                    Ok(Some(exit)) => break exit.code(),
                    Ok(None) => {}
                    Err(_) => return,
                }
                drop(procs);
                std::thread::sleep(Duration::from_millis(10));
            };
            let status = seen.lock().unwrap().status.clone();
            emit(
                &subscribers,
                TerminalEvent::Exited(snapshot(status, false, code)),
            );
        });
        Ok(TerminalRef(spec.name.clone()))
    }

    /// The terminals as they are at one moment: read under one hold of the lock, so an `end` in
    /// another thread falls before the list or after it, never inside it (Issue 506).
    fn list(&self) -> Result<Vec<Terminal>, BaseError> {
        let mut procs = self.procs.lock().unwrap();
        procs
            .iter_mut()
            .map(|(name, p)| snapshot(&TerminalRef(name.clone()), p))
            .collect()
    }

    fn get(&self, t: &TerminalRef) -> Result<Terminal, BaseError> {
        self.with(t, |p| snapshot(t, p))
    }

    fn read(&self, t: &TerminalRef, lines: usize) -> Result<Vec<String>, BaseError> {
        self.with(t, |p| {
            let s = p.seen.lock().unwrap();
            let from = match lines {
                0 => 0,
                n => s.screen.len().saturating_sub(n),
            };
            Ok(s.screen[from..].to_vec())
        })
    }

    fn keys(&self, t: &TerminalRef, keys: &[&str]) -> Result<(), BaseError> {
        self.write(
            t,
            &keys.iter().flat_map(|k| key_bytes(k)).collect::<Vec<u8>>(),
        )
    }

    fn send(&self, t: &TerminalRef, text: &str, _only_if_idle: bool) -> Result<(), BaseError> {
        // As the real base: typing into a blocked terminal would answer its dialog.
        if self
            .get(t)?
            .status
            .is_some_and(|s| s.state == State::Blocked)
        {
            return Err(err(
                BaseError::Blocked,
                "blocked",
                "typing would answer its dialog; send keys instead".into(),
            ));
        }
        self.write(t, format!("{text}\n").as_bytes())
    }

    fn end(&self, t: &TerminalRef) -> Result<(), BaseError> {
        let mut p = self.procs.lock().unwrap().remove(&t.0).ok_or_else(|| {
            err(
                BaseError::NotFound,
                "terminal",
                format!("no terminal {:?}", t.0),
            )
        })?;
        let _ = p.child.kill();
        let _ = p.child.wait();
        emit(&self.subscribers, TerminalEvent::Removed(t.clone()));
        Ok(())
    }

    fn events(&self) -> Result<EventStream, BaseError> {
        let (tx, rx) = channel();
        // The lock is held across the list and the registration, so no event falls between them.
        let mut subscribers = self.subscribers.lock().unwrap();
        tx.send(BaseEvent::Resync(self.list()?))
            .expect("the receiver is held here");
        subscribers.push(tx);
        Ok(Box::new(rx.into_iter()))
    }

    fn start(&self) -> Result<(), BaseError> {
        Ok(())
    }

    fn worker_denies(&self) -> BaseDenies {
        BaseDenies::default()
    }
}
