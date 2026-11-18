//! M1-2: terminal operations over the base's socket. A stand-in core listens on a Unix socket in a
//! temporary folder and speaks the `terminal.*` methods one JSON-RPC message per line. Its test
//! agent prints one line, records the keys and text it is given, and starts with the environment
//! of a person's shell, so what `launch` unset can be read back from it.

use makiba::base::{Base, BaseError, LaunchSpec, State, TerminalRef, connect_as};
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
const LINE: &str = "makiba-test-agent-ready";

/// The variables the stand-in's shell has before the launch changes anything.
const SHELL_ENV: &[(&str, &str)] = &[
    ("PATH", "/usr/bin"),
    ("GH_TOKEN", "secret"),
    ("CLAUDECODE", "1"),
];

struct Term {
    id: String,
    labels: BTreeMap<String, String>,
    env: BTreeMap<String, String>,
    screen: Vec<String>,
    live: bool,
}

#[derive(Default)]
struct World {
    terms: Vec<Term>,
    next: u32,
    /// The params of every `terminal.run`.
    runs: Vec<Value>,
}

struct Core {
    _dir: tempfile::TempDir,
    socket: PathBuf,
    world: Arc<Mutex<World>>,
}

fn labels_of(v: &Value) -> BTreeMap<String, String> {
    v.as_object()
        .map(|o| {
            o.iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
                .collect()
        })
        .unwrap_or_default()
}

fn terminal_json(t: &Term) -> Value {
    json!({
        "id": t.id, "name": t.id, "labels": t.labels,
        "status": "idle", "turn": 0, "exit_code": null,
        "state": if t.live { "running" } else { "finished" },
    })
}

fn not_found(id: &str) -> Value {
    json!({"code": -32000, "message": format!("no terminal {id}"),
        "data": {"kind": "not_found", "hint": "see `hc ls --all`"}})
}

fn live<'a>(w: &'a mut World, id: &str) -> Option<&'a mut Term> {
    w.terms.iter_mut().find(|t| t.id == id && t.live)
}

/// What the stand-in does with one request: `Ok(result)` or `Err(error)`.
fn handle(world: &mut World, method: &str, p: &Value) -> Result<Value, Value> {
    let id = p["terminal"].as_str().unwrap_or_default().to_string();
    match method {
        "terminal.run" => {
            world.runs.push(p.clone());
            // The program is `env [-u VAR]... [VAR=value]... argv`, as `launch` is told to send it.
            let argv: Vec<&str> = p["argv"]
                .as_array()
                .unwrap()
                .iter()
                .map(|a| a.as_str().unwrap())
                .collect();
            let mut env: BTreeMap<String, String> = SHELL_ENV
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            env.extend(labels_of(&p["env"]));
            let mut rest = &argv[..];
            if rest.first() == Some(&"env") {
                rest = &rest[1..];
                while rest.len() > 1 && rest[0] == "-u" {
                    env.remove(rest[1]);
                    rest = &rest[2..];
                }
            }
            world.next += 1;
            let t = Term {
                id: format!("t{}", world.next),
                labels: labels_of(&p["labels"]),
                env,
                screen: vec![LINE.to_string()],
                live: true,
            };
            let out = json!({"id": t.id, "name": p["name"]});
            world.terms.push(t);
            Ok(out)
        }
        // The stand-in ignores the label filter: `list` must not rely on the core to apply it.
        "terminal.list" => Ok(
            json!({"terminals": world.terms.iter().filter(|t| t.live).map(terminal_json).collect::<Vec<_>>()}),
        ),
        "terminal.get" => match world.terms.iter().find(|t| t.id == id) {
            Some(t) => Ok(terminal_json(t)),
            None => Err(not_found(&id)),
        },
        "terminal.read" => match world.terms.iter().find(|t| t.id == id) {
            Some(t) => Ok(json!({"text": t.screen.join("\n")})),
            None => Err(not_found(&id)),
        },
        "terminal.keys" => match live(world, &id) {
            Some(t) => {
                for k in p["keys"].as_array().unwrap() {
                    t.screen.push(format!("key:{}", k.as_str().unwrap()));
                }
                Ok(json!({}))
            }
            None => Err(not_found(&id)),
        },
        "terminal.send" => match live(world, &id) {
            Some(t) => {
                t.screen
                    .push(format!("sent:{}", p["text"].as_str().unwrap()));
                Ok(json!({}))
            }
            None => Err(not_found(&id)),
        },
        "terminal.kill" => match live(world, &id) {
            Some(t) => {
                t.live = false;
                Ok(json!({}))
            }
            None => Err(not_found(&id)),
        },
        "terminal.rm" => match world.terms.iter().position(|t| t.id == id && !t.live) {
            Some(i) => {
                world.terms.remove(i);
                Ok(json!({}))
            }
            None => Err(not_found(&id)),
        },
        other => Err(json!({"code": -32601, "message": format!("no method {other}")})),
    }
}

fn core() -> Core {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("test.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let world = Arc::new(Mutex::new(World::default()));
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
                    let method = r["method"].as_str().unwrap();
                    let out = handle(&mut world.lock().unwrap(), method, &r["params"]);
                    let reply = match out {
                        Ok(result) => json!({"jsonrpc": "2.0", "id": r["id"], "result": result}),
                        Err(error) => json!({"jsonrpc": "2.0", "id": r["id"], "error": error}),
                    };
                    writeln!(writer, "{reply}").unwrap();
                }
            });
        }
    });
    Core {
        _dir: dir,
        socket,
        world,
    }
}

impl Core {
    fn base(&self) -> Box<dyn Base> {
        connect_as(&self.socket, TIMEOUT, ORG)
    }

    /// A terminal some other program launched: no makiba tag, or another organization's.
    fn add_foreign(&self, labels: &[(&str, &str)]) -> String {
        let mut w = self.world.lock().unwrap();
        w.next += 1;
        let id = format!("t{}", w.next);
        w.terms.push(Term {
            id: id.clone(),
            labels: labels
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            env: BTreeMap::new(),
            screen: vec![],
            live: true,
        });
        id
    }
}

fn spec(name: &str) -> LaunchSpec {
    LaunchSpec {
        argv: vec!["test-agent".into(), "--token=abc".into()],
        cwd: std::env::temp_dir(),
        name: name.into(),
        tags: BTreeMap::from([
            ("makiba.role".to_string(), "worker".to_string()),
            ("makiba.task_id".to_string(), "task-1".to_string()),
        ]),
        env: BTreeMap::from([("GH_CONFIG_DIR".to_string(), String::new())]),
        unset: vec!["GH_TOKEN".into(), "CLAUDECODE".into()],
    }
}

/// Launch the test agent, read the line it printed, send it a key, end it.
#[test]
fn launch_read_keys_send_and_end() {
    let c = core();
    let base = c.base();
    let t = base.launch(&spec("worker-1")).expect("launch");

    let screen = base.read(&t, 50).expect("read");
    assert!(screen.iter().any(|l| l == LINE), "{screen:?}");

    base.keys(&t, &["enter"]).expect("keys");
    base.send(&t, "hello", false).expect("send");
    let screen = base.read(&t, 50).unwrap();
    assert!(screen.iter().any(|l| l == "key:enter"), "{screen:?}");
    assert!(screen.iter().any(|l| l == "sent:hello"), "{screen:?}");

    let got = base.get(&t).expect("get");
    assert_eq!(got.id, t);
    assert!(got.alive);
    assert_eq!(got.status.map(|s| s.state), Some(State::Idle));

    base.end(&t).expect("end");
    assert!(matches!(base.get(&t), Err(BaseError::NotFound(_))));
    assert!(base.list().unwrap().is_empty());
}

/// The launch carries the tags, the environment and the unset list, and the base's own options.
#[test]
fn a_launched_terminal_carries_the_tags_and_none_of_the_unset_variables() {
    let c = core();
    let base = c.base();
    let t = base.launch(&spec("worker-2")).unwrap();

    let w = c.world.lock().unwrap();
    let term = w.terms.iter().find(|x| x.id == t.0).unwrap();
    for (k, v) in [
        ("makiba.role", "worker"),
        ("makiba.task_id", "task-1"),
        ("makiba.org", ORG),
    ] {
        assert_eq!(term.labels.get(k).map(String::as_str), Some(v), "{k}");
    }
    assert!(!term.env.contains_key("GH_TOKEN"));
    assert!(!term.env.contains_key("CLAUDECODE"));
    assert_eq!(term.env.get("GH_CONFIG_DIR").map(String::as_str), Some(""));
    assert!(term.env.contains_key("PATH"), "only the listed ones go");

    let run = &w.runs[0];
    for flag in ["keep", "no_parent", "no_notify"] {
        assert_eq!(run[flag], true, "{flag}");
    }
    assert_eq!(run["name"], "worker-2");
    // The agent's own arguments come last and unchanged.
    let argv = run["argv"].as_array().unwrap();
    assert_eq!(
        argv[argv.len() - 2..],
        [json!("test-agent"), json!("--token=abc")]
    );
}

/// The organization's tag is the base's, not the caller's: a spec cannot launch without it or
/// under another organization.
#[test]
fn launch_always_tags_the_organization() {
    let c = core();
    let base = c.base();
    let mut s = spec("worker-3");
    s.tags.insert("makiba.org".into(), "somewhere-else".into());
    let t = base.launch(&s).unwrap();
    let w = c.world.lock().unwrap();
    let term = w.terms.iter().find(|x| x.id == t.0).unwrap();
    assert_eq!(term.labels.get("makiba.org").map(String::as_str), Some(ORG));
}

/// A terminal launched by something else is not in `list`, whatever the core returns.
#[test]
fn a_terminal_launched_by_something_else_is_not_listed() {
    let c = core();
    let base = c.base();
    let mine = base.launch(&spec("worker-4")).unwrap();
    let untagged = c.add_foreign(&[]);
    let other_org = c.add_foreign(&[("makiba.org", "/var/lib/makiba/org-b")]);
    let not_makiba = c.add_foreign(&[("team", "x")]);

    let listed: Vec<TerminalRef> = base.list().unwrap().into_iter().map(|t| t.id).collect();
    assert_eq!(listed, vec![mine]);
    for id in [untagged, other_org, not_makiba] {
        assert!(!listed.contains(&TerminalRef(id)));
    }
}

#[test]
fn an_unknown_terminal_is_not_found_and_a_dead_base_is_unavailable() {
    let c = core();
    let base = c.base();
    let gone = TerminalRef("t99".into());
    assert!(matches!(base.read(&gone, 5), Err(BaseError::NotFound(_))));
    assert!(matches!(
        base.keys(&gone, &["esc"]),
        Err(BaseError::NotFound(_))
    ));
    assert!(matches!(base.end(&gone), Err(BaseError::NotFound(_))));
    let dir = tempfile::tempdir().unwrap();
    let dead = connect_as(&dir.path().join("none.sock"), TIMEOUT, ORG);
    assert!(matches!(dead.list(), Err(BaseError::Unavailable(_))));
}
