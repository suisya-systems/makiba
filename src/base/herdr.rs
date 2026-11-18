//! The real base: herdr-core over its Unix socket, one JSON-RPC 2.0 message per line.
//! Private to `base`; nothing else names it (spec section 6.1, `tests/base_seam.rs`).
//! M1-1 builds `hello()`. Each call opens its own connection, has a read and write timeout, and is
//! never retried here.

use super::version::checked_hello;
use super::{
    Base, BaseDenies, BaseError, BaseInfo, Detail, EventStream, LaunchSpec, Terminal, TerminalRef,
};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The protocol generation makiba speaks. A core of another generation refuses the hello.
const GENERATION: u64 = 1;

pub struct Herdr {
    socket: PathBuf,
    timeout: Duration,
}

/// `$XDG_RUNTIME_DIR/herdr-core/<session>.sock`; `/tmp` when the variable is not set, as the base does.
pub fn socket_path(runtime_dir: Option<&Path>, session: &str) -> PathBuf {
    runtime_dir
        .unwrap_or(Path::new("/tmp"))
        .join("herdr-core")
        .join(format!("{session}.sock"))
}

impl Herdr {
    pub fn new(socket: PathBuf, timeout: Duration) -> Herdr {
        Herdr { socket, timeout }
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
            let text = |k: &str| err.get(k).and_then(Value::as_str).map(String::from);
            return Err(BaseError::Other(Detail {
                kind: text("kind").unwrap_or_else(|| "rpc".into()),
                message: text("message").unwrap_or_else(|| err.to_string()),
                hint: text("hint"),
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

impl Base for Herdr {
    fn name(&self) -> &'static str {
        "herdr-core"
    }

    fn hello(&self) -> Result<BaseInfo, BaseError> {
        checked_hello(|| {
            let result = self.call("hello", json!({"generation": GENERATION}))?;
            let version = version_of(&result)
                .ok_or_else(|| other("hello", "the hello reply carries no version"))?;
            Ok(BaseInfo {
                name: self.name().into(),
                version: version.into(),
            })
        })
    }

    fn launch(&self, _spec: &LaunchSpec) -> Result<TerminalRef, BaseError> {
        Err(not_built("launch"))
    }
    fn list(&self) -> Result<Vec<Terminal>, BaseError> {
        Err(not_built("list"))
    }
    fn get(&self, _t: &TerminalRef) -> Result<Terminal, BaseError> {
        Err(not_built("get"))
    }
    fn read(&self, _t: &TerminalRef, _lines: usize) -> Result<Vec<String>, BaseError> {
        Err(not_built("read"))
    }
    fn keys(&self, _t: &TerminalRef, _keys: &[&str]) -> Result<(), BaseError> {
        Err(not_built("keys"))
    }
    fn send(&self, _t: &TerminalRef, _text: &str, _only_if_idle: bool) -> Result<(), BaseError> {
        Err(not_built("send"))
    }
    fn end(&self, _t: &TerminalRef) -> Result<(), BaseError> {
        Err(not_built("end"))
    }
    fn events(&self) -> Result<EventStream, BaseError> {
        Err(not_built("events"))
    }
    fn start(&self) -> Result<(), BaseError> {
        Err(not_built("start"))
    }
    fn worker_denies(&self) -> BaseDenies {
        BaseDenies::default()
    }
}
