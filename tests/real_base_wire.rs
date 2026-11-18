//! #81: what the real base answers on the wire, checked in CI instead of by hand
//! (scripts/check-hello-wire.sh, check-terminals-wire.sh, check-events-wire.sh stay for diagnosis).
//! Every test returns at once unless `MAKIBA_TEST_REAL_BASE=1`; then `hc` runs in the fixture's
//! place of its own and the tests speak raw JSON-RPC to its socket, so the field names are those of
//! the wire, not makiba's reading of them. The test agent supplies status and turn.

mod common;

use common::{Place, base, real_wanted};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

const AGENT: &str = env!("CARGO_BIN_EXE_makiba-fake-agent");

/// A running real base. Dropping `_fixture` stops it and removes its place.
struct Core {
    _fixture: common::Fixture,
    place: Place,
}

fn core() -> Option<Core> {
    if !real_wanted() {
        return None;
    }
    let fixture = base();
    let place = fixture.place.clone().expect("a real base has a place");
    let hc = std::env::var_os("HC_BIN").unwrap_or_else(|| "hc".into());
    let status = Command::new(hc)
        .args(["server", "start"])
        .env_remove("HC_SOCKET")
        .env_remove("HC_SCOPE")
        .env_remove("HC_TERMINAL_ID")
        .env_remove("HC_TERMINAL_SOCKET")
        .env("HC_SESSION", &place.session)
        .env("XDG_CONFIG_HOME", &place.config_home)
        .env("XDG_STATE_HOME", &place.state_home)
        .env("XDG_RUNTIME_DIR", &place.runtime_dir)
        .output()
        .expect("run hc server start")
        .status;
    assert!(status.success(), "hc server start failed");
    Some(Core {
        _fixture: fixture,
        place,
    })
}

fn socket(place: &Place) -> PathBuf {
    place
        .runtime_dir
        .join("herdr-core")
        .join(format!("{}.sock", place.session))
}

impl Core {
    /// One request on a connection of its own; the whole reply object.
    fn rpc(&self, method: &str, params: Value) -> Value {
        rpc(&socket(&self.place), method, params)
    }

    /// The `result` of a call that must succeed.
    fn ok(&self, method: &str, params: Value) -> Value {
        let reply = self.rpc(method, params);
        assert!(reply.get("error").is_none(), "{method} failed: {reply}");
        reply["result"].clone()
    }

    /// The `error.data.kind` of a call that must fail.
    fn error_kind(&self, method: &str, params: Value) -> String {
        let reply = self.rpc(method, params);
        let err = reply
            .get("error")
            .unwrap_or_else(|| panic!("{method} did not fail: {reply}"));
        err.pointer("/data/kind")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("no data.kind in {err}"))
            .to_string()
    }

    fn run(&self, name: &str, argv: &[&str]) -> String {
        let result = self.ok(
            "terminal.run",
            json!({"argv": argv, "cwd": std::env::temp_dir(), "name": name,
                "labels": {"makiba.org": "wire-check", "makiba.role": "worker"},
                "keep": true, "no_parent": true, "no_notify": true}),
        );
        result["address"]
            .as_str()
            .unwrap_or_else(|| panic!("no address in {result}"))
            .to_string()
    }

    fn run_agent(&self, name: &str, script: &[&str]) -> String {
        let mut argv = vec![AGENT, "--record", "/dev/null"];
        argv.extend(script);
        self.run(name, &argv)
    }

    fn end(&self, t: &str) {
        let _ = self.rpc("terminal.kill", json!({"terminal": t}));
        self.ok("terminal.rm", json!({"terminal": t}));
    }

    /// Poll `terminal.get` until the status is `state`.
    fn wait_status(&self, t: &str, state: &str) {
        let until = Instant::now() + Duration::from_secs(30);
        while Instant::now() < until {
            let got = self.ok("terminal.get", json!({"terminal": t}));
            if status_state(&got["status"]) == Some(state) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("{t} never reported {state}");
    }
}

fn rpc(sock: &PathBuf, method: &str, params: Value) -> Value {
    let mut s = UnixStream::connect(sock).expect("connect to the base");
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let request = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    writeln!(s, "{request}").unwrap();
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line).expect("a reply");
    serde_json::from_str(&line).unwrap_or_else(|e| panic!("{method}: {e}: {line}"))
}

/// The state word of a `status` that is a string or an object with `state`.
fn status_state(status: &Value) -> Option<&str> {
    status
        .as_str()
        .or_else(|| status.get("state").and_then(Value::as_str))
}

fn items(list: &Value) -> &Vec<Value> {
    list.as_array()
        .or_else(|| list["terminals"].as_array())
        .unwrap_or_else(|| panic!("not a list: {list}"))
}

// --- hello (scripts/check-hello-wire.sh) ---

#[test]
fn hello_answers_the_pinned_version_and_generation_and_refuses_another_generation() {
    let Some(c) = core() else { return };
    let ok = c.ok("hello", json!({"generation": 1}));
    assert_eq!(ok["version"], makiba::base::version::PINNED);
    assert_eq!(ok["generation"], 1);
    assert!(ok["core"].is_object() && ok["methods"].is_array(), "{ok}");

    let reply = c.rpc("hello", json!({"generation": 2}));
    let err = reply
        .get("error")
        .unwrap_or_else(|| panic!("generation 2 was accepted: {reply}"));
    assert!(err.to_string().contains("generation"), "{err}");
}

// --- terminal.* (scripts/check-terminals-wire.sh) ---

#[test]
fn the_terminal_methods_answer_in_the_shapes_makiba_reads() {
    let Some(c) = core() else { return };
    let t = c.run(
        "wire-check",
        &["sh", "-c", "echo makiba-wire-line; sleep 60"],
    );

    let mine = c.ok(
        "terminal.list",
        json!({"labels": {"makiba.org": "wire-check"}, "all": true}),
    );
    let found = items(&mine)
        .iter()
        .find(|x| x["address"] == t.as_str())
        .unwrap_or_else(|| panic!("{t} is not in {mine}"));
    assert_eq!(found["labels"]["makiba.org"], "wire-check");
    let others = c.ok(
        "terminal.list",
        json!({"labels": {"makiba.org": "someone-else"}}),
    );
    assert!(items(&others).is_empty(), "{others}");

    let got = c.ok("terminal.get", json!({"terminal": t}));
    assert_eq!(got["address"], t.as_str());
    assert_eq!(got["name"], "wire-check");
    assert_eq!(got["state"], "live");
    assert!(
        got["status"].is_null() && got["exit_code"].is_null(),
        "{got}"
    );
    assert_eq!(got["turn"], 0);
    assert_eq!(got["labels"]["makiba.role"], "worker");

    let until = Instant::now() + Duration::from_secs(30);
    loop {
        let read = c.ok("terminal.read", json!({"terminal": t, "lines": 20}));
        let text = read.as_str().or_else(|| read["text"].as_str());
        if text.is_some_and(|x| x.contains("makiba-wire-line")) {
            break;
        }
        assert!(Instant::now() < until, "the line was not read: {read}");
        std::thread::sleep(Duration::from_millis(200));
    }

    c.ok("terminal.keys", json!({"terminal": t, "keys": ["enter"]}));
    c.ok("terminal.send", json!({"terminal": t, "text": "echo sent"}));

    let killed = c.ok("terminal.kill", json!({"terminal": t}));
    assert_eq!(killed["address"], t.as_str());
    assert_eq!(killed["state"], "finished");
    assert_eq!(killed["reason"], "killed");
    c.ok("terminal.rm", json!({"terminal": t}));
    assert_eq!(
        c.error_kind("terminal.get", json!({"terminal": t})),
        "not_found"
    );
}

#[test]
fn a_send_to_a_terminal_in_the_wrong_status_is_refused_with_its_kind() {
    let Some(c) = core() else { return };
    let working = c.run_agent("wire-working", &["working", "hang"]);
    c.wait_status(&working, "working");
    assert_eq!(
        c.error_kind(
            "terminal.send",
            json!({"terminal": working, "text": "x", "if": "idle"})
        ),
        "status_mismatch"
    );
    c.end(&working);

    let blocked = c.run_agent("wire-blocked", &["blocked:permission", "hang"]);
    c.wait_status(&blocked, "blocked");
    assert_eq!(
        c.error_kind("terminal.send", json!({"terminal": blocked, "text": "x"})),
        "blocked"
    );
    c.end(&blocked);
}

// --- events.subscribe (scripts/check-events-wire.sh) ---

#[test]
fn events_subscribe_sends_a_snapshot_then_notices_and_heartbeats() {
    let Some(c) = core() else { return };
    let sock = socket(&c.place);
    let mut s = UnixStream::connect(&sock).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
    let request = json!({"jsonrpc": "2.0", "id": 1, "method": "events.subscribe", "params": {}});
    writeln!(s, "{request}").unwrap();
    let mut reader = BufReader::new(s);
    let mut first = String::new();
    reader.read_line(&mut first).expect("the snapshot");
    let snapshot: Value = serde_json::from_str(&first).unwrap();
    let result = &snapshot["result"];
    assert!(
        result["seq"].is_u64() && result["terminals"].is_array(),
        "{snapshot}"
    );

    // A terminal that ends by itself, and the test agent through every state.
    let driver = {
        let sock = sock.clone();
        std::thread::spawn(move || {
            let call = |method: &str, params: Value| {
                let reply = rpc(&sock, method, params);
                assert!(reply.get("error").is_none(), "{method}: {reply}");
                reply["result"].clone()
            };
            let run = |name: &str, argv: Vec<&str>| {
                let r = call(
                    "terminal.run",
                    json!({"argv": argv, "cwd": std::env::temp_dir(), "name": name,
                        "keep": true, "no_parent": true, "no_notify": true}),
                );
                r["address"].as_str().unwrap().to_string()
            };
            let plain = run("wire-plain", vec!["sh", "-c", "echo hi; sleep 3"]);
            std::thread::sleep(Duration::from_secs(6));
            // It ended by itself after 3 s: a kill may answer `finished`.
            let _ = rpc(&sock, "terminal.kill", json!({"terminal": plain}));
            call("terminal.rm", json!({"terminal": plain}));
            let agent = run(
                "wire-agent",
                vec![
                    AGENT,
                    "--record",
                    "/dev/null",
                    "idle",
                    "working",
                    "sleep:1500",
                    "blocked:permission",
                    "sleep:1500",
                    "idle",
                    "sleep:1500",
                    "working",
                    "sleep:1500",
                    "done",
                    "sleep:2000",
                    "exit",
                ],
            );
            std::thread::sleep(Duration::from_secs(10));
            let _ = rpc(&sock, "terminal.kill", json!({"terminal": agent}));
            call("terminal.rm", json!({"terminal": agent}));
        })
    };

    let mut line = String::new(); // kept across read timeouts: a line may arrive in pieces
    let mut types = std::collections::BTreeSet::new();
    let (mut heartbeat, mut removed, mut last_seq) = (false, 0, result["seq"].as_u64().unwrap());
    let (mut blocked_permission, mut status_null, mut finished_code) = (false, false, false);
    let until = Instant::now() + Duration::from_secs(45);
    while Instant::now() < until && !(heartbeat && removed >= 2) {
        match reader.read_line(&mut line) {
            Ok(0) => panic!("the base closed the subscription"),
            Ok(_) => {}
            Err(_) => continue, // the 1 s read timeout: look again
        }
        let m: Value = serde_json::from_str(&line).unwrap();
        line.clear();
        match m["method"].as_str() {
            Some("heartbeat") => heartbeat = true,
            Some("event") => {
                let p = &m["params"];
                let seq = p["seq"].as_u64().expect("a seq");
                assert!(seq > last_seq, "seq went back: {seq} after {last_seq}");
                last_seq = seq;
                assert!(p["terminal"].is_string(), "{m}");
                let kind = p["type"].as_str().expect("a type").to_string();
                let data = &p["data"];
                match kind.as_str() {
                    "created" => assert_eq!(data["state"], "live", "{m}"),
                    "status" if data["status"].is_null() && data["reported"] == true => {
                        status_null = true
                    }
                    "status" if data["status"]["state"] == "blocked" => {
                        assert_eq!(data["status"]["kind"], "permission", "{m}");
                        blocked_permission = true;
                    }
                    "finished" if data["exit_code"] == 0 => finished_code = true,
                    "removed" => removed += 1,
                    _ => {}
                }
                types.insert(kind);
            }
            _ => {}
        }
    }
    driver.join().expect("the driver ran");
    for kind in [
        "created",
        "foreground",
        "status",
        "turn",
        "finished",
        "removed",
    ] {
        assert!(types.contains(kind), "no `{kind}` notice; saw {types:?}");
    }
    assert!(heartbeat, "no heartbeat in 45 s");
    assert!(blocked_permission, "no blocked/permission status notice");
    assert!(status_null, "no status notice with a null status");
    assert!(finished_code, "no finished notice with exit_code 0");
}
