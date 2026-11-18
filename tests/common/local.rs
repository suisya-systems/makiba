//! `Local`: a `Base` that really runs a terminal's argv in a child process, for tests that need a
//! program to behave (the stand-in `Stub` does not run argv). Output is read through a pipe, so
//! OSC 7501 records in it become the terminal's status and every other line the screen. Input
//! goes to the child's stdin. Tests use it when no real base is wanted.
#![allow(dead_code)]

use makiba::base::version::PINNED;
use makiba::base::{
    Base, BaseDenies, BaseError, BaseInfo, BlockKind, Detail, EventStream, LaunchSpec, State,
    Status, Terminal, TerminalRef,
};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};

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

#[derive(Default)]
pub struct Local {
    procs: Mutex<BTreeMap<String, Proc>>,
}

fn err(variant: fn(Detail) -> BaseError, kind: &str, message: String) -> BaseError {
    variant(Detail {
        kind: kind.into(),
        message,
        hint: None,
    })
}

/// Read `ESC ] 7501 ; state=S:app=A[:kind=K] BEL` out of `line`, returning the status and the
/// rest of the line.
fn take_record(line: &str) -> (Option<Status>, String) {
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
        _ => return (None, rest),
    };
    let kind = match field("kind") {
        Some("permission") => Some(BlockKind::Permission),
        Some("question") => Some(BlockKind::Question),
        Some("auth") => Some(BlockKind::Auth),
        _ => None,
    };
    (
        Some(Status {
            state,
            kind,
            msg: None,
        }),
        rest,
    )
}

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
        for p in self.procs.lock().unwrap().values_mut() {
            let _ = p.child.kill();
            let _ = p.child.wait();
        }
    }
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
        let sink = Arc::clone(&seen);
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).split(b'\n').flatten() {
                let (status, rest) = take_record(&String::from_utf8_lossy(&line));
                let mut s = sink.lock().unwrap();
                if status.is_some() {
                    s.status = status;
                }
                if !rest.is_empty() {
                    s.screen.push(rest);
                }
            }
        });
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
                seen,
                tags: spec.tags.clone(),
            },
        );
        Ok(TerminalRef(spec.name.clone()))
    }

    fn list(&self) -> Result<Vec<Terminal>, BaseError> {
        let names: Vec<String> = self.procs.lock().unwrap().keys().cloned().collect();
        names
            .iter()
            .map(|n| self.get(&TerminalRef(n.clone())))
            .collect()
    }

    fn get(&self, t: &TerminalRef) -> Result<Terminal, BaseError> {
        self.with(t, |p| {
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
        })
    }

    fn read(&self, t: &TerminalRef, lines: usize) -> Result<Vec<String>, BaseError> {
        self.with(t, |p| {
            let s = p.seen.lock().unwrap();
            Ok(s.screen[s.screen.len().saturating_sub(lines)..].to_vec())
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
        Ok(())
    }

    fn events(&self) -> Result<EventStream, BaseError> {
        Err(err(BaseError::Other, "unsupported", "events".into()))
    }

    fn start(&self) -> Result<(), BaseError> {
        Ok(())
    }

    fn worker_denies(&self) -> BaseDenies {
        BaseDenies::default()
    }
}
