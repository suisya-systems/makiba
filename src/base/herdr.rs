//! The real base: herdr-core over its Unix socket, one JSON-RPC 2.0 message per line.
//! Private to `base`; nothing else names it (spec section 6.1, `tests/base_seam.rs`).
//! M1-1 builds `hello()`, M1-2 the terminal operations, M1-3 `events()` (`herdr/events.rs`). Each call
//! opens its own connection, has a read and write timeout, and is
//! never retried here.

mod events;

use super::version::checked_hello;
use super::{
    Base, BaseDenies, BaseError, BaseInfo, BlockKind, Detail, EventStream, LaunchSpec, State,
    Status, Terminal, TerminalRef,
};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The protocol generation makiba speaks. A core of another generation refuses the hello.
const GENERATION: u64 = 1;

/// The tag every terminal of an organization carries (`spec::TAG_ORG`): its state folder.
const TAG_ORG: &str = crate::spec::TAG_ORG;

#[derive(Clone)]
pub struct Herdr {
    socket: PathBuf,
    timeout: Duration,
    /// The organization this base acts for. Without one, `launch` and `list` refuse.
    org: Option<String>,
}

/// `$XDG_RUNTIME_DIR/herdr-core/<session>.sock`; `/tmp` when the variable is not set, as the base does.
pub fn socket_path(runtime_dir: Option<&Path>, session: &str) -> PathBuf {
    runtime_dir
        .unwrap_or(Path::new("/tmp"))
        .join("herdr-core")
        .join(format!("{session}.sock"))
}

/// The commands, the socket folder and the folders of herdr-core (spec section 6.2).
pub fn denies(runtime_dir: Option<&Path>) -> BaseDenies {
    let socket_dir = socket_path(runtime_dir, "x")
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    BaseDenies {
        commands: ["hc", "herdr-core", "herdr-app"].map(String::from).into(),
        sockets: vec![socket_dir],
        folders: [
            "~/.config/herdr-core",
            "~/.config/herdr-app",
            "~/.local/state/herdr-core",
        ]
        .map(PathBuf::from)
        .into(),
    }
}

impl Herdr {
    pub fn new(socket: PathBuf, timeout: Duration) -> Herdr {
        Herdr {
            socket,
            timeout,
            org: None,
        }
    }

    pub fn for_organization(mut self, org: &str) -> Herdr {
        self.org = Some(org.to_string());
        self
    }

    fn org(&self) -> Result<&str, BaseError> {
        self.org.as_deref().ok_or_else(|| {
            other(
                "organization",
                "this connection has no organization; connect with connect_as",
            )
        })
    }

    fn unavailable(&self, kind: &str, what: &str, e: &dyn std::fmt::Display) -> BaseError {
        BaseError::Unavailable(Detail {
            kind: kind.into(),
            message: format!("{what} {}: {e}", self.socket.display()),
            hint: Some("is the base running? `makiba up` starts it".into()),
        })
    }

    /// One request, one reply, on a connection of its own.
    fn call(&self, method: &str, params: Value) -> Result<Value, BaseError> {
        let stream = UnixStream::connect(&self.socket)
            .map_err(|e| self.unavailable("connect", "cannot connect to", &e))?;
        for set in [
            stream.set_read_timeout(Some(self.timeout)),
            stream.set_write_timeout(Some(self.timeout)),
        ] {
            set.map_err(|e| self.unavailable("socket", "cannot set a timeout on", &e))?;
        }
        let request = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        let mut writer = &stream;
        writeln!(writer, "{request}")
            .map_err(|e| self.unavailable("write", "cannot write to", &e))?;
        let mut line = String::new();
        let n = BufReader::new(&stream)
            .read_line(&mut line)
            .map_err(|e| self.unavailable("read", "no answer from", &e))?;
        if n == 0 {
            return Err(self.unavailable("closed", "connection closed by", &"no answer"));
        }
        let reply: Value =
            serde_json::from_str(&line).map_err(|e| other("reply", &e.to_string()))?;
        if let Some(err) = reply.get("error") {
            // The base's shape: {"code", "message", "data": {"kind", "hint"}}.
            let text = |v: Option<&Value>| v.and_then(Value::as_str).map(String::from);
            let kind = text(err.pointer("/data/kind")).unwrap_or_else(|| "rpc".into());
            let variant = match kind.as_str() {
                "not_found" => BaseError::NotFound,
                "status_mismatch" => BaseError::StatusMismatch,
                "unavailable" => BaseError::Unavailable,
                "refused" | "blocked" => BaseError::Blocked,
                "finished" => BaseError::Finished,
                _ => BaseError::Other,
            };
            return Err(variant(Detail {
                kind,
                message: text(err.get("message")).unwrap_or_else(|| err.to_string()),
                hint: text(err.pointer("/data/hint")),
            }));
        }
        reply
            .get("result")
            .cloned()
            .ok_or_else(|| other("reply", "no result in the reply"))
    }
}

fn other(kind: &str, message: &str) -> BaseError {
    BaseError::Other(Detail {
        kind: kind.into(),
        message: message.into(),
        hint: None,
    })
}

fn not_built(what: &str) -> BaseError {
    other("not_built", &format!("{what} is not built yet"))
}

/// The core's version, at the top of the result or under `core`.
fn version_of(result: &Value) -> Option<&str> {
    [result.get("version"), result.pointer("/core/version")]
        .into_iter()
        .flatten()
        .find_map(Value::as_str)
}

/// Refuse a reply whose `generation` is a number other than ours. A reply without one is accepted:
/// the field name is read from the binary's strings, not yet seen on the wire (M1-1 report).
fn check_generation(result: &Value) -> Result<(), BaseError> {
    match result.get("generation").and_then(Value::as_u64) {
        Some(g) if g != GENERATION => Err(other(
            "generation",
            &format!("the base speaks protocol generation {g}; makiba speaks {GENERATION}"),
        )),
        _ => Ok(()),
    }
}

fn text_of<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

/// A terminal's address in a reply: the base's `{"address": "t7"}` (hc 0.1.21, seen on the wire),
/// or `{"id"}`, `{"terminal": {"id"}}`, `{"terminal": "t7"}`.
fn id_of(v: &Value) -> Option<&str> {
    text_of(v, "address")
        .or_else(|| text_of(v, "id"))
        .or_else(|| v.pointer("/terminal/id").and_then(Value::as_str))
        .or_else(|| text_of(v, "terminal"))
}

/// The base's terminal object. The shape is read from the binary's schema text, not yet seen on
/// the wire (scripts/check-terminals-wire.sh shows it), so the readings are generous.
fn terminal_of(v: &Value) -> Result<Terminal, BaseError> {
    let v = v.get("terminal").filter(|t| t.is_object()).unwrap_or(v);
    let id = id_of(v).ok_or_else(|| other("reply", "a terminal without an id"))?;
    let tags = v
        .get("labels")
        .and_then(Value::as_object)
        .map(|o| {
            o.iter()
                .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default();
    let status = v.get("status");
    let word = status
        .and_then(Value::as_str)
        .or_else(|| status.and_then(|s| text_of(s, "state")));
    let status = word.and_then(|w| {
        let state = match w {
            "idle" => State::Idle,
            "working" => State::Working,
            "blocked" => State::Blocked,
            "done" => State::Done,
            "error" => State::Error,
            _ => return None,
        };
        let kind = match status.and_then(|s| text_of(s, "kind")) {
            Some("permission") => Some(BlockKind::Permission),
            Some("question") => Some(BlockKind::Question),
            Some("auth") => Some(BlockKind::Auth),
            _ => None,
        };
        Some(Status {
            state,
            kind,
            msg: status.and_then(|s| text_of(s, "msg")).map(String::from),
        })
    });
    // The exit code is `exit_code`, or under `exit` in the notices of `events.subscribe`.
    let exit_code = v
        .get("exit_code")
        .or_else(|| v.pointer("/exit/code"))
        .and_then(Value::as_i64)
        .map(|c| c as i32);
    let exited = v.get("exit").is_some_and(|e| !e.is_null());
    let alive = v.get("alive").and_then(Value::as_bool).unwrap_or(
        !exited
            && exit_code.is_none()
            && !matches!(text_of(v, "state"), Some("finished" | "exited")),
    );
    Ok(Terminal {
        id: TerminalRef(id.into()),
        name: text_of(v, "name").unwrap_or(id).into(),
        tags,
        status,
        turn: v.get("turn").and_then(Value::as_u64),
        alive,
        exit_code,
    })
}

impl Base for Herdr {
    fn name(&self) -> &'static str {
        "herdr-core"
    }

    fn hello(&self) -> Result<BaseInfo, BaseError> {
        checked_hello(|| {
            let result = self.call("hello", json!({"generation": GENERATION}))?;
            check_generation(&result)?;
            let version = version_of(&result)
                .ok_or_else(|| other("hello", "the hello reply carries no version"))?;
            Ok(BaseInfo {
                name: self.name().into(),
                version: version.into(),
            })
        })
    }

    fn launch(&self, spec: &LaunchSpec) -> Result<TerminalRef, BaseError> {
        // The organization's tag is the base's own: a spec cannot omit it or name another.
        let mut labels = spec.tags.clone();
        labels.insert(TAG_ORG.into(), self.org()?.into());
        // Unset variables go through `env -u` in front of the agent's argv (spec section 6.2).
        let mut argv: Vec<&str> = Vec::new();
        if !spec.unset.is_empty() {
            argv.push("env");
            for v in &spec.unset {
                argv.extend(["-u", v]);
            }
        }
        argv.extend(spec.argv.iter().map(String::as_str));
        let result = self.call(
            "terminal.run",
            json!({
                "argv": argv,
                "cwd": spec.cwd,
                "name": spec.name,
                "labels": labels,
                "env": spec.env,
                "keep": true,
                "no_parent": true,
                "no_notify": true,
            }),
        )?;
        id_of(&result)
            .map(|id| TerminalRef(id.into()))
            .ok_or_else(|| other("reply", "terminal.run answered without a terminal id"))
    }

    fn list(&self) -> Result<Vec<Terminal>, BaseError> {
        let org = self.org()?;
        let result = self.call(
            "terminal.list",
            json!({"labels": {TAG_ORG: org}, "all": true}),
        )?;
        let items = result
            .as_array()
            .or_else(|| result.get("terminals").and_then(Value::as_array))
            .ok_or_else(|| other("reply", "terminal.list answered without a list"))?;
        // The base is asked to filter, and the answer is checked: only this organization's.
        let mut mine = Vec::new();
        for item in items {
            let t = terminal_of(item)?;
            if t.tags.get(TAG_ORG).map(String::as_str) == Some(org) {
                mine.push(t);
            }
        }
        Ok(mine)
    }

    fn get(&self, t: &TerminalRef) -> Result<Terminal, BaseError> {
        terminal_of(&self.call("terminal.get", json!({"terminal": t.0}))?)
    }

    fn read(&self, t: &TerminalRef, lines: usize) -> Result<Vec<String>, BaseError> {
        let result = self.call("terminal.read", json!({"terminal": t.0, "lines": lines}))?;
        let text = result
            .as_str()
            .or_else(|| text_of(&result, "text"))
            .ok_or_else(|| other("reply", "terminal.read answered without text"))?;
        Ok(text.lines().map(String::from).collect())
    }

    fn keys(&self, t: &TerminalRef, keys: &[&str]) -> Result<(), BaseError> {
        self.call("terminal.keys", json!({"terminal": t.0, "keys": keys}))
            .map(drop)
    }

    fn send(&self, t: &TerminalRef, text: &str, only_if_idle: bool) -> Result<(), BaseError> {
        let mut params = json!({"terminal": t.0, "text": text});
        if only_if_idle {
            params["if"] = json!("idle");
        }
        self.call("terminal.send", params).map(drop)
    }

    fn end(&self, t: &TerminalRef) -> Result<(), BaseError> {
        match self.call("terminal.kill", json!({"terminal": t.0})) {
            Ok(_) | Err(BaseError::Finished(_)) => {}
            Err(e) => return Err(e),
        }
        self.call("terminal.rm", json!({"terminal": t.0})).map(drop)
    }
    fn events(&self) -> Result<EventStream, BaseError> {
        self.events_stream()
    }
    fn start(&self) -> Result<(), BaseError> {
        Err(not_built("start"))
    }
    fn worker_denies(&self) -> BaseDenies {
        super::worker_denies()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_different_generation_is_refused() {
        assert!(check_generation(&json!({"generation": GENERATION})).is_ok());
        assert!(check_generation(&json!({})).is_ok());
        assert!(check_generation(&json!({"generation": GENERATION + 1})).is_err());
    }
}

#[cfg(test)]
mod wire_tests {
    use super::*;

    /// The real base's answers, as the owner's run of scripts/check-hello-wire.sh showed them.
    #[test]
    fn the_real_replies_are_read() {
        let ok = json!({"core": {"id": "c0", "name": "m"}, "framed": false, "generation": 1,
            "methods": ["hello"], "version": "0.1.20"});
        assert_eq!(version_of(&ok), Some("0.1.20"));
        assert!(check_generation(&ok).is_ok());
    }

    #[test]
    fn a_terminal_is_read_in_the_shapes_the_base_may_use() {
        let t = terminal_of(
            &json!({"id": "t7", "name": "w", "labels": {"makiba.org": "o"},
            "status": {"state": "blocked", "kind": "permission", "msg": "ok?"}, "turn": 3}),
        )
        .unwrap();
        assert_eq!(t.id, TerminalRef("t7".into()));
        assert_eq!(t.tags["makiba.org"], "o");
        let status = t.status.unwrap();
        assert_eq!(status.state, State::Blocked);
        assert_eq!(status.kind, Some(BlockKind::Permission));
        assert!(t.alive);
        let t = terminal_of(&json!({"terminal": {"id": "t8", "status": "idle", "exit_code": 1}}))
            .unwrap();
        assert_eq!(t.status.map(|s| s.state), Some(State::Idle));
        assert!(!t.alive && t.exit_code == Some(1));
        assert!(terminal_of(&json!({"name": "no id"})).is_err());
    }

    /// A terminal as hc 0.1.21 returned it for `terminal.kill` (owner's run of
    /// scripts/check-terminals-wire.sh): `address`, `state`, a null `status` and `exit_code`.
    #[test]
    fn a_terminal_as_the_real_base_returns_it() {
        let live = json!({"address": "t1", "name": "wire-check", "state": "live", "status": null,
            "exit_code": null, "turn": 0, "labels": {"makiba.org": "o"}});
        let t = terminal_of(&live).unwrap();
        assert_eq!(t.id, TerminalRef("t1".into()));
        assert!(t.alive && t.status.is_none() && t.tags["makiba.org"] == "o");
        let killed = json!({"address": "t1", "state": "finished", "reason": "killed",
            "exit_code": null, "status": null});
        assert!(!terminal_of(&killed).unwrap().alive);
    }
}
