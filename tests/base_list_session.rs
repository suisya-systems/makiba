//! Issue 420: `Base::list_session` lists every live terminal of the base's session, whatever its
//! `makiba.org` tag (docs/design/base-upgrade.md S2), while `list` still lists only this
//! organization's. On the stand-in, and on the real base against a stand-in core on a Unix socket
//! that answers `terminal.list` with a fixed list, whatever the params, and records the params.

use makiba::base::stub::Stub;
use makiba::base::{Base, BaseError, LaunchSpec, TerminalRef, connect, connect_as};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(5);
const ORG: &str = "/var/lib/makiba/org-a";

fn spec(name: &str, tags: &[(&str, &str)]) -> LaunchSpec {
    LaunchSpec {
        argv: vec!["test-agent".into()],
        cwd: std::env::temp_dir(),
        name: name.into(),
        tags: tags
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        env: BTreeMap::new(),
        unset: Vec::new(),
    }
}

fn names(list: Vec<makiba::base::Terminal>) -> Vec<String> {
    let mut names: Vec<String> = list.into_iter().map(|t| t.name).collect();
    names.sort();
    names
}

#[test]
fn the_stand_in_lists_every_live_terminal_of_the_session() {
    let stub = Stub::new().organization("/state/a");
    stub.launch(&spec(
        "mine",
        &[("makiba.org", "/state/a"), ("makiba.role", "worker")],
    ))
    .unwrap();
    stub.launch(&spec(
        "theirs",
        &[("makiba.org", "/state/b"), ("makiba.role", "worker")],
    ))
    .unwrap();
    stub.launch(&spec("yours", &[])).unwrap();
    let gone = stub
        .launch(&spec("gone", &[("makiba.org", "/state/b")]))
        .unwrap();
    stub.exit(&gone, 0);

    let session = stub.list_session().unwrap();
    assert_eq!(names(session.clone()), ["mine", "theirs", "yours"]);
    let theirs = session.iter().find(|t| t.name == "theirs").unwrap();
    assert_eq!(theirs.tags["makiba.org"], "/state/b");
    let yours = session.iter().find(|t| t.name == "yours").unwrap();
    assert!(yours.tags.is_empty());

    assert_eq!(names(stub.list().unwrap()), ["mine"]);
}

#[test]
fn the_stand_in_without_an_organization_lists_the_session_too() {
    let stub = Stub::new();
    stub.launch(&spec("a", &[("makiba.org", "/state/a")]))
        .unwrap();
    stub.launch(&spec("b", &[])).unwrap();
    assert_eq!(names(stub.list_session().unwrap()), ["a", "b"]);
}

/// A stand-in core: answers `terminal.list` with `{"terminals": items}` and records its params.
struct Core {
    _dir: tempfile::TempDir,
    socket: PathBuf,
    params: Arc<Mutex<Vec<Value>>>,
}

fn core(items: Value) -> Core {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("test.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let params = Arc::new(Mutex::new(Vec::new()));
    let seen = params.clone();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            let seen = seen.clone();
            let items = items.clone();
            thread::spawn(move || {
                let mut writer = stream.try_clone().unwrap();
                for line in BufReader::new(stream).lines() {
                    let Ok(line) = line else { return };
                    let r: Value = serde_json::from_str(&line).unwrap();
                    let reply = if r["method"] == "terminal.list" {
                        seen.lock().unwrap().push(r["params"].clone());
                        json!({"jsonrpc": "2.0", "id": r["id"],
                            "result": {"terminals": items}})
                    } else {
                        json!({"jsonrpc": "2.0", "id": r["id"],
                            "error": {"code": -32601, "message": "no such method"}})
                    };
                    writeln!(writer, "{reply}").unwrap();
                }
            });
        }
    });
    Core {
        _dir: dir,
        socket,
        params,
    }
}

fn terminal(address: &str, labels: Value, state: &str) -> Value {
    let exit_code = if state == "finished" {
        json!(0)
    } else {
        Value::Null
    };
    json!({"address": address, "name": address, "labels": labels, "state": state,
        "status": null, "turn": 0, "exit_code": exit_code})
}

fn ids(list: Vec<makiba::base::Terminal>) -> Vec<TerminalRef> {
    list.into_iter().map(|t| t.id).collect()
}

fn refs(names: &[&str]) -> Vec<TerminalRef> {
    names.iter().map(|n| TerminalRef(n.to_string())).collect()
}

#[test]
fn the_real_base_lists_the_session_without_labels() {
    let c = core(json!([
        terminal("t1", json!({"makiba.org": ORG}), "live"),
        terminal("t2", json!({"makiba.org": "/var/lib/makiba/org-b"}), "live"),
        terminal("t3", json!({}), "live"),
        terminal("t4", json!({}), "finished"),
        terminal("t5", json!({"makiba.org": ORG}), "finished"),
    ]));

    let session = connect(&c.socket, TIMEOUT).list_session().unwrap();
    assert_eq!(ids(session), refs(&["t1", "t2", "t3"]));
    let sent = c.params.lock().unwrap().last().cloned().unwrap();
    assert!(sent.get("labels").is_none(), "labels were sent: {sent}");
    assert_eq!(sent["all"], json!(true));

    let mine = connect_as(&c.socket, TIMEOUT, ORG).list().unwrap();
    assert_eq!(ids(mine), refs(&["t1", "t5"]));
    assert!(matches!(
        connect(&c.socket, TIMEOUT).list(),
        Err(BaseError::Other(_))
    ));
}

#[test]
fn a_dead_base_is_unavailable_for_list_session() {
    let dir = tempfile::tempdir().unwrap();
    let base = connect(&dir.path().join("none.sock"), TIMEOUT);
    assert!(matches!(
        base.list_session(),
        Err(BaseError::Unavailable(_))
    ));
}

#[test]
fn an_unreadable_label_refuses_the_inventory() {
    for labels in [json!({"makiba.org": 5}), json!("x")] {
        let c = core(json!([
            terminal("t1", json!({}), "live"),
            terminal("t2", labels.clone(), "live"),
        ]));
        match connect(&c.socket, TIMEOUT).list_session() {
            Err(BaseError::Other(d)) => assert_eq!(d.kind, "reply", "labels {labels}"),
            other => panic!("labels {labels}: {other:?}"),
        }
    }
}
