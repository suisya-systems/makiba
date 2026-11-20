//! The stand-in for the base: an in-process `Base` with scripted terminals. It is the reference
//! for what the interface means (spec section 6.1); tests use it where no real base can run.

use super::version::{PINNED, checked_hello};
use super::{
    Base, BaseDenies, BaseError, BaseEvent, BaseInfo, Detail, EventStream, LaunchSpec, State,
    Status, Terminal, TerminalEvent, TerminalRef,
};
use std::collections::BTreeMap;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

struct Entry {
    terminal: Terminal,
    screen: Vec<String>,
}

#[derive(Default)]
struct Inner {
    terminals: BTreeMap<String, Entry>,
    subscribers: Vec<Sender<BaseEvent>>,
}

impl Inner {
    fn emit(&mut self, event: TerminalEvent) {
        let event = BaseEvent::Event(event);
        // A dropped stream is forgotten.
        self.subscribers.retain(|s| s.send(event.clone()).is_ok());
    }

    fn entry(&mut self, t: &TerminalRef) -> Result<&mut Entry, BaseError> {
        self.terminals.get_mut(&t.0).ok_or_else(|| {
            error(
                BaseError::NotFound,
                "not_found",
                &format!("no terminal {:?}", t.0),
            )
        })
    }
}

fn error(variant: fn(Detail) -> BaseError, kind: &str, message: &str) -> BaseError {
    variant(Detail {
        kind: kind.into(),
        message: message.into(),
        hint: None,
    })
}

pub struct Stub {
    /// What a launched terminal shows, whatever its argv: the script.
    script: Vec<String>,
    version: String,
    organization: Option<String>,
    subscribe_delay: Duration,
    inner: Mutex<Inner>,
}

impl Default for Stub {
    fn default() -> Self {
        Self::new()
    }
}

impl Stub {
    /// A stand-in whose terminals show nothing.
    pub fn new() -> Stub {
        Stub::scripted(&[])
    }

    /// A stand-in whose terminals show `lines`, then sit idle. The argv is not run.
    pub fn scripted(lines: &[&str]) -> Stub {
        Stub {
            script: lines.iter().map(|l| l.to_string()).collect(),
            version: PINNED.to_string(),
            organization: None,
            subscribe_delay: Duration::ZERO,
            inner: Mutex::new(Inner::default()),
        }
    }

    /// The version `hello` reports.
    pub fn with_version(mut self, version: &str) -> Stub {
        self.version = version.to_string();
        self
    }

    /// `list` returns only terminals whose `makiba.org` tag is `org`.
    pub fn organization(mut self, org: &str) -> Stub {
        self.organization = Some(org.to_string());
        self
    }

    /// `events()` yields its first item (the `Resync`) only after `delay`, as a base that is slow
    /// to answer would.
    pub fn with_subscribe_delay(mut self, delay: Duration) -> Stub {
        self.subscribe_delay = delay;
        self
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn mine(&self, t: &Terminal) -> bool {
        self.organization
            .as_ref()
            .is_none_or(|org| t.tags.get("makiba.org") == Some(org))
    }

    /// Script a change of state, as the program in the terminal would report it.
    pub fn set_state(&self, t: &TerminalRef, state: State) {
        let mut inner = self.lock();
        let Ok(e) = inner.entry(t) else { return };
        e.terminal.status = Some(Status {
            state,
            kind: None,
            msg: None,
        });
        let after = e.terminal.clone();
        inner.emit(TerminalEvent::StatusChanged(after));
    }

    /// Script the program's exit. The terminal stays until `end`, as a finished one does.
    pub fn exit(&self, t: &TerminalRef, code: i32) {
        let mut inner = self.lock();
        let Ok(e) = inner.entry(t) else { return };
        e.terminal.alive = false;
        e.terminal.exit_code = Some(code);
        e.terminal.status = Some(Status {
            state: if code == 0 { State::Done } else { State::Error },
            kind: None,
            msg: None,
        });
        let after = e.terminal.clone();
        inner.emit(TerminalEvent::Exited(after));
    }
}

impl Base for Stub {
    fn name(&self) -> &'static str {
        "stub"
    }

    fn hello(&self) -> Result<BaseInfo, BaseError> {
        checked_hello(|| {
            Ok(BaseInfo {
                name: "stub".into(),
                version: self.version.clone(),
            })
        })
    }

    fn launch(&self, spec: &LaunchSpec) -> Result<TerminalRef, BaseError> {
        let mut inner = self.lock();
        if inner.terminals.contains_key(&spec.name) {
            return Err(error(
                BaseError::Other,
                "duplicate",
                &format!("a terminal named {:?} exists", spec.name),
            ));
        }
        let id = TerminalRef(spec.name.clone());
        let terminal = Terminal {
            id: id.clone(),
            name: spec.name.clone(),
            tags: spec.tags.clone(),
            status: Some(Status {
                state: State::Idle,
                kind: None,
                msg: None,
            }),
            turn: None,
            alive: true,
            exit_code: None,
        };
        inner.terminals.insert(
            spec.name.clone(),
            Entry {
                terminal: terminal.clone(),
                screen: self.script.clone(),
            },
        );
        inner.emit(TerminalEvent::Created(terminal));
        Ok(id)
    }

    fn list(&self) -> Result<Vec<Terminal>, BaseError> {
        let inner = self.lock();
        Ok(inner
            .terminals
            .values()
            .map(|e| e.terminal.clone())
            .filter(|t| self.mine(t))
            .collect())
    }

    fn get(&self, t: &TerminalRef) -> Result<Terminal, BaseError> {
        Ok(self.lock().entry(t)?.terminal.clone())
    }

    fn read(&self, t: &TerminalRef, lines: usize) -> Result<Vec<String>, BaseError> {
        let mut inner = self.lock();
        let screen = &inner.entry(t)?.screen;
        Ok(screen[screen.len().saturating_sub(lines)..].to_vec())
    }

    fn keys(&self, t: &TerminalRef, _keys: &[&str]) -> Result<(), BaseError> {
        let mut inner = self.lock();
        let e = inner.entry(t)?;
        if !e.terminal.alive {
            return Err(error(
                BaseError::Finished,
                "finished",
                "the terminal has exited",
            ));
        }
        Ok(())
    }

    fn send(&self, t: &TerminalRef, text: &str, only_if_idle: bool) -> Result<(), BaseError> {
        let mut inner = self.lock();
        let e = inner.entry(t)?;
        if !e.terminal.alive {
            return Err(error(
                BaseError::Finished,
                "finished",
                "the terminal has exited",
            ));
        }
        let state = e.terminal.status.as_ref().map(|s| s.state);
        // As hc: a blocked terminal takes no text, whatever the caller asked.
        if state == Some(State::Blocked) {
            return Err(error(
                BaseError::Blocked,
                "blocked",
                "the terminal is blocked on a prompt",
            ));
        }
        if only_if_idle && state != Some(State::Idle) {
            return Err(error(
                BaseError::StatusMismatch,
                "status_mismatch",
                &format!("the terminal is {state:?}, not idle"),
            ));
        }
        e.screen.extend(text.lines().map(String::from));
        Ok(())
    }

    fn end(&self, t: &TerminalRef) -> Result<(), BaseError> {
        let mut inner = self.lock();
        inner.entry(t)?;
        inner.terminals.remove(&t.0);
        inner.emit(TerminalEvent::Removed(t.clone()));
        Ok(())
    }

    fn events(&self) -> Result<EventStream, BaseError> {
        let (tx, rx) = channel();
        let mut inner = self.lock();
        let list = inner
            .terminals
            .values()
            .map(|e| e.terminal.clone())
            .filter(|t| self.mine(t))
            .collect();
        tx.send(BaseEvent::Resync(list))
            .expect("the receiver is held here");
        inner.subscribers.push(tx);
        // Blocks while empty; ends only when the stand-in is dropped.
        let mut delay = Some(self.subscribe_delay);
        let mut items = rx.into_iter();
        Ok(Box::new(std::iter::from_fn(move || {
            if let Some(d) = delay.take() {
                std::thread::sleep(d);
            }
            items.next()
        })))
    }

    fn start(&self) -> Result<(), BaseError> {
        Ok(())
    }

    fn stop(&self) -> Result<(), BaseError> {
        Ok(())
    }

    fn worker_denies(&self) -> BaseDenies {
        BaseDenies {
            commands: ["hc", "herdr-core", "herdr-app"].map(String::from).into(),
            resolved: Vec::new(),
            folders: Vec::new(),
        }
    }
}
