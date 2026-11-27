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

/// The label `launch` adds: a mark of the argv it sent (`argv_mark`). It is the base's own, not
/// one of the spec's tags, and `terminal_of` keeps it out of `Terminal::tags`. A kept terminal
/// whose program the base replaced with the machine's shell stays `live`, with the shell's argv:
/// a terminal whose argv no longer has this mark is not running what makibad launched (#560).
const LABEL_ARGV: &str = "makiba.argv";

/// The first 32 hex digits of the SHA-256 of the argv's JSON.
fn argv_mark<S: AsRef<str>>(argv: &[S]) -> String {
    use sha2::{Digest, Sha256};
    let argv: Vec<&str> = argv.iter().map(AsRef::as_ref).collect();
    let text = serde_json::to_string(&argv).expect("a list of strings is JSON");
    Sha256::digest(text.as_bytes())[..16]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The base's own reading of a terminal: not finished, as far as its reply says.
fn running(v: &Value) -> bool {
    let exit_code = v
        .get("exit_code")
        .or_else(|| v.pointer("/exit/code"))
        .and_then(Value::as_i64);
    let exited = v.get("exit").is_some_and(|e| !e.is_null());
    v.get("alive").and_then(Value::as_bool).unwrap_or(
        !exited
            && exit_code.is_none()
            && !matches!(text_of(v, "state"), Some("finished" | "exited")),
    )
}

/// The terminal's argv is not the one makibad launched it with: its program was replaced. A
/// terminal without the mark, or a reply without an argv of strings, is not judged.
fn replaced(v: &Value) -> bool {
    let Some(mark) = v
        .get("labels")
        .and_then(|l| l.get(LABEL_ARGV))
        .and_then(Value::as_str)
    else {
        return false;
    };
    let argv = v
        .get("argv")
        .and_then(Value::as_array)
        .and_then(|a| a.iter().map(Value::as_str).collect::<Option<Vec<&str>>>());
    argv.is_some_and(|argv| argv_mark(&argv) != mark)
}

/// The terminal object of a reply that may wrap it in `terminal`.
fn unwrapped(v: &Value) -> &Value {
    v.get("terminal").filter(|t| t.is_object()).unwrap_or(v)
}

#[derive(Clone)]
pub struct Herdr {
    socket: PathBuf,
    timeout: Duration,
    /// The organization this base acts for. Without one, `launch` and `list` refuse.
    org: Option<String>,
    /// The session `start` starts the core of (`HC_SESSION`).
    session: Option<String>,
}

/// The process that serves `socket` (Issue 382): its pid, and the file it runs when that can be
/// read. Linux only: the peer's pid by `SO_PEERCRED` on a connection that writes nothing, then
/// `/proc/<pid>/exe`. Nothing is run and nothing is sent to the core. `None` when nothing
/// listens, or on another system.
#[cfg(target_os = "linux")]
pub fn server_process(socket: &Path) -> Option<(u32, Option<PathBuf>)> {
    use std::os::fd::AsRawFd;
    let stream = UnixStream::connect(socket).ok()?;
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: the buffer is a ucred of the length given, owned by this frame.
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    if rc != 0 || cred.pid <= 0 {
        return None;
    }
    let pid = cred.pid as u32;
    Some((pid, std::fs::read_link(format!("/proc/{pid}/exe")).ok()))
}

#[cfg(not(target_os = "linux"))]
pub fn server_process(_socket: &Path) -> Option<(u32, Option<PathBuf>)> {
    None
}

/// The `hc` that `start` runs (`$HC_BIN`, else `hc` on `PATH`), as a file, when it is found. On
/// `PATH`, as the lookup that runs it: the first regular file with an execute bit.
fn hc_file() -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let hc = PathBuf::from(std::env::var_os("HC_BIN").unwrap_or_else(|| "hc".into()));
    if hc.components().count() > 1 {
        return Some(hc);
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(&hc))
        .find(|p| {
            std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

/// The text of `base::started_other_version` (Issue 382). `server` is what `server_process` read.
pub fn started_other_version(
    session: &str,
    found: &str,
    exe: &str,
    server: Option<(u32, Option<PathBuf>)>,
) -> String {
    let pinned = super::version::PINNED;
    let hc = std::env::var("HC_BIN").unwrap_or_else(|_| "hc".into());
    let named = match &server {
        Some((pid, Some(f))) => format!("its server is {} (pid {pid})", f.display()),
        Some((pid, None)) => format!("its server's file could not be read (pid {pid})"),
        None => "its server's file could not be read".into(),
    };
    let same = |a: &Path, b: &Path| match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    };
    let is_hc = match (server.and_then(|(_, f)| f), hc_file()) {
        (Some(f), Some(h)) => same(&f, &h),
        _ => false,
    };
    let head = format!("`{hc} server start` started a base of session {session} that runs {found}");
    // The way out depends on the direction, as for a core found running (Issue 418): only an older
    // server is replaced by this makiba's pin; a newer one means this makiba is the older part.
    match super::version::compare_with_pinned(found) {
        Some(std::cmp::Ordering::Less) => {
            let mut text = format!(
                "{head}, this makiba pins {pinned}; {named}. Stop it with `{exe} down --stop-base`, \
                 then install {pinned} with `bash scripts/install-base.sh PREFIX` and fix what it \
                 names until it exits 0 (it names a herdr-core or hc that HC_BIN or PATH finds \
                 before the installed one), then run `{exe} up` again"
            );
            if is_hc {
                text.push_str(
                    "; HC_BIN or PATH names that file as hc: point HC_BIN at PREFIX/bin/hc or unset it",
                );
            }
            text
        }
        Some(std::cmp::Ordering::Greater) => format!(
            "{head}, this makiba pins {pinned}; {named}. This makiba is older than that server: \
             run the makiba that pins {found} by its path"
        ),
        _ => format!("{head}, which this makiba cannot compare with {pinned}; {named}"),
    }
}

#[cfg(test)]
mod started_tests {
    use super::*;

    /// Issue 382: the way out follows the direction of the started server's version.
    #[test]
    fn the_way_out_follows_the_direction() {
        let pinned = crate::base::version::PINNED;
        let (a, b, c) = {
            let p: Vec<u64> = pinned.split('.').map(|x| x.parse().unwrap()).collect();
            (p[0], p[1], p[2])
        };
        let server = || Some((7, Some(PathBuf::from("/x/herdr-core"))));
        let newer = format!("{a}.{b}.{}", c + 1);
        let text = started_other_version("s", &newer, "/m/makiba", server());
        assert!(text.contains("older than that server"), "{text}");
        assert!(
            text.contains(&format!("pins {newer} by its path")),
            "{text}"
        );
        assert!(!text.contains("install-base.sh"), "{text}");
        assert!(text.contains("/x/herdr-core (pid 7)"), "{text}");
        let odd = format!("{pinned}-rc.1");
        let text = started_other_version("s", &odd, "/m/makiba", server());
        assert!(text.contains("cannot compare"), "{text}");
        assert!(!text.contains("install-base.sh"), "{text}");
        if c > 0 {
            let older = format!("{a}.{b}.{}", c - 1);
            let text = started_other_version("s", &older, "/m/makiba", server());
            assert!(text.contains("install-base.sh"), "{text}");
            assert!(text.contains("/m/makiba down --stop-base"), "{text}");
        }
    }
}

/// `$XDG_RUNTIME_DIR/herdr-core/<session>.sock`; `/tmp` when the variable is not set, as the base does.
pub fn socket_path(runtime_dir: Option<&Path>, session: &str) -> PathBuf {
    runtime_dir
        .unwrap_or(Path::new("/tmp"))
        .join("herdr-core")
        .join(format!("{session}.sock"))
}

/// The commands, the socket folder and the folders of herdr-core (spec section 6.2), and the
/// configuration folder `start` gives the core: at its default place in `folders`, which the
/// static role settings name, and at `config_home`, where it is resolved, with the socket folder.
pub fn denies(runtime_dir: Option<&Path>, config_home: &Path) -> BaseDenies {
    let socket_dir = socket_path(runtime_dir, "x")
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    BaseDenies {
        commands: ["hc", "herdr-core", "herdr-app"].map(String::from).into(),
        resolved: vec![socket_dir, config_home.to_path_buf()],
        folders: [
            "~/.config/herdr-core",
            "~/.config/herdr-app",
            "~/.local/state/herdr-core",
            "~/.config/makiba/base",
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
            session: None,
        }
    }

    pub fn for_session(mut self, session: &str) -> Herdr {
        self.session = Some(session.to_string());
        self
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

impl Herdr {
    /// The items of a `terminal.list` reply: the reply itself, or under `terminals`.
    fn list_items(&self, params: Value) -> Result<Vec<Value>, BaseError> {
        let result = self.call("terminal.list", params)?;
        result
            .as_array()
            .or_else(|| result.get("terminals").and_then(Value::as_array))
            .cloned()
            .ok_or_else(|| other("reply", "terminal.list answered without a list"))
    }
}

/// A terminal's labels, where `terminal_of` reads them, are absent, null or an object of strings.
fn check_labels(item: &Value) -> Result<(), BaseError> {
    let v = unwrapped(item);
    let readable = match v.get("labels") {
        None | Some(Value::Null) => true,
        Some(Value::Object(o)) => o.values().all(Value::is_string),
        Some(_) => false,
    };
    if readable {
        Ok(())
    } else {
        Err(other(
            "reply",
            "terminal.list answered a terminal whose labels cannot be read",
        ))
    }
}

fn other(kind: &str, message: &str) -> BaseError {
    BaseError::Other(Detail {
        kind: kind.into(),
        message: message.into(),
        hint: None,
    })
}

/// Make `<config_home>/herdr-core/config.toml` say `notes = false` (spec section 6.2, `start`): the
/// base sends no notices and gives agents no rules of its own. The folder is makiba's own, never
/// the person's herdr-core configuration.
fn notes_off(config_home: &Path) -> std::io::Result<()> {
    let dir = config_home.join("herdr-core");
    std::fs::create_dir_all(&dir)?;
    let file = dir.join("config.toml");
    if std::fs::read_to_string(&file).ok().as_deref() != Some("notes = false\n") {
        std::fs::write(&file, "notes = false\n")?;
    }
    Ok(())
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
    let v = unwrapped(v);
    let id = id_of(v).ok_or_else(|| other("reply", "a terminal without an id"))?;
    let tags = v
        .get("labels")
        .and_then(Value::as_object)
        .map(|o| {
            o.iter()
                .filter(|(k, _)| k.as_str() != LABEL_ARGV)
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
    // Alive: the program makibad launched runs. A replaced one does not, though the base keeps
    // its terminal live; its exit code is not known.
    let alive = running(v) && !replaced(v);
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
        labels.insert(LABEL_ARGV.into(), argv_mark(&argv));
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
        let items = self.list_items(json!({"labels": {TAG_ORG: org}, "all": true}))?;
        // The base is asked to filter, and the answer is checked: only this organization's.
        let mut mine = Vec::new();
        for item in &items {
            let t = terminal_of(item)?;
            if t.tags.get(TAG_ORG).map(String::as_str) == Some(org) {
                mine.push(t);
            }
        }
        Ok(mine)
    }

    /// `terminal.list` without labels: every terminal of the session, the ones the base runs kept
    /// (a terminal whose program was replaced runs the shell, and is kept). A
    /// terminal whose labels cannot be read refuses the whole list, so that an unreadable
    /// organization's tag is never taken for "no tag".
    fn list_session(&self) -> Result<Vec<Terminal>, BaseError> {
        let items = self.list_items(json!({"all": true}))?;
        let mut live = Vec::new();
        for item in &items {
            check_labels(item)?;
            let t = terminal_of(item)?;
            if running(unwrapped(item)) {
                live.push(t);
            }
        }
        Ok(live)
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
    /// `notes = false` in makiba's own config folder, then `$HC_BIN server start` (default `hc`) with the
    /// variables of spec section 6.4 removed from its environment, so no terminal of the core
    /// inherits them. Never stops or restarts a core.
    fn start(&self) -> Result<(), BaseError> {
        let session = self.session.as_deref().ok_or_else(|| {
            other(
                "session",
                "this connection has no session; use connect_session_as",
            )
        })?;
        let config_home = crate::cli::base_config_home();
        notes_off(&config_home)
            .map_err(|e| other("config", &format!("cannot write the base's config: {e}")))?;
        let hc = std::env::var_os("HC_BIN").unwrap_or_else(|| "hc".into());
        let mut cmd = std::process::Command::new(&hc);
        cmd.args(["server", "start"]);
        for v in crate::daemon::launch::UNSET {
            cmd.env_remove(v);
        }
        // The core reads its configuration from makiba's folder; a client of the same session
        // keeps its own settings and still reaches it.
        let out = cmd
            .env("HC_SESSION", session)
            .env("XDG_CONFIG_HOME", &config_home)
            .output()
            .map_err(|e| {
                BaseError::Unavailable(Detail {
                    kind: "spawn".into(),
                    message: format!("cannot run {}: {e}", hc.to_string_lossy()),
                    hint: Some("set HC_BIN to the hc program".into()),
                })
            })?;
        if out.status.success() {
            Ok(())
        } else {
            Err(other(
                "start",
                &format!(
                    "`hc server start` failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ),
            ))
        }
    }
    /// `server.stop`: the core and its programs stop (`makiba down --stop-base`).
    fn stop(&self) -> Result<(), BaseError> {
        self.call("server.stop", json!({})).map(drop)
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

    /// A kept terminal as hc 0.1.22 shows it before and after the base replaced its program with
    /// the machine's shell (#560): still `live`, `reason` and `exit_code` null, the same labels,
    /// and the shell's argv.
    #[test]
    fn a_kept_terminal_whose_program_was_replaced_is_not_alive() {
        let launched = ["env", "-u", "X", "agent", "--worker"];
        let mark = argv_mark(&launched);
        assert_eq!(mark.len(), 32);
        let labels = json!({"makiba.org": "o", "makiba.spawn": "s", LABEL_ARGV: mark});
        let before = json!({"address": "t1", "state": "live", "reason": null, "exit_code": null,
            "status": {"state": "working"}, "argv": launched, "labels": labels,
            "foreground": {"argv": launched, "name": "env", "pid": 10}});
        let t = terminal_of(&before).unwrap();
        assert!(t.alive && t.exit_code.is_none());
        assert!(!t.tags.contains_key(LABEL_ARGV) && t.tags["makiba.spawn"] == "s");
        let shell = json!({"address": "t1", "state": "live", "reason": null, "exit_code": null,
            "signal": null, "finished_ms": null, "status": null, "reported": false,
            "argv": ["/usr/bin/zsh"], "labels": labels,
            "foreground": {"argv": ["/usr/bin/zsh"], "name": "zsh", "pid": 11}});
        let t = terminal_of(&shell).unwrap();
        assert!(!t.alive && t.exit_code.is_none() && t.tags["makiba.org"] == "o");
        assert!(!terminal_of(&json!({"terminal": shell})).unwrap().alive);
        // Without the mark, or without an argv, nothing is judged: a terminal launched before.
        let unmarked = json!({"address": "t1", "state": "live", "argv": ["/usr/bin/zsh"],
            "labels": {"makiba.org": "o"}});
        assert!(terminal_of(&unmarked).unwrap().alive);
        let no_argv = json!({"address": "t1", "state": "live", "labels": labels});
        assert!(terminal_of(&no_argv).unwrap().alive);
        // The base still runs the shell: the session's inventory keeps it.
        assert!(running_in_base(&shell));
    }

    fn running_in_base(v: &Value) -> bool {
        running(unwrapped(v))
    }
}
