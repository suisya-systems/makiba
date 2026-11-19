//! The person's command (spec section 12): `makiba send`, `report` and `show`. It talks to
//! `makibad` over the person's socket only, with the content of the person key file as its key;
//! with no file it sends no key. It never opens `state.db` (only `src/status_local.rs` does, for
//! `status`), so it uses no database module.
//!
//! The places on disk the person's command and `makibad` share (spec section 2) are here, so the
//! daemon takes them from this module and not the other way round.

use crate::spec::{EXIT_OK, ErrorKind};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

/// The agents' port when `$MAKIBA_PORT` is not set (spec section 2).
pub const DEFAULT_PORT: u16 = 47317;

/// A request or a response line is at most this long (spec section 3).
pub const LINE_MAX: usize = 1 << 20;

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// `MAKIBA_DOOR=none`: the test seam that leaves the socket off (spec section 2).
pub fn door_off() -> bool {
    std::env::var("MAKIBA_DOOR").as_deref() == Ok("none")
}

/// `$MAKIBA_DOOR`, default `${XDG_RUNTIME_DIR}/makiba/door.sock`; `None` when neither is set.
pub fn door_path() -> Option<PathBuf> {
    env_path("MAKIBA_DOOR")
        .or_else(|| env_path("XDG_RUNTIME_DIR").map(|d| d.join("makiba").join("door.sock")))
}

/// `$MAKIBA_SHARE_DIR`, default `${XDG_DATA_HOME:-~/.local/share}/makiba`: the role files.
/// Made absolute, since an agent reads it from its own folder.
pub fn share_dir() -> PathBuf {
    let dir = env_path("MAKIBA_SHARE_DIR").unwrap_or_else(|| {
        env_path("XDG_DATA_HOME")
            .or_else(|| env_path("HOME").map(|h| h.join(".local/share")))
            .unwrap_or_else(|| PathBuf::from("."))
            .join("makiba")
    });
    std::path::absolute(&dir).unwrap_or(dir)
}

/// `$MAKIBA_PERSON_KEY`, default `${XDG_CONFIG_HOME:-~/.config}/makiba/person.key`.
pub fn person_key_path() -> PathBuf {
    env_path("MAKIBA_PERSON_KEY").unwrap_or_else(|| {
        env_path("XDG_CONFIG_HOME")
            .or_else(|| env_path("HOME").map(|h| h.join(".config")))
            .unwrap_or_else(|| PathBuf::from("."))
            .join("makiba")
            .join("person.key")
    })
}

/// The configuration folder `makiba up` gives the base's core as its `XDG_CONFIG_HOME`:
/// `${XDG_CONFIG_HOME:-~/.config}/makiba/base`. The person's own configuration of the base is
/// never written.
pub fn base_config_home() -> PathBuf {
    env_path("XDG_CONFIG_HOME")
        .or_else(|| env_path("HOME").map(|h| h.join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("makiba")
        .join("base")
}

/// `$MAKIBA_PORT`, default `DEFAULT_PORT`; `0` lets the system choose (tests).
pub fn port() -> Result<u16, String> {
    match std::env::var("MAKIBA_PORT") {
        Ok(v) if !v.is_empty() => v
            .parse()
            .map_err(|_| format!("MAKIBA_PORT is not a port: {v:?}")),
        _ => Ok(DEFAULT_PORT),
    }
}

/// The person key, or none when the file is absent or empty.
fn person_key() -> Option<String> {
    let key = std::fs::read_to_string(person_key_path()).ok()?;
    let key = key.trim();
    (!key.is_empty()).then(|| key.to_string())
}

const USAGE: &str = "usage:\n  makiba send <to> <text> [--typed]\n  makiba send <to> --file <f> [--typed]\n  makiba report start|done|blocked --task <task_id> <text>\n  makiba show <message id>\n";

/// The request's verb and arguments, from the command line.
fn parse(args: &[String]) -> Result<(&'static str, Value), String> {
    let (verb, rest) = args.split_first().ok_or("no verb")?;
    let typed = rest.iter().any(|a| a == "--typed");
    let rest: Vec<&str> = rest
        .iter()
        .map(String::as_str)
        .filter(|a| *a != "--typed")
        .collect();
    match (verb.as_str(), rest.as_slice()) {
        ("send", [to, "--file", f]) => {
            let text = std::fs::read_to_string(f).map_err(|e| format!("cannot read {f}: {e}"))?;
            Ok(("send", json!({"to": to, "text": text, "typed": typed})))
        }
        ("send", [to, text]) => Ok(("send", json!({"to": to, "text": text, "typed": typed}))),
        ("report", [state, "--task", task, text]) if !typed => Ok((
            "report",
            json!({"state": state, "task": task, "text": text}),
        )),
        ("show", [id]) if !typed => {
            let id: i64 = id.parse().map_err(|_| format!("not a message id: {id}"))?;
            Ok(("show", json!({"id": id})))
        }
        _ => Err(format!("bad arguments for {verb}")),
    }
}

/// Send one request to the door and read its answer line.
fn ask(verb: &str, args: Value) -> Result<Value, (ErrorKind, String)> {
    let unavailable = |e: String| (ErrorKind::Unavailable, e);
    if door_off() {
        return Err(unavailable(
            "MAKIBA_DOOR=none: the door's socket is off, so there is nothing to call".into(),
        ));
    }
    let path = door_path().ok_or_else(|| {
        unavailable("neither MAKIBA_DOOR nor XDG_RUNTIME_DIR is set: no door to call".into())
    })?;
    let mut req = json!({"v": 1, "verb": verb, "args": args});
    if let Some(key) = person_key() {
        req["key"] = Value::String(key);
    }
    let mut s = UnixStream::connect(&path)
        .map_err(|e| unavailable(format!("makibad is not running ({}: {e})", path.display())))?;
    let io = |e: std::io::Error| unavailable(format!("the door: {e}"));
    s.set_read_timeout(Some(Duration::from_secs(30)))
        .map_err(io)?;
    writeln!(s, "{req}").map_err(io)?;
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line).map_err(io)?;
    serde_json::from_str(&line).map_err(|e| {
        (
            ErrorKind::Internal,
            format!("the door answered {line:?}: {e}"),
        )
    })
}

fn exit(code: i32) -> ExitCode {
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

/// `makiba send|report|show ...`.
pub fn main(args: &[String]) -> ExitCode {
    let (verb, request) = match parse(args) {
        Ok(r) => r,
        Err(e) => {
            eprint!("makiba: {e}\n{USAGE}");
            return exit(ErrorKind::Usage.exit_code());
        }
    };
    match ask(verb, request) {
        Ok(answer) if answer["ok"] == json!(true) => {
            println!("{}", answer["result"]);
            exit(EXIT_OK)
        }
        Ok(answer) => {
            let e = &answer["error"];
            let kind = e["kind"].as_str().unwrap_or("internal");
            eprintln!(
                "makiba: {kind}: {}",
                e["message"].as_str().unwrap_or_default()
            );
            if let Some(hint) = e["hint"].as_str().filter(|h| !h.is_empty()) {
                eprintln!("{hint}");
            }
            let code = ErrorKind::ALL
                .iter()
                .find(|k| k.as_str() == kind)
                .map_or(ErrorKind::Internal.exit_code(), |k| k.exit_code());
            exit(code)
        }
        Err((kind, message)) => {
            eprintln!("makiba: {}: {message}", kind.as_str());
            exit(kind.exit_code())
        }
    }
}
