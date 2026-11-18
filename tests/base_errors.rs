//! M1-4: the base's errors arrive as `BaseError` variants, each with the base's own kind, message
//! and hint. Two things are held. The real base's answer, as hc 0.1.21 gives it
//! (`{"error": {"code", "message", "data": {"kind", "hint"}}}`, from a stand-in core on a Unix
//! socket), is read into the variants. And the in-process stand-in `Stub` refuses in the same
//! variants with the same kinds, so what is tested against it holds on the real base.

use makiba::base::stub::Stub;
use makiba::base::{Base, BaseError, Detail, LaunchSpec, State, TerminalRef, connect_as};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(5);
const ORG: &str = "/var/lib/makiba/org-a";

#[derive(Default)]
struct World {
    /// Terminal address -> what it is doing: `idle`, `working`, `blocked` or `finished`.
    states: BTreeMap<String, String>,
}

struct Core {
    _dir: tempfile::TempDir,
    base: Box<dyn Base>,
    _world: Arc<Mutex<World>>,
}

/// An error as the base answers it: `kind` and `hint` under `data`.
fn refusal(kind: &str, message: &str) -> Value {
    let hint = format!("hint for {kind}");
    json!({"code": -32000, "message": message, "data": {"kind": kind, "hint": hint}})
}

fn handle(w: &World, method: &str, p: &Value) -> Result<Value, Value> {
    let id = p["terminal"].as_str().unwrap_or_default();
    let Some(state) = w.states.get(id) else {
        return Err(refusal("not_found", &format!("no terminal {id}")));
    };
    match (method, state.as_str()) {
        ("terminal.send", "finished") => Err(refusal("finished", "the terminal has exited")),
        ("terminal.send", "blocked") => Err(refusal("blocked", "the terminal is blocked")),
        ("terminal.send", s) if p["if"] == "idle" && s != "idle" => {
            Err(refusal("status_mismatch", &format!("the terminal is {s}")))
        }
        ("terminal.send", _) => Ok(json!({})),
        ("terminal.get", "working") if id == "t-odd" => {
            Err(refusal("something_new", "an error the base added later"))
        }
        _ => Err(json!({"code": -32601, "message": format!("no method {method}")})),
    }
}

fn core() -> Core {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("test.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let world = Arc::new(Mutex::new(World {
        states: [
            ("t-blocked", "blocked"),
            ("t-working", "working"),
            ("t-idle", "idle"),
            ("t-finished", "finished"),
            ("t-odd", "working"),
        ]
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .into(),
    }));
    let shared = world.clone();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            let world = shared.clone();
            thread::spawn(move || {
                let mut writer = stream.try_clone().unwrap();
                for line in BufReader::new(stream).lines() {
                    let Ok(line) = line else { return };
                    let r: Value = serde_json::from_str(&line).unwrap();
                    let out = handle(
                        &world.lock().unwrap(),
                        r["method"].as_str().unwrap(),
                        &r["params"],
                    );
                    let reply = match out {
                        Ok(result) => json!({"jsonrpc": "2.0", "id": r["id"], "result": result}),
                        Err(error) => json!({"jsonrpc": "2.0", "id": r["id"], "error": error}),
                    };
                    writeln!(writer, "{reply}").unwrap();
                }
            });
        }
    });
    let base = connect_as(&socket, TIMEOUT, ORG);
    Core {
        _dir: dir,
        base,
        _world: world,
    }
}

fn term(id: &str) -> TerminalRef {
    TerminalRef(id.into())
}

fn detail(e: &BaseError) -> &Detail {
    match e {
        BaseError::NotFound(d)
        | BaseError::Blocked(d)
        | BaseError::StatusMismatch(d)
        | BaseError::Finished(d)
        | BaseError::Unavailable(d)
        | BaseError::Other(d) => d,
        BaseError::UnsupportedVersion { .. } => panic!("not a base answer: {e:?}"),
    }
}

#[test]
fn send_to_a_blocked_terminal_is_blocked() {
    let c = core();
    let e = c.base.send(&term("t-blocked"), "hi", false).unwrap_err();
    assert!(matches!(e, BaseError::Blocked(_)), "{e:?}");
    // The base's own kind, message and hint are carried, not rewritten.
    assert_eq!(
        detail(&e),
        &Detail {
            kind: "blocked".into(),
            message: "the terminal is blocked".into(),
            hint: Some("hint for blocked".into()),
        }
    );
}

#[test]
fn send_only_if_idle_to_a_working_terminal_is_a_status_mismatch() {
    let c = core();
    let e = c.base.send(&term("t-working"), "hi", true).unwrap_err();
    assert!(matches!(e, BaseError::StatusMismatch(_)), "{e:?}");
    assert_eq!(detail(&e).kind, "status_mismatch");
    assert_eq!(detail(&e).message, "the terminal is working");
    // Without the condition the same send goes through, and to an idle terminal with it.
    c.base.send(&term("t-working"), "hi", false).unwrap();
    c.base.send(&term("t-idle"), "hi", true).unwrap();
}

#[test]
fn an_unknown_terminal_is_not_found() {
    let c = core();
    let e = c.base.send(&term("t99"), "hi", false).unwrap_err();
    assert!(matches!(e, BaseError::NotFound(_)), "{e:?}");
    assert_eq!(detail(&e).message, "no terminal t99");
    assert_eq!(detail(&e).hint.as_deref(), Some("hint for not_found"));
}

#[test]
fn the_rest_of_the_kinds() {
    let c = core();
    let e = c.base.send(&term("t-finished"), "hi", false).unwrap_err();
    assert!(matches!(e, BaseError::Finished(_)), "{e:?}");
    let e = c.base.get(&term("t-odd")).unwrap_err();
    assert!(matches!(e, BaseError::Other(_)), "{e:?}");
    assert_eq!(detail(&e).kind, "something_new");
    assert_eq!(detail(&e).hint.as_deref(), Some("hint for something_new"));

    let dir = tempfile::tempdir().unwrap();
    let dead = connect_as(&dir.path().join("none.sock"), TIMEOUT, ORG);
    let e = dead.send(&term("t1"), "hi", false).unwrap_err();
    assert!(matches!(e, BaseError::Unavailable(_)), "{e:?}");
}

/// An error with no kind of its own is `Other`, and still says what it said.
#[test]
fn an_error_without_a_kind_is_other_with_its_message() {
    let c = core();
    let e = c.base.keys(&term("t-idle"), &["esc"]).unwrap_err();
    assert!(matches!(e, BaseError::Other(_)), "{e:?}");
    assert_eq!(detail(&e).message, "no method terminal.keys");
    assert_eq!(detail(&e).hint, None);
}

fn spec(name: &str) -> LaunchSpec {
    LaunchSpec {
        argv: vec!["agent".into()],
        cwd: std::env::temp_dir(),
        name: name.into(),
        tags: BTreeMap::new(),
        env: BTreeMap::new(),
        unset: vec![],
    }
}

/// The variant, and the kind the real base gives it.
fn assert_kind(e: &BaseError, variant: fn(Detail) -> BaseError, kind: &str) {
    let d = detail(e);
    assert_eq!(e, &variant(d.clone()), "{e:?}");
    assert_eq!(d.kind, kind, "{e:?}");
    assert!(!d.message.is_empty(), "{e:?}");
}

/// Real hc behaviour wins over the stand-in: it refuses a send to a blocked terminal, and its
/// kinds are the base's.
#[test]
fn the_stand_in_refuses_as_the_real_base_does() {
    let stub = Stub::new();
    let id = stub.launch(&spec("a")).unwrap();

    stub.set_state(&id, State::Blocked);
    let e = stub.send(&id, "hi", false).unwrap_err();
    assert_kind(&e, BaseError::Blocked, "blocked");

    stub.set_state(&id, State::Working);
    let e = stub.send(&id, "hi", true).unwrap_err();
    assert_kind(&e, BaseError::StatusMismatch, "status_mismatch");
    stub.send(&id, "hi", false).unwrap();

    let e = stub.get(&term("t99")).unwrap_err();
    assert_kind(&e, BaseError::NotFound, "not_found");

    stub.exit(&id, 0);
    let e = stub.send(&id, "hi", false).unwrap_err();
    assert_kind(&e, BaseError::Finished, "finished");
}
